//! 代码浏览：目录列举、文件读取，以及文件相对 git HEAD 的未提交改动。
//!
//! 只处理本地文件系统语义，不碰 axum：路由层带参数进来、把 [`FileBrowserError`]
//! 映射成 HTTP 响应。浏览根目录由客户端指定（项目工作目录），所以相对路径一律先解析
//! 成绝对路径、再用 [`is_path_within`] 复核——`..`、绝对路径、以及指向根目录之外的
//! 符号链接都在打开文件之前挡掉；根目录由客户端给，这一点不能省。

use std::{
    path::{Component, Path, PathBuf},
    process::Output,
};

use astrcode_core::hostpaths::is_path_within;
use astrcode_protocol::http::{
    FileChangeStateDto, FileContentResponseDto, FileDiffResponseDto, FileEntryDto,
    FileTreeResponseDto, GitStatusAvailabilityDto, GitStatusEntryDto, GitStatusEntryStateDto,
    GitStatusResponseDto,
};
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
    let text = if truncated {
        decoded[..decoded.rfind('\n').map_or(0, |index| index + 1)].to_owned()
    } else {
        decoded.to_owned()
    };
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
        let Some(diff) = git_stdout(root, &["diff", "HEAD", "--", &target.relative]).await? else {
            response.state = FileChangeStateDto::GitUnavailable;
            return Ok(response);
        };
        if diff.trim().is_empty() {
            return Ok(response);
        }
        response.state = FileChangeStateDto::Modified;
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

    fn entry(path: &str, state: GitStatusEntryStateDto) -> GitStatusEntryDto {
        GitStatusEntryDto {
            path: path.to_owned(),
            state,
        }
    }
}
