//! 代码浏览：目录列举、文件读取、工作区全局搜索，以及文件相对 git HEAD 的未提交改动。
//!
//! 只处理本地文件系统语义，不碰 axum：路由层带参数进来、把 [`FileBrowserError`]
//! 映射成 HTTP 响应。浏览根目录由客户端指定（项目工作目录），所以相对路径一律先解析
//! 成绝对路径、再用 [`is_path_within`] 复核——`..`、绝对路径、以及指向根目录之外的
//! 符号链接都在打开文件之前挡掉；根目录由客户端给，这一点不能省。

use std::{
    path::{Component, Path, PathBuf},
    process::Output,
};

use astrcode_core::{
    hostpaths::is_path_within,
    text::{ceil_char_boundary, floor_char_boundary},
};
use astrcode_protocol::http::{
    FileChangeStateDto, FileContentResponseDto, FileDiffResponseDto, FileEntryDto,
    FileSearchFileDto, FileSearchMatchDto, FileSearchResponseDto, FileTreeResponseDto,
    GitStatusAvailabilityDto, GitStatusEntryDto, GitStatusEntryStateDto, GitStatusResponseDto,
};
use grep_matcher::Matcher as _;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks};
use ignore::WalkBuilder;
use similar::TextDiff;
use tokio::io::AsyncReadExt as _;

/// 单次返回的正文上限。超出的文件只给前面的若干行。
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// 单次返回的 diff 上限。
const MAX_DIFF_BYTES: usize = 512 * 1024;
/// 单次返回的改动条目上限；一个工作区可能躺着上万条改动，界面也放不下。
const MAX_STATUS_ENTRIES: usize = 2000;
/// 统计整文件行数时的读块大小。
const COUNT_LINES_CHUNK_BYTES: usize = 64 * 1024;
/// 单次搜索返回的命中总数上限。
const MAX_SEARCH_MATCHES: usize = 500;
/// 单次搜索返回的文件数上限。
const MAX_SEARCH_FILES: usize = 100;
/// 单次搜索扫过的文件数上限。目录很大（没被忽略的构建产物）时到此为止：宁可结果不全，
/// 也不让一次搜索跑成分钟级。
const MAX_SEARCH_SCANNED_FILES: usize = 20_000;
/// 命中行回给界面的最长字节数；超出的行以命中为中心取一段窗口。
const MAX_SEARCH_LINE_BYTES: usize = 240;
/// 窗口里命中之前保留的字节数。
const SEARCH_EXCERPT_LEAD_BYTES: usize = 60;
/// 窗口被裁掉那一侧的省略号。它占的字节要计进 `column`，界面才能按偏移量直接画高亮。
const ELLIPSIS: char = '…';

/// 代码浏览失败；分类决定路由层回哪个状态码。
#[derive(Debug, thiserror::Error)]
pub(crate) enum FileBrowserError {
    /// 路径为空、逃出根目录，或指向的对象类型不对（目录当文件读）。
    #[error("{0}")]
    InvalidPath(String),
    /// 目标不存在。
    #[error("{0}")]
    NotFound(String),
    #[error("读取 {path} 失败：{source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

/// 一次请求要访问的目标：回显与 git 命令用相对路径，文件操作用绝对路径。
struct Target {
    relative: String,
    absolute: PathBuf,
}

/// 列举 `rel` 目录下的一层条目；`rel` 为空串表示根目录。
///
/// 只给一层：浏览器按展开动作逐层取，服务端因此不必递归一个可能很大的仓库。
pub(crate) async fn list_dir(
    root: &Path,
    rel: &str,
) -> Result<FileTreeResponseDto, FileBrowserError> {
    let target = resolve(root, rel)?;
    let mut reader = tokio::fs::read_dir(&target.absolute)
        .await
        .map_err(|source| io_error(&target.absolute, source))?;

    let mut entries = Vec::new();
    loop {
        let entry = reader
            .next_entry()
            .await
            .map_err(|source| io_error(&target.absolute, source))?;
        let Some(entry) = entry else {
            break;
        };
        let name = entry.file_name();
        // 文件名的字节序列不是 UTF-8 时无法用相对路径表示，拿出来也没法再访问，
        // 因此跳过；否则回显的名字与真实路径对不上，点开只会 404。
        let Some(name) = name.to_str() else {
            tracing::debug!(?name, "跳过文件名不是 UTF-8 的目录项");
            continue;
        };
        let file_type = entry
            .file_type()
            .await
            .map_err(|source| io_error(&entry.path(), source))?;
        entries.push(FileEntryDto {
            name: name.to_owned(),
            path: join_relative(&target.relative, name),
            is_dir: file_type.is_dir(),
        });
    }

    sort_entries(&mut entries);
    Ok(FileTreeResponseDto {
        path: target.relative,
        entries,
    })
}

/// 读取一个文件的正文。
pub(crate) async fn read_file(
    root: &Path,
    rel: &str,
) -> Result<FileContentResponseDto, FileBrowserError> {
    let target = resolve(root, rel)?;
    let (bytes, truncated, size_bytes) = read_prefix(&target.absolute).await?;

    let Some(decoded) = decode_text(&bytes, truncated) else {
        return Ok(FileContentResponseDto {
            path: target.relative,
            size_bytes,
            binary: true,
            truncated: false,
            total_lines: 0,
            text: String::new(),
        });
    };
    // 截断边界多半落在某一行中间，那一行是被切开的，展示出来只会误导，丢掉。
    let text = drop_cut_line(decoded, truncated);
    let total_lines = if truncated {
        count_lines(&target.absolute).await?
    } else {
        text.lines().count()
    };

    Ok(FileContentResponseDto {
        path: target.relative,
        size_bytes,
        binary: false,
        truncated,
        total_lines,
        text,
    })
}

/// 取文件相对 git HEAD 的未提交改动。
///
/// 基线是 HEAD 而不是索引：这里要回答的是「agent 刚把这份文件改成什么样了」，用户关心的
/// 是工作区里实际躺着的内容，未暂存的改动不该被藏起来。
pub(crate) async fn file_diff(
    root: &Path,
    rel: &str,
) -> Result<FileDiffResponseDto, FileBrowserError> {
    let target = resolve(root, rel)?;
    let mut response = FileDiffResponseDto {
        path: target.relative.clone(),
        state: FileChangeStateDto::Unchanged,
        unified_diff: String::new(),
        original: String::new(),
        original_truncated: false,
        insertions: 0,
        deletions: 0,
        truncated: false,
    };

    match work_tree_state(root).await? {
        None => {
            response.state = FileChangeStateDto::GitUnavailable;
            return Ok(response);
        },
        Some(false) => {
            response.state = FileChangeStateDto::NotARepository;
            return Ok(response);
        },
        Some(true) => {},
    }

    let (bytes, truncated, _) = read_prefix(&target.absolute).await?;
    let Some(text) = decode_text(&bytes, truncated).map(str::to_owned) else {
        response.state = FileChangeStateDto::Binary;
        return Ok(response);
    };

    let Some(status) = git_stdout(root, &["status", "--porcelain", "--", &target.relative]).await?
    else {
        response.state = FileChangeStateDto::GitUnavailable;
        return Ok(response);
    };

    let unified_diff = if status.lines().any(|line| line.starts_with("??")) {
        response.state = FileChangeStateDto::Untracked;
        let header = format!("a/{}", target.relative);
        TextDiff::from_lines("", &text)
            .unified_diff()
            .context_radius(3)
            .header(&header, &header)
            .to_string()
    } else {
        // `--no-ext-diff` 与 `--no-color` 是必需的：下面要按 `+`/`-` 前缀逐行解析这份输出，而
        // 前一条关掉 `diff.external`（difftastic 这类外部 diff 工具会吐并排格式，一行 `+` 都没有，
        // 于是增删计数恒为 0），后一条关掉 `color.ui = always` 那种把 ANSI 转义塞进行首的配置。
        // 两者都是用户级 git 配置，不能让它们决定这台机器上界面能不能报出改动行数。
        let Some(diff) = git_stdout(
            root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-color",
                "HEAD",
                "--",
                &target.relative,
            ],
        )
        .await?
        else {
            response.state = FileChangeStateDto::GitUnavailable;
            return Ok(response);
        };
        if diff.trim().is_empty() {
            return Ok(response);
        }
        response.state = FileChangeStateDto::Modified;
        match head_text(root, &target.relative).await? {
            HeadText::Text { text, truncated } => {
                response.original = text;
                response.original_truncated = truncated;
            },
            HeadText::Binary => {
                response.state = FileChangeStateDto::Binary;
                return Ok(response);
            },
            // 工作区的 diff 取到了、HEAD 侧却读不出来，说明仓库本身有问题：按既有的
            // 「拿不到改动」呈现，好过让界面拿一份靠不住的 HEAD 正文去对齐。
            HeadText::Unavailable => {
                response.state = FileChangeStateDto::GitUnavailable;
                return Ok(response);
            },
        }
        diff
    };

    let (unified_diff, truncated) = bounded_diff(unified_diff);
    let (insertions, deletions) = count_changes(&unified_diff);
    response.unified_diff = unified_diff;
    response.insertions = insertions;
    response.deletions = deletions;
    response.truncated = truncated;
    Ok(response)
}

/// 取整个工作区相对 git HEAD 的未提交改动清单。
///
/// 基线同样是 HEAD（理由见 [`file_diff`]）：清单要回答的是「agent 动过哪些文件」，与逐文件
/// 的 diff 用同一套基线才对得上。条目按 git 自己的输出顺序给，「被改的」聚在前面、「新增的」
/// 聚在后面。
pub(crate) async fn worktree_status(root: &Path) -> Result<GitStatusResponseDto, FileBrowserError> {
    match work_tree_state(root).await? {
        None => return Ok(unavailable(GitStatusAvailabilityDto::GitUnavailable)),
        Some(false) => return Ok(unavailable(GitStatusAvailabilityDto::NotARepository)),
        Some(true) => {},
    }

    // `--untracked-files=all` 而不是默认的 `normal`：默认会把整个新目录压成一条 `?? dir/`，
    // 而清单要回答的正是「哪些文件是新增的」。忽略规则仍由 git 自己算，不在这里重判。
    let Some(stdout) = git_stdout_bytes(
        root,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )
    .await?
    else {
        return Ok(unavailable(GitStatusAvailabilityDto::GitUnavailable));
    };

    let (entries, truncated) = parse_porcelain(&stdout);
    Ok(GitStatusResponseDto {
        availability: GitStatusAvailabilityDto::Available,
        entries,
        truncated,
    })
}

/// 在工作区里按字面量搜 `query`，按文件分组返回命中行。
///
/// 遍历与忽略规则交给 `ignore`：认 `.gitignore` / `.ignore`、不看隐藏项，与 ripgrep 的默认
/// 口径一致。逐文件的扫描用 ripgrep 的 `grep-searcher`，行匹配用 `grep-regex` 的匹配器——
/// 这两件就是 ripgrep 本体用的实现，不必自己写扫描与匹配。匹配按字面量走（`fixed_strings`），
/// 查询串里的正则元字符一律当普通字符。
///
/// 要读遍整棵树，整段放进阻塞线程池：压在异步线程上会把其他请求一起拖住。
pub(crate) async fn search(
    root: &Path,
    query: &str,
    case_sensitive: bool,
) -> Result<FileSearchResponseDto, FileBrowserError> {
    let root = root.to_owned();
    let query = query.to_owned();
    let label = root.display().to_string();
    tokio::task::spawn_blocking(move || search_workspace(&root, &query, case_sensitive))
        .await
        .map_err(|error| FileBrowserError::Io {
            path: label,
            source: std::io::Error::other(format!("搜索任务失败：{error}")),
        })?
}

/// [`search`] 的同步实现。
fn search_workspace(
    root: &Path,
    query: &str,
    case_sensitive: bool,
) -> Result<FileSearchResponseDto, FileBrowserError> {
    // 空查询串在每一行的每个位置都算命中；界面清空搜索框时不该发这一次请求，但跨了一次 HTTP
    // 的值不能假定，这里按「没有命中」处理。
    if query.is_empty() {
        return Ok(FileSearchResponseDto {
            files: Vec::new(),
            truncated: false,
        });
    }

    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        // 字面量匹配：查询串里的元字符一律当普通字符，而不是让它们变成正则。
        .fixed_strings(true)
        // 声明按行匹配：匹配器于是永不越过行终结符，searcher 才能走按行的快路径。
        .line_terminator(Some(b'\n'))
        .build(query)
        .map_err(|error| FileBrowserError::InvalidPath(format!("搜索词无法匹配：{error}")))?;
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\0'))
        .build();

    let mut files: Vec<FileSearchFileDto> = Vec::new();
    let mut total_matches = 0usize;
    let mut scanned_files = 0usize;
    let mut truncated = false;

    let walker = WalkBuilder::new(root)
        // 忽略规则与隐藏项按 ripgrep 的默认口径；`require_git(false)` 让不是仓库的目录也认
        // `.gitignore`——浏览根目录常常只是一个普通目录。
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        // 同一目录内按名字排：结果是稳定的，界面不会因为文件系统的返回顺序而跳来跳去，
        // 「到上限就收手」时截断的是同一批。
        .sort_by_file_name(|left, right| left.cmp(right))
        .build();

    for entry in walker {
        // 单个目录项读不了（权限、刚好被删掉）不该让整次搜索失败：跳过去继续。
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        scanned_files += 1;
        if scanned_files > MAX_SEARCH_SCANNED_FILES {
            truncated = true;
            break;
        }
        let Some(path) = entry
            .path()
            .strip_prefix(root)
            .ok()
            .and_then(relative_label)
        else {
            // 路径不是合法 UTF-8（与 [`list_dir`] 同口径）：界面上表示不出来，也点不开。
            continue;
        };

        let mut matches: Vec<FileSearchMatchDto> = Vec::new();
        let searched = searcher.search_path(
            &matcher,
            entry.path(),
            sinks::Lossy(|line, text| {
                let Some(hit) = matcher.find(text.as_bytes()).ok().flatten() else {
                    // 行是在匹配之后才被处理的，这里再找一次找不到只可能是行被裁剪过；跳过。
                    return Ok(true);
                };
                let (text, column) = match_excerpt(text, hit.start(), hit.end());
                matches.push(FileSearchMatchDto {
                    line: line as usize,
                    column,
                    text,
                });
                total_matches += 1;
                Ok(total_matches < MAX_SEARCH_MATCHES)
            }),
        );
        // 读不开的文件（权限、特殊文件）与目录项读不了同一口径：跳过，不算搜索失败。
        if searched.is_err() || matches.is_empty() {
            continue;
        }
        files.push(FileSearchFileDto { path, matches });
        if total_matches >= MAX_SEARCH_MATCHES || files.len() >= MAX_SEARCH_FILES {
            truncated = true;
            break;
        }
    }

    Ok(FileSearchResponseDto { files, truncated })
}

/// 遍历拿到的路径转成 DTO 用的相对路径：分隔符统一成 `/`，不是合法 UTF-8 时给 `None`。
///
/// 归一化口径与 [`resolve`] 一致，因此搜索给的路径一定拿得回去打开。
fn relative_label(path: &Path) -> Option<String> {
    Some(path.to_str()?.replace('\\', "/"))
}

/// 把命中所在行裁成一段可显示文本，并给出命中在这段文本里的字节偏移。
///
/// 整行太长（压缩过的 JS、单行日志）时以命中为中心取一段窗口：被裁掉的那一侧补一个省略号，
/// 返回值里的 `column` 就相对裁好之后的文本算，界面拿它与查询串长度直接画高亮。窗口一定
/// 完整包含命中——查询串本身比窗口还长时，窗口跟着它长。
fn match_excerpt(line: &str, start: usize, end: usize) -> (String, usize) {
    let line = without_line_terminator(line);
    if line.len() <= MAX_SEARCH_LINE_BYTES {
        return (line.to_owned(), start);
    }

    let window_start = floor_char_boundary(line, start.saturating_sub(SEARCH_EXCERPT_LEAD_BYTES));
    let window_end = ceil_char_boundary(line, (window_start + MAX_SEARCH_LINE_BYTES).max(end));
    let mut text = line[window_start..window_end].to_owned();
    let mut column = start - window_start;
    if window_start > 0 {
        text.insert(0, ELLIPSIS);
        column += ELLIPSIS.len_utf8();
    }
    if window_end < line.len() {
        text.push(ELLIPSIS);
    }
    (text, column)
}

/// 行文本去掉行终结符：`\n` 与 CRLF 里的 `\r` 都不属于行内容。
fn without_line_terminator(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// 清单取不到时的响应：没有条目，原因由 `availability` 表达。
fn unavailable(availability: GitStatusAvailabilityDto) -> GitStatusResponseDto {
    GitStatusResponseDto {
        availability,
        entries: Vec::new(),
        truncated: false,
    }
}

/// 解析 `git status --porcelain -z` 的输出。
///
/// 每条记录是 `XY <路径>`，以 NUL 结尾；重命名/复制的记录紧跟一个 NUL 结尾的原路径字段
/// （`-z` 下原路径排在新路径之后，与短格式 `old -> new` 的顺序相反）。原路径不带上：清单按
/// 新路径定位文件，原路径在界面上没有可点的动作。
fn parse_porcelain(bytes: &[u8]) -> (Vec<GitStatusEntryDto>, bool) {
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut fields = bytes.split(|byte| *byte == 0);
    while let Some(field) = fields.next() {
        // 输出以 NUL 结尾，末尾会多出一个空字段；长度不足的字段不是 porcelain 的记录。
        if field.len() < 4 || field[2] != b' ' {
            continue;
        }
        let (code, path) = field.split_at(3);
        let (x, y) = (code[0], code[1]);
        // 重命名/复制多一个原路径字段：不管要不要它都得吃掉，否则会被当成下一条记录。
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            fields.next();
        }
        // 路径不是合法 UTF-8 的条目跳过（与 [`list_dir`] 同口径）：`/api/files/*` 只认 UTF-8
        // 相对路径，报出来的名字也开不了。
        let Ok(path) = std::str::from_utf8(path) else {
            tracing::debug!("跳过路径不是 UTF-8 的改动条目");
            continue;
        };
        let Some(state) = entry_state(x, y) else {
            tracing::debug!(code = %String::from_utf8_lossy(code), "跳过归不了类的 git 状态");
            continue;
        };
        if entries.len() == MAX_STATUS_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(GitStatusEntryDto {
            path: path.to_owned(),
            state,
        });
    }
    (entries, truncated)
}

/// 把 porcelain 的两个状态字符归成一个可展示的状态；归不了的返回 `None`。
///
/// 基线是 HEAD，因此索引态（X）与工作区态（Y）合起来看：更具体的那一侧说了什么就算什么。
fn entry_state(x: u8, y: u8) -> Option<GitStatusEntryStateDto> {
    let either = |code: u8| x == code || y == code;
    match (x, y) {
        (b'?', b'?') => Some(GitStatusEntryStateDto::Untracked),
        // 两侧都加、两侧都删，以及任一为 `U`（`UU`/`AU`/`DU`…）都是未解决的冲突。
        (b'A', b'A') | (b'D', b'D') => Some(GitStatusEntryStateDto::Conflicted),
        _ if either(b'U') => Some(GitStatusEntryStateDto::Conflicted),
        _ if either(b'R') => Some(GitStatusEntryStateDto::Renamed),
        _ if either(b'D') => Some(GitStatusEntryStateDto::Deleted),
        // 复制也算新增：那个路径相对 HEAD 确实是新出现的。
        _ if either(b'A') || either(b'C') => Some(GitStatusEntryStateDto::Added),
        _ if either(b'M') || either(b'T') => Some(GitStatusEntryStateDto::Modified),
        _ => None,
    }
}

/// 根目录是否在 git 工作树里；`None` 表示没有可用的 `git` 命令。
async fn work_tree_state(root: &Path) -> Result<Option<bool>, FileBrowserError> {
    match run_git(root, &["rev-parse", "--is-inside-work-tree"]).await {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(FileBrowserError::Io {
            path: root.display().to_string(),
            source,
        }),
        Ok(output) => Ok(Some(is_work_tree(&output))),
    }
}

/// 把客户端给的相对路径解析成根目录下的绝对路径。
///
/// 空串指根目录本身。纯字符串上的检查（前导斜杠、`..`、非普通路径段）只是为了让错误
/// 更直白；真正的边界是 [`is_path_within`]——它按真实路径判断，因此符号链接绕不过去。
fn resolve(root: &Path, rel: &str) -> Result<Target, FileBrowserError> {
    let normalized = rel.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(FileBrowserError::InvalidPath(format!(
            "相对路径不能是绝对路径：{rel}"
        )));
    }

    let mut absolute = root.to_path_buf();
    let mut segments = Vec::new();
    for segment in normalized.split('/') {
        match segment {
            "" | "." => {},
            ".." => {
                return Err(FileBrowserError::InvalidPath(format!(
                    "相对路径不能包含 `..`：{rel}"
                )));
            },
            segment => {
                if !is_plain_segment(segment) {
                    return Err(FileBrowserError::InvalidPath(format!(
                        "相对路径含非法片段：{rel}"
                    )));
                }
                segments.push(segment);
                absolute.push(segment);
            },
        }
    }

    if !is_path_within(&absolute, root) {
        return Err(FileBrowserError::InvalidPath(format!(
            "路径越出工作目录：{rel}"
        )));
    }

    Ok(Target {
        relative: segments.join("/"),
        absolute,
    })
}

/// 一段路径是否是单个普通名字（不含分隔符、盘符、前缀）。
fn is_plain_segment(segment: &str) -> bool {
    let mut components = Path::new(segment).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

/// 相对路径的拼接：根目录下的一层就是它的名字，其余接在父路径后面。
fn join_relative(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

/// 目录在前，同类按名字排（大小写不敏感，同名不同大小写时按原样定序）。
///
/// 排序放在服务端：`read_dir` 的顺序由文件系统决定，两个宿主不该各自再排一遍，
/// 也不该在顺序上互不相同。
fn sort_entries(entries: &mut [FileEntryDto]) {
    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
}

/// 读取文件开头的一段字节：`(bytes, 是否截断, 文件总字节数)`。
async fn read_prefix(path: &Path) -> Result<(Vec<u8>, bool, u64), FileBrowserError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|source| io_error(path, source))?;
    if metadata.is_dir() {
        return Err(FileBrowserError::InvalidPath(format!(
            "{} 是一个目录",
            path.display()
        )));
    }
    let size_bytes = metadata.len();
    let truncated = size_bytes > MAX_TEXT_BYTES as u64;
    let bytes = if truncated {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|source| io_error(path, source))?;
        let mut buffer = Vec::with_capacity(MAX_TEXT_BYTES);
        file.take(MAX_TEXT_BYTES as u64)
            .read_to_end(&mut buffer)
            .await
            .map_err(|source| io_error(path, source))?;
        buffer
    } else {
        tokio::fs::read(path)
            .await
            .map_err(|source| io_error(path, source))?
    };
    Ok((bytes, truncated, size_bytes))
}

/// 把字节解码成可展示的文本；不是 UTF-8 文本时返回 `None`。
///
/// 截断的前缀可能切在多字节字符中间，因此末尾那段残缺字节按截断吸收掉：只有从头就
/// 解不通的字节序列才算二进制。NUL 在 UTF-8 里合法，但它不属于代码文本，单独判一次。
fn decode_text(bytes: &[u8], truncated: bool) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(error) if truncated && error.valid_up_to() > 0 => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
        },
        Err(_) => None,
    }
}

/// 流式统计整个文件的行数，不把文件读进内存；口径与 `str::lines` 一致。
async fn count_lines(path: &Path) -> Result<usize, FileBrowserError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|source| io_error(path, source))?;
    let mut buffer = vec![0u8; COUNT_LINES_CHUNK_BYTES];
    let mut newlines = 0usize;
    let mut last_byte = None;
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|source| io_error(path, source))?;
        if read == 0 {
            break;
        }
        newlines += buffer[..read].iter().filter(|byte| **byte == b'\n').count();
        last_byte = Some(buffer[read - 1]);
    }
    Ok(match last_byte {
        None => 0,
        Some(b'\n') => newlines,
        Some(_) => newlines + 1,
    })
}

/// 统计统一 diff 里的增删行数。
///
/// 判法必须与界面的行分类一致（`tool_view::diff_line_kind`）：文件头先判、再看首字符。
/// 这样数字与界面上着色的行数对得上，比 git 的 `--numstat` 更贴合用户实际看到的东西。
fn count_changes(unified_diff: &str) -> (usize, usize) {
    let mut insertions = 0;
    let mut deletions = 0;
    for line in unified_diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            insertions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }
    (insertions, deletions)
}

/// 丢弃被截断切开的尾行。
///
/// 正文与 HEAD 侧原文共用这一口径：只要有一侧留下被切开的尾行，双栏就会在截断边界处
/// 凭空多出一行替换。
fn drop_cut_line(text: &str, truncated: bool) -> String {
    if truncated {
        text[..text.rfind('\n').map_or(0, |index| index + 1)].to_owned()
    } else {
        text.to_owned()
    }
}

/// HEAD 侧正文的取用结果。
///
/// 「HEAD 里没有这个路径」与「HEAD 里是空文件」对界面等价（两侧都全是新增），因此不单列。
enum HeadText {
    /// 取到了正文；截断以 [`MAX_TEXT_BYTES`] 为准，口径与 [`read_prefix`] 一致。
    Text { text: String, truncated: bool },
    /// HEAD 侧不是 UTF-8 文本，无法按行比较。
    Binary,
    /// git 命令失败。
    Unavailable,
}

/// 取 `rel` 在 HEAD 里的正文。
///
/// 先判存在性：`ls-tree` 在路径缺失时仍以 0 退出，能把「HEAD 里没有这个路径」与
/// 「git 真的失败了」分开；`git show` 失败则是真故障，升级成 [`HeadText::Unavailable`]。
async fn head_text(root: &Path, rel: &str) -> Result<HeadText, FileBrowserError> {
    let Some(listed) = git_stdout(root, &["ls-tree", "--name-only", "HEAD", "--", rel]).await?
    else {
        return Ok(HeadText::Unavailable);
    };
    if listed.trim().is_empty() {
        return Ok(HeadText::Text {
            text: String::new(),
            truncated: false,
        });
    }

    let Some(bytes) = git_stdout_bytes(root, &["show", &format!("HEAD:{rel}")]).await? else {
        return Ok(HeadText::Unavailable);
    };
    let (bytes, truncated) = bounded_bytes(bytes);
    let Some(text) = decode_text(&bytes, truncated) else {
        return Ok(HeadText::Binary);
    };
    Ok(HeadText::Text {
        text: drop_cut_line(text, truncated),
        truncated,
    })
}

/// 把 git 输出的字节压到 [`MAX_TEXT_BYTES`]，返回 `(字节前缀, 是否截断)`。
///
/// 上限必须与 [`read_prefix`] 相同：两侧切在同一个字节数上，被切开的尾行才能按同一条
/// 规则丢掉。切在多字节字符中间由 [`decode_text`] 的截断分支吸收。
fn bounded_bytes(mut bytes: Vec<u8>) -> (Vec<u8>, bool) {
    if bytes.len() <= MAX_TEXT_BYTES {
        return (bytes, false);
    }
    bytes.truncate(MAX_TEXT_BYTES);
    (bytes, true)
}

/// 按字节上限截断 diff，与 host workspace 的 diff 上限同一套做法。
fn bounded_diff(mut diff: String) -> (String, bool) {
    if diff.len() <= MAX_DIFF_BYTES {
        return (diff, false);
    }

    const SUFFIX: &str = "\n... (diff truncated)\n";
    let mut prefix_bytes = MAX_DIFF_BYTES - SUFFIX.len();
    while !diff.is_char_boundary(prefix_bytes) {
        prefix_bytes -= 1;
    }
    diff.truncate(prefix_bytes);
    diff.push_str(SUFFIX);
    (diff, true)
}

/// 在根目录下跑一条 git 子命令。
async fn run_git(root: &Path, args: &[&str]) -> std::io::Result<Output> {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
}

/// 跑一条 git 子命令并取 stdout 的原始字节；命令存在但执行失败时返回 `None`，由调用点决定
/// 回退到哪种状态——对界面来说「拿不到改动」是可呈现的结果，不必升级成 500。
///
/// 取字节而不是文本：状态清单用 `-z` 分隔，路径未必是合法 UTF-8，先做一次 lossy 转换就分不出
/// 「原文如此」和「被替换过的」了。
async fn git_stdout_bytes(root: &Path, args: &[&str]) -> Result<Option<Vec<u8>>, FileBrowserError> {
    let output = run_git(root, args)
        .await
        .map_err(|source| FileBrowserError::Io {
            path: root.display().to_string(),
            source,
        })?;
    if !output.status.success() {
        tracing::warn!(
            command = ?args,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "git 命令执行失败"
        );
        return Ok(None);
    }
    Ok(Some(output.stdout))
}

/// 跑一条 git 子命令并取 stdout 文本；只按行看的输出（diff 正文）用它。
async fn git_stdout(root: &Path, args: &[&str]) -> Result<Option<String>, FileBrowserError> {
    Ok(git_stdout_bytes(root, args)
        .await?
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}

/// `git rev-parse --is-inside-work-tree` 是否确认了工作树。
fn is_work_tree(output: &Output) -> bool {
    output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true"
}

fn io_error(path: &Path, source: std::io::Error) -> FileBrowserError {
    if source.kind() == std::io::ErrorKind::NotFound {
        FileBrowserError::NotFound(format!("找不到 {}", path.display()))
    } else {
        FileBrowserError::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// 一个可以直接提交的临时仓库；提交身份写进仓库配置，免去每次 `-c`。
    fn git_repo() -> tempfile::TempDir {
        let repo = root();
        git(repo.path(), &["init"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "user.name", "Test User"]);
        repo
    }

    /// 在临时仓库里跑一条 git 子命令；测试只用可预判成功的命令，失败即断言。
    fn git(root: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} 失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// HEAD 里没有这个路径（已暂存的新文件）时 HEAD 侧按空正文处理，而不是把整份正文
    /// 当成新增：状态不是 `??`，因此走的是 `git diff HEAD` 那条路径。
    #[tokio::test]
    async fn a_path_absent_from_head_reads_as_empty_head_text() {
        let repo = git_repo();
        fs::write(repo.path().join("committed.txt"), "old\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "initial"]);
        fs::write(repo.path().join("fresh.txt"), "one\ntwo\n").unwrap();
        git(repo.path(), &["add", "fresh.txt"]);

        let diff = file_diff(repo.path(), "fresh.txt").await.unwrap();
        assert_eq!(diff.state, FileChangeStateDto::Modified);
        assert!(!diff.unified_diff.is_empty());
        assert_eq!(diff.original, "");
        assert!(!diff.original_truncated);
    }

    /// 改过的文件要带回 HEAD 侧的原文，界面才能逐行对齐。
    #[tokio::test]
    async fn a_modified_file_carries_the_head_side_text() {
        let repo = git_repo();
        fs::write(repo.path().join("a.txt"), "one\ntwo\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "initial"]);
        fs::write(repo.path().join("a.txt"), "one\nTWO\n").unwrap();

        let diff = file_diff(repo.path(), "a.txt").await.unwrap();
        assert_eq!(diff.state, FileChangeStateDto::Modified);
        assert_eq!(diff.original, "one\ntwo\n");
        assert!(!diff.original_truncated);
        assert_eq!(
            (diff.insertions, diff.deletions),
            (1, 1),
            "改一行算一次增一次删：diff={:?}",
            diff.unified_diff
        );
    }

    /// HEAD 侧超上限时只能留下完整的行：尾行被切开会让双栏在截断边界处多出一行替换。
    #[tokio::test]
    async fn a_truncated_head_side_drops_the_cut_line() {
        let repo = git_repo();
        let line = "let value = compute_something(argument, another_argument);\n";
        let head_text = line.repeat(45_000);
        assert!(head_text.len() > MAX_TEXT_BYTES, "夹具得真的超上限");
        fs::write(repo.path().join("big.rs"), &head_text).unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "initial"]);
        // 工作区改一处，diff 才有正文；HEAD 侧仍是提交时那一份。
        fs::write(
            repo.path().join("big.rs"),
            head_text.replacen("value", "other", 1),
        )
        .unwrap();

        let diff = file_diff(repo.path(), "big.rs").await.unwrap();
        assert!(diff.original_truncated, "HEAD 侧应当被截断");
        assert_eq!(diff.original, complete_line_prefix(&head_text));
    }

    /// 与 [`head_text`] 同一口径的期望值：按行累加，加不下就停。
    fn complete_line_prefix(text: &str) -> String {
        let mut prefix = String::new();
        for line in text.lines() {
            if prefix.len() + line.len() + 1 > MAX_TEXT_BYTES {
                break;
            }
            prefix.push_str(line);
            prefix.push('\n');
        }
        prefix
    }

    /// HEAD 侧不是 UTF-8 时按二进制处理：拿替换字符去对齐只会造出假改动。
    #[tokio::test]
    async fn a_binary_head_side_reads_as_binary() {
        let repo = git_repo();
        fs::write(repo.path().join("data.txt"), b"\xff\xfe binary\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "initial"]);
        // 只让 HEAD 那一侧解不开：工作区这一侧换成合法 UTF-8。
        fs::write(repo.path().join("data.txt"), "plain text\n").unwrap();

        let diff = file_diff(repo.path(), "data.txt").await.unwrap();
        assert_eq!(diff.state, FileChangeStateDto::Binary);
        assert_eq!(diff.original, "");
    }

    #[test]
    fn resolve_rejects_escapes_and_accepts_nested_paths() {
        let root = root();
        let base = root.path();

        assert_eq!(resolve(base, "").unwrap().relative, "");
        assert_eq!(resolve(base, "src").unwrap().relative, "src");
        assert_eq!(
            resolve(base, "./src/main.rs").unwrap().relative,
            "src/main.rs"
        );
        assert_eq!(resolve(base, "src//lib.rs").unwrap().relative, "src/lib.rs");

        for escape in ["..", "../etc/passwd", "src/../../etc", "/etc/passwd"] {
            assert!(
                matches!(resolve(base, escape), Err(FileBrowserError::InvalidPath(_))),
                "{escape} 应该被拒绝"
            );
        }
    }

    /// 根目录之外的符号链接不能靠字符串检查漏过去。
    #[cfg(unix)]
    #[test]
    fn resolve_rejects_symlink_pointing_outside_root() {
        let workspace = root();
        let outside = root();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("link")).unwrap();

        assert!(matches!(
            resolve(workspace.path(), "link/secret.txt"),
            Err(FileBrowserError::InvalidPath(_))
        ));
    }

    #[test]
    fn entries_are_sorted_with_directories_first() {
        let mut entries = vec![
            FileEntryDto {
                name: "zeta.rs".into(),
                path: "zeta.rs".into(),
                is_dir: false,
            },
            FileEntryDto {
                name: "Beta".into(),
                path: "Beta".into(),
                is_dir: true,
            },
            FileEntryDto {
                name: "alpha.rs".into(),
                path: "alpha.rs".into(),
                is_dir: false,
            },
            FileEntryDto {
                name: "Alpha".into(),
                path: "Alpha".into(),
                is_dir: true,
            },
        ];
        sort_entries(&mut entries);

        let order: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(order, ["Alpha", "Beta", "alpha.rs", "zeta.rs"]);
    }

    #[test]
    fn decode_text_separates_binary_from_truncated_multibyte() {
        assert_eq!(
            decode_text("fn main() {}".as_bytes(), false),
            Some("fn main() {}")
        );
        // NUL 是合法 UTF-8，但它说明这不是代码文本。
        assert_eq!(decode_text(b"a\0b", false), None);
        // 从头就解不通：二进制。
        assert_eq!(decode_text(&[0xff, 0xfe, 0xfd], true), None);
        // 截断切在多字节字符中间：吸收残缺字节，保留前面的正文。
        let mut cut = "你好".as_bytes().to_vec();
        cut.truncate(4);
        assert_eq!(decode_text(&cut, true), Some("你"));
        assert_eq!(decode_text(&cut, false), None);
    }

    #[test]
    fn count_changes_ignores_file_headers() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1,2 @@\n-old\n+new\n+added\n context\n";
        assert_eq!(count_changes(diff), (2, 1));
    }

    #[test]
    fn bounded_diff_keeps_utf8_boundaries() {
        let diff = "中".repeat(MAX_DIFF_BYTES);
        let (bounded, truncated) = bounded_diff(diff);
        assert!(truncated);
        assert!(bounded.len() <= MAX_DIFF_BYTES);
        assert!(bounded.ends_with("(diff truncated)\n"));
    }

    /// porcelain 的几种记录形状：普通改动、未跟踪、重命名（多带一个原路径字段）、删除。
    ///
    /// 重命名那条正是哨兵：原路径字段没被吃掉的话，下一条记录会串位。
    #[test]
    fn parse_porcelain_reads_every_record_shape() {
        let raw = b"M  src/lib.rs\0?? new.txt\0R  src/new.rs\0src/old.rs\0D  gone.rs\0";
        let (entries, truncated) = parse_porcelain(raw);
        assert!(!truncated);
        assert_eq!(
            entries,
            [
                entry("src/lib.rs", GitStatusEntryStateDto::Modified),
                entry("new.txt", GitStatusEntryStateDto::Untracked),
                entry("src/new.rs", GitStatusEntryStateDto::Renamed),
                entry("gone.rs", GitStatusEntryStateDto::Deleted),
            ]
        );
    }

    /// 归不了类的状态与非法 UTF-8 的路径都跳过，不能让整份清单因此取不出来。
    #[test]
    fn parse_porcelain_skips_unusable_records() {
        let mut raw = b"!! ignored.txt\0M  ok.rs\0".to_vec();
        raw.extend_from_slice(b"M  \xff\xfe.rs\0");
        let (entries, truncated) = parse_porcelain(&raw);
        assert!(!truncated);
        assert_eq!(entries, [entry("ok.rs", GitStatusEntryStateDto::Modified)]);
    }

    /// 到达上限才报截断：界面上那句话只有真丢了条目时才该出现。
    #[test]
    fn parse_porcelain_stops_at_the_cap() {
        let exact = "M  a.rs\0".repeat(MAX_STATUS_ENTRIES).into_bytes();
        let (entries, truncated) = parse_porcelain(&exact);
        assert_eq!(entries.len(), MAX_STATUS_ENTRIES);
        assert!(!truncated, "刚好到上限不该报截断");

        let mut over = exact;
        over.extend_from_slice(b"M  b.rs\0");
        let (entries, truncated) = parse_porcelain(&over);
        assert_eq!(entries.len(), MAX_STATUS_ENTRIES);
        assert!(truncated);
    }

    /// 归类看的是 porcelain 的两个状态字符：冲突与重命名优先于普通的改动。
    #[test]
    fn entry_state_reads_both_columns() {
        assert_eq!(
            entry_state(b'?', b'?'),
            Some(GitStatusEntryStateDto::Untracked)
        );
        assert_eq!(
            entry_state(b' ', b'M'),
            Some(GitStatusEntryStateDto::Modified)
        );
        assert_eq!(
            entry_state(b'M', b' '),
            Some(GitStatusEntryStateDto::Modified)
        );
        assert_eq!(entry_state(b'A', b' '), Some(GitStatusEntryStateDto::Added));
        assert_eq!(
            entry_state(b' ', b'D'),
            Some(GitStatusEntryStateDto::Deleted)
        );
        assert_eq!(
            entry_state(b'R', b' '),
            Some(GitStatusEntryStateDto::Renamed)
        );
        for (x, y) in [(b'U', b'U'), (b'A', b'A'), (b'D', b'D'), (b'U', b'D')] {
            assert_eq!(
                entry_state(x, y),
                Some(GitStatusEntryStateDto::Conflicted),
                "{x:?}{y:?} 是未解决的冲突"
            );
        }
        // 忽略项（`!!`）不属于未提交的改动。
        assert_eq!(entry_state(b'!', b'!'), None);
    }

    /// 命中按文件分组、行号从 1 起，且 `column` 指向返回文本里的命中——界面靠它画高亮。
    /// 查询串里的正则元字符按字面量算：`a.b(c)` 不该匹配到别的形状。
    #[tokio::test]
    async fn matches_are_literal_grouped_by_file_and_point_at_the_hit() {
        let dir = root();
        fs::write(
            dir.path().join("alpha.rs"),
            "let hit = a.b(c);\nlet other = aXbYc;\n",
        )
        .unwrap();
        fs::write(dir.path().join("beta.rs"), "// 另一个文件也有 a.b(c)\n").unwrap();

        let response = search(dir.path(), "a.b(c)", false).await.unwrap();
        let paths: Vec<&str> = response
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(paths, ["alpha.rs", "beta.rs"]);

        let hit = &response.files[0].matches[0];
        assert_eq!(hit.line, 1);
        assert_eq!(&hit.text[hit.column..hit.column + "a.b(c)".len()], "a.b(c)");
        assert!(
            response.files[0].matches.len() == 1,
            "元字符不当字面量就会多出命中"
        );
        assert!(!response.truncated);
    }

    /// 默认不区分大小写，开关打开后区分。
    #[tokio::test]
    async fn case_insensitive_by_default_and_case_sensitive_on_demand() {
        let dir = root();
        fs::write(dir.path().join("alpha.rs"), "let Value = 1;\n").unwrap();

        assert_eq!(
            search(dir.path(), "value", false)
                .await
                .unwrap()
                .files
                .len(),
            1
        );
        assert!(
            search(dir.path(), "value", true)
                .await
                .unwrap()
                .files
                .is_empty()
        );
        assert_eq!(
            search(dir.path(), "Value", true).await.unwrap().files.len(),
            1
        );
    }

    /// 被 `.gitignore` 忽略的文件不进搜索结果：遍历口径与 ripgrep 一致。
    #[tokio::test]
    async fn ignored_files_are_not_searched() {
        let dir = root();
        fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
        fs::write(dir.path().join("kept.txt"), "needle\n").unwrap();

        let response = search(dir.path(), "needle", false).await.unwrap();
        let paths: Vec<&str> = response
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(paths, ["kept.txt"]);
    }

    /// 超长行只回命中附近的一段，`column` 跟着这段走：整行回给界面既没用也放不下。
    #[tokio::test]
    async fn a_long_line_is_trimmed_around_the_hit() {
        let dir = root();
        let line = format!("{}needle{}\n", "x".repeat(5_000), "y".repeat(5_000));
        fs::write(dir.path().join("long.txt"), line).unwrap();

        let response = search(dir.path(), "needle", false).await.unwrap();
        let hit = &response.files[0].matches[0];
        assert_eq!(&hit.text[hit.column..hit.column + "needle".len()], "needle");
        assert!(hit.text.starts_with(ELLIPSIS) && hit.text.ends_with(ELLIPSIS));
        assert!(hit.text.len() <= MAX_SEARCH_LINE_BYTES + 2 * ELLIPSIS.len_utf8());
    }

    /// 命中数到上限就收手，并把「还有没返回的」这一事实告诉界面。
    #[tokio::test]
    async fn hitting_the_match_cap_reports_truncation() {
        let dir = root();
        fs::write(
            dir.path().join("many.txt"),
            "needle\n".repeat(MAX_SEARCH_MATCHES + 10),
        )
        .unwrap();

        let response = search(dir.path(), "needle", false).await.unwrap();
        assert!(response.truncated);
        let total: usize = response.files.iter().map(|file| file.matches.len()).sum();
        assert_eq!(total, MAX_SEARCH_MATCHES);
    }

    /// 空查询串不该被当成「处处命中」——那等于把整棵树回一遍。
    #[tokio::test]
    async fn an_empty_query_matches_nothing() {
        let dir = root();
        fs::write(dir.path().join("alpha.rs"), "anything\n").unwrap();

        let response = search(dir.path(), "", false).await.unwrap();
        assert!(response.files.is_empty());
        assert!(!response.truncated);
    }

    fn entry(path: &str, state: GitStatusEntryStateDto) -> GitStatusEntryDto {
        GitStatusEntryDto {
            path: path.to_owned(),
            state,
        }
    }
}
