//! 工具卡的呈现推导，对应 Web 前端的 `Chat/tools/*`。
//!
//! 双层注册表照搬前端（ADR 0001 第 8 轮）：**先按工具名匹配内置渲染，名字层没有匹配时
//! 再看结果声明的呈现 intent**（`ToolResult.metadata["presentation"]`，键见
//! [`PRESENTATION_METADATA_KEY`]）。顺序与前端一致——前端的名字渲染器 priority 100、
//! intent 渲染器 50，取首个匹配即名字层优先，intent 层是给名字没有内置渲染的工具
//! （例如 MCP 工具）用的扩展口。协议零变更：intent 值就是 [`ToolPresentation`] 的
//! snake_case 线缆字符串，未知值按未声明处理（向前兼容）。
//!
//! 这里只做推导：形态、摘要行、元信息行、正文来源、diff 行分类。全部不碰 gpui，
//! 因此可以脱离窗口与 GPU 测试；渲染在 `views::chat`。

use std::fmt::Write as _;

use astrcode_core::tool::{PRESENTATION_METADATA_KEY, ToolPresentation};
use astrcode_protocol::http::ConversationBlockDto;
use serde_json::{Map, Value};

use crate::{ask_user, todo_list};

/// 摘要行最多占的字符数，超过就截断——它是一行摘要，不是正文。
const SUMMARY_MAX_CHARS: usize = 160;

/// 工具卡选定的渲染形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolView {
    /// 文件变更：diff 或文件正文。`diff` intent 与 `write`/`edit` 都落在这里。
    File,
    /// 终端/命令输出。
    Terminal,
    /// 搜索结果。
    Search,
    /// 文件读取，正文带行号。
    Read,
    /// 读回工具结果的产物（artifact），正文同样带行号。
    ToolResult,
    /// 补丁应用。
    Patch,
    /// 未匹配任何内置形态时的通用形态。
    Generic,
}

/// 正文的呈现方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewKind {
    /// 逐行显示，不区分行的语义。
    Plain,
    /// 统一 diff：增删行按语义着色。
    Diff,
    /// 每一行带一个行号前缀。
    Numbered,
}

/// 详情面板的正文来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailBody<'a> {
    /// 没有可显示的内容。
    Empty,
    /// 单块正文。
    Text(&'a str, PreviewKind),
    /// 一次替换的替换前/替换后文本。
    Replacement { old: &'a str, new: &'a str },
}

/// 按名字层与 intent 层选择渲染形态。
pub(crate) fn tool_view(block: &ConversationBlockDto) -> ToolView {
    let ConversationBlockDto::ToolCall { name, metadata, .. } = block else {
        return ToolView::Generic;
    };
    if let Some(view) = named_view(name) {
        return view;
    }
    match declared_intent(metadata.as_ref()) {
        Some(ToolPresentation::Terminal) => ToolView::Terminal,
        Some(ToolPresentation::Diff) => ToolView::File,
        Some(ToolPresentation::Search) => ToolView::Search,
        Some(ToolPresentation::Read) => ToolView::Read,
        Some(ToolPresentation::Generic) | None => ToolView::Generic,
    }
}

/// 名字回退层的匹配表，与前端 `tools/builtinRenderers` 的 `match` 一致。
fn named_view(name: &str) -> Option<ToolView> {
    match name {
        "read" => Some(ToolView::Read),
        "read_tool_result" => Some(ToolView::ToolResult),
        "grep" | "find" => Some(ToolView::Search),
        "write" | "edit" => Some(ToolView::File),
        "shell" | "shell_poll" => Some(ToolView::Terminal),
        "patch" => Some(ToolView::Patch),
        _ => None,
    }
}

/// 读取结果声明的呈现 intent；未声明或值无法识别时返回 `None`。
fn declared_intent(metadata: Option<&Value>) -> Option<ToolPresentation> {
    let value = metadata?.get(PRESENTATION_METADATA_KEY)?;
    serde_json::from_value(value.clone()).ok()
}

/// 折叠摘要行：名字层渲染器各给一行人类可读的摘要，没有匹配时返回 `None`。
pub(crate) fn summary_line(block: &ConversationBlockDto) -> Option<String> {
    let ConversationBlockDto::ToolCall {
        name,
        arguments_json,
        metadata,
        text,
        ..
    } = block
    else {
        return None;
    };
    let args = record(arguments_json.as_ref());
    let meta = record(metadata.as_ref());
    let line = match name.as_str() {
        "read" => {
            let lines = match (
                number_at(meta, &["shownLines"]),
                number_at(meta, &["totalLines"]),
            ) {
                (Some(shown), Some(total)) => format!("{shown}/{total} lines"),
                (Some(shown), None) => format!("{shown} lines"),
                _ => String::new(),
            };
            join(&[
                "read".into(),
                truncate_middle(path_for(args, meta), 96),
                lines,
                pagination_label(meta),
            ])
        },
        "read_tool_result" => {
            let artifact =
                string_at(meta, &["artifactId"]).or_str(string_at(args, &["artifactId"]));
            let size = match (
                number_at(meta, &["returnedBytes"]),
                number_at(meta, &["bytes"]),
            ) {
                (Some(returned), Some(total)) => {
                    format!("{}/{}", format_bytes(returned), format_bytes(total))
                },
                (Some(returned), None) => format_bytes(returned),
                _ => String::new(),
            };
            join(&[
                "read tool result".into(),
                truncate_middle(artifact, 40),
                size,
                pagination_label(meta),
            ])
        },
        "grep" => {
            let pattern = string_at(meta, &["pattern"]).or_str(string_at(args, &["pattern"]));
            join(&[
                "grep".into(),
                if pattern.is_empty() {
                    String::new()
                } else {
                    format!("\"{}\"", truncate_middle(pattern, 60))
                },
                number_at(meta, &["returned"])
                    .map_or(String::new(), |count| format!("{count} results")),
                string_at(meta, &["outputMode"]).to_owned(),
                pagination_label(meta),
            ])
        },
        "find" => {
            let pattern = string_at(meta, &["pattern"]).or_str(string_at(args, &["pattern"]));
            let count = number_at(meta, &["count"]).or_else(|| number_at(meta, &["returned"]));
            let total = number_at(meta, &["totalMatches"]);
            let files = match (count, total) {
                (Some(count), Some(total)) => format!("{count}/{total} files"),
                (Some(count), None) => format!("{count} files"),
                _ => String::new(),
            };
            join(&[
                "find".into(),
                truncate_middle(pattern, 64),
                files,
                pagination_label(meta),
            ])
        },
        "write" => join(&[
            match bool_at(meta, &["created"]) {
                Some(true) => "create".to_owned(),
                _ => "write".to_owned(),
            },
            truncate_middle(path_for(args, meta), 96),
            changes_label(meta),
        ]),
        "edit" => {
            let count = match (
                number_at(meta, &["replacements"]),
                number_at(meta, &["operationCount"]),
            ) {
                (Some(replacements), _) => format!("{replacements} replacements"),
                (None, Some(operations)) => format!("{operations} edits"),
                _ => String::new(),
            };
            join(&[
                "edit".into(),
                truncate_middle(path_for(args, meta), 96),
                count,
                changes_label(meta),
            ])
        },
        "shell" | "shell_poll" => {
            let command = string_at(meta, &["command"]).or_str(string_at(args, &["command"]));
            if !command.is_empty() {
                format!("$ {}", compact_preview_line(command, 140))
            } else {
                let shell_id = string_at(meta, &["shellId"]).or_str(string_at(args, &["shellId"]));
                if shell_id.is_empty() {
                    return None;
                }
                format!("poll {}", compact_preview_line(shell_id, 140))
            }
        },
        "patch" => {
            let applied = number_at(meta, &["filesApplied", "filesChanged"]).unwrap_or(0.0);
            let failed = number_at(meta, &["filesFailed"]).unwrap_or(0.0);
            join(&[
                "patch".into(),
                format!("{applied} applied"),
                if failed > 0.0 {
                    format!("{failed} failed")
                } else {
                    String::new()
                },
            ])
        },
        // 问卷的摘要在参数里，与结果文本无关。
        "askUser" => return ask_user::summary(block),
        // 计划列表的摘要在结果与参数里，结果文本只是「更新了几项」。
        "todoWrite" => return todo_list::summary(block),
        // 名字层没有渲染器时，退回结果文本；参数原文由摘要行自己在上面那段之外兜底。
        _ => {
            if text.trim().is_empty() {
                return None;
            }
            return Some(compact_preview_line(text, SUMMARY_MAX_CHARS));
        },
    };
    let line = compact_preview_line(&line, SUMMARY_MAX_CHARS);
    (!line.is_empty()).then_some(line)
}

/// 详情面板的元信息行 `(标签, 值)`，对应前端的 `MetaGrid`；空值行不返回。
pub(crate) fn meta_rows(block: &ConversationBlockDto) -> Vec<(&'static str, String)> {
    let ConversationBlockDto::ToolCall {
        name,
        arguments_json,
        metadata,
        ..
    } = block
    else {
        return Vec::new();
    };
    let args = record(arguments_json.as_ref());
    let meta = record(metadata.as_ref());
    let mut rows: Vec<(&'static str, String)> = Vec::new();

    match tool_view(block) {
        ToolView::File => {
            push(&mut rows, "path", string_at(meta, &["path"]).to_owned());
            let action = if name == "write" {
                match bool_at(meta, &["created"]) {
                    Some(true) => "created",
                    Some(false) => "overwritten",
                    None => "",
                }
            } else {
                "edited"
            };
            push(&mut rows, "action", action.to_owned());
            push(&mut rows, "size", changes_label(meta));
            if name == "edit" {
                let ops = [
                    number_at(meta, &["operationCount"]).map(|count| format!("{count} op")),
                    number_at(meta, &["replacements"]).map(|count| format!("{count} repl")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" / ");
                push(&mut rows, "ops", ops);
            }
            let lines = match string_at(args, &["content"]) {
                content if !content.is_empty() => count_lines(content).to_string(),
                _ => match (
                    number_at(meta, &["insertions"]),
                    number_at(meta, &["deletions"]),
                ) {
                    (Some(insertions), Some(deletions)) => format!("+{insertions} / -{deletions}"),
                    (Some(insertions), None) => format!("+{insertions}"),
                    _ => String::new(),
                },
            };
            push(&mut rows, "lines", lines);
            push(
                &mut rows,
                "bytes",
                byte_change_label(
                    number_at(meta, &["oldBytes"]),
                    number_at(meta, &["newBytes"]),
                ),
            );
        },
        ToolView::Terminal => {
            push(&mut rows, "cwd", string_at(meta, &["cwd"]).to_owned());
            push(&mut rows, "shell", string_at(meta, &["shell"]).to_owned());
            push(
                &mut rows,
                "shellId",
                string_at(meta, &["shellId"]).to_owned(),
            );
            let exit = if bool_at(meta, &["timedOut"]) == Some(true) {
                "timed out".to_owned()
            } else {
                number_at(meta, &["exitCode"]).map_or(String::new(), |code| code.to_string())
            };
            push(&mut rows, "exit", exit);
            push(
                &mut rows,
                "timeout",
                number_at(meta, &["timeoutSecs"]).map_or(String::new(), |secs| format!("{secs}s")),
            );
            let output = [
                number_at(meta, &["stdoutBytes"]).map(format_bytes),
                number_at(meta, &["stderrBytes"])
                    .map(|bytes| format!("stderr {}", format_bytes(bytes))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" / ");
            push(&mut rows, "output", output);
            push(&mut rows, "intent", string_at(meta, &["intent"]).to_owned());
            let stdin = string_at(args, &["stdin"]);
            push(
                &mut rows,
                "stdin",
                if stdin.is_empty() {
                    String::new()
                } else {
                    format!("{} piped", format_bytes(stdin.len() as f64))
                },
            );
        },
        ToolView::Search => {
            let is_find = name == "find";
            push(
                &mut rows,
                "pattern",
                string_at(meta, &["pattern"])
                    .or_str(string_at(args, &["pattern"]))
                    .to_owned(),
            );
            let scope = string_at(meta, &["path", "root"])
                .or_str(string_at(args, &["path", "root"]))
                .to_owned();
            push(&mut rows, "scope", scope);
            let mode = if is_find {
                "files".to_owned()
            } else {
                string_at(meta, &["outputMode"]).to_owned()
            };
            push(&mut rows, "mode", mode);
            let returned = match (
                number_at(meta, &["returned", "count"]),
                number_at(meta, &["totalMatches"]),
            ) {
                (Some(returned), Some(total)) if is_find => format!("{returned}/{total}"),
                (returned, _) => returned.map_or(String::new(), |count| count.to_string()),
            };
            push(&mut rows, "returned", returned);
            push(&mut rows, "glob", string_at(args, &["glob"]).to_owned());
            push(&mut rows, "type", string_at(args, &["fileType"]).to_owned());
            push(
                &mut rows,
                "skipped",
                number_at(meta, &["skippedFiles"]).map_or(String::new(), |count| count.to_string()),
            );
            push(&mut rows, "next", pagination_label(meta));
        },
        ToolView::Read => {
            push(&mut rows, "path", path_for(args, meta).to_owned());
            let lines = match (
                number_at(meta, &["shownLines"]),
                number_at(meta, &["totalLines"]),
            ) {
                (Some(shown), Some(total)) => format!("{shown}/{total}"),
                (shown, _) => shown.map_or(String::new(), |shown| shown.to_string()),
            };
            push(&mut rows, "lines", lines);
            push(
                &mut rows,
                "offset",
                number_at(meta, &["offset"]).map_or(String::new(), |value| value.to_string()),
            );
            push(
                &mut rows,
                "chars",
                number_at(meta, &["returnedChars"])
                    .map_or(String::new(), |value| value.to_string()),
            );
            push(
                &mut rows,
                "charOffset",
                number_at(meta, &["charOffset"]).map_or(String::new(), |value| value.to_string()),
            );
            push(&mut rows, "next", pagination_label(meta));
        },
        ToolView::ToolResult => {
            push(
                &mut rows,
                "artifact",
                string_at(meta, &["artifactId"])
                    .or_str(string_at(args, &["artifactId"]))
                    .to_owned(),
            );
            push(
                &mut rows,
                "size",
                number_at(meta, &["bytes"]).map_or(String::new(), format_bytes),
            );
            push(
                &mut rows,
                "returned",
                number_at(meta, &["returnedBytes"]).map_or(String::new(), format_bytes),
            );
            push(
                &mut rows,
                "byteOffset",
                number_at(meta, &["byteOffset"]).map_or(String::new(), |value| value.to_string()),
            );
            push(&mut rows, "next", pagination_label(meta));
        },
        ToolView::Patch => {
            push(
                &mut rows,
                "applied",
                number_at(meta, &["filesApplied", "filesChanged"])
                    .map_or(String::new(), |value| value.to_string()),
            );
            push(
                &mut rows,
                "failed",
                number_at(meta, &["filesFailed"]).map_or(String::new(), |value| value.to_string()),
            );
            let files = meta
                .and_then(|meta| meta.get("files"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .filter(|count| *count > 0);
            push(
                &mut rows,
                "files",
                files.map_or(String::new(), |count| count.to_string()),
            );
        },
        ToolView::Generic => {},
    }
    rows
}

/// `patch` 结果里一个文件的落盘情况，对应前端 `PatchToolDetails` 的文件段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchFileEntry {
    /// 行首标签：没应用上时是 `failed`，否则是 `changeType`（缺省 `changed`）。
    pub(crate) label: String,
    pub(crate) applied: bool,
    pub(crate) path: String,
    pub(crate) error: String,
}

/// `patch` 结果里逐文件的应用情况；`metadata.files` 不是数组时为空。
pub(crate) fn patch_files(block: &ConversationBlockDto) -> Vec<PatchFileEntry> {
    let ConversationBlockDto::ToolCall { metadata, .. } = block else {
        return Vec::new();
    };
    metadata
        .as_ref()
        .and_then(|meta| meta.get("files"))
        .and_then(Value::as_array)
        .map(|files| files.iter().filter_map(patch_file).collect())
        .unwrap_or_default()
}

/// `applied` 缺席按「应用了」算——与前端 `fileApplied === false` 才判失败同口径。
fn patch_file(raw: &Value) -> Option<PatchFileEntry> {
    let fields = raw.as_object()?;
    let applied = bool_at(Some(fields), &["applied"]).unwrap_or(true);
    let change_type = string_at(Some(fields), &["changeType", "change_type"]);
    Some(PatchFileEntry {
        label: if !applied {
            "failed".to_owned()
        } else if change_type.is_empty() {
            "changed".to_owned()
        } else {
            change_type.to_owned()
        },
        applied,
        path: string_at(Some(fields), &["path"]).to_owned(),
        error: string_at(Some(fields), &["error"]).to_owned(),
    })
}

/// 详情面板的正文来源。
pub(crate) fn detail_body(block: &ConversationBlockDto) -> DetailBody<'_> {
    let ConversationBlockDto::ToolCall {
        name,
        arguments_json,
        metadata,
        text,
        ..
    } = block
    else {
        return DetailBody::Empty;
    };
    let args = record(arguments_json.as_ref());
    let meta = record(metadata.as_ref());

    match tool_view(block) {
        ToolView::File => {
            // 结果里的 diff 是唯一带增删信息的正文，优先它。
            let diff = string_at(meta, &["diff"]);
            if !diff.is_empty() {
                return DetailBody::Text(diff, PreviewKind::Diff);
            }
            let content = string_at(args, &["content"]);
            if !content.is_empty() {
                return DetailBody::Text(content, PreviewKind::Plain);
            }
            // 编辑尚未产生结果时，参数里已有替换前后文本。
            let old = string_at(args, &["oldText", "old_string"]);
            let new = string_at(args, &["newText", "new_string"]);
            if !old.is_empty() || !new.is_empty() {
                return DetailBody::Replacement { old, new };
            }
        },
        ToolView::Read | ToolView::ToolResult => {
            let body = if text.is_empty() {
                "(no content)"
            } else {
                text
            };
            return DetailBody::Text(body, PreviewKind::Numbered);
        },
        ToolView::Terminal => {
            let body = if !text.is_empty() {
                text
            } else if name == "shell_poll" {
                "(no output)"
            } else {
                ""
            };
            return if body.is_empty() {
                DetailBody::Empty
            } else {
                DetailBody::Text(body, PreviewKind::Plain)
            };
        },
        ToolView::Patch => {
            // 补丁本体在参数里，结果文本只是「应用了几处」。
            let patch = string_at(args, &["patch"]);
            if !patch.is_empty() {
                return DetailBody::Text(patch, PreviewKind::Diff);
            }
        },
        ToolView::Search | ToolView::Generic => {},
    }

    let body = if text.is_empty() {
        match tool_view(block) {
            ToolView::Search => "(no results)",
            ToolView::Patch => "(no patch preview)",
            _ => "",
        }
    } else {
        text
    };
    if body.is_empty() {
        DetailBody::Empty
    } else {
        DetailBody::Text(body, PreviewKind::Plain)
    }
}

/// 正文预览的截断：`(预览文本, 被省略的字符数)`。省略数为 0 表示正文是完整的。
///
/// 与前端 `previewText` 同口径：先按字符数封顶，再按行数封顶，取两者中更早到的位置。
pub(crate) fn truncate_preview(text: &str, max_chars: usize, max_lines: usize) -> (&str, usize) {
    let mut end = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(index, _)| index);
    let mut lines = 1;
    for (index, _) in text[..end].match_indices('\n') {
        lines += 1;
        if lines > max_lines {
            end = index;
            break;
        }
    }
    (&text[..end], text.len() - end)
}

/// diff 一行的语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Addition,
    Deletion,
    FileHeader,
    Hunk,
    Context,
}

/// 与前端 `DiffCodeLines` 一致的行分类：文件头先判，再判增删，最后是 hunk。
pub(crate) fn diff_line_kind(line: &str) -> DiffLineKind {
    let header = line.starts_with("+++") || line.starts_with("---");
    match () {
        _ if header => DiffLineKind::FileHeader,
        _ if line.starts_with('+') => DiffLineKind::Addition,
        _ if line.starts_with('-') => DiffLineKind::Deletion,
        _ if line.starts_with("@@") => DiffLineKind::Hunk,
        _ => DiffLineKind::Context,
    }
}

/// `read` 输出里的一行：`(行号, 正文)`；这一行没有 `行号\t` 前缀时返回 `None`。
pub(crate) fn numbered_line(line: &str) -> Option<(&str, &str)> {
    let digits = line.len() - line.trim_start().len();
    let rest = &line[digits..];
    let (number, code) = rest.split_once('\t')?;
    (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some((number, code))
}

/// 工具耗时（秒）：毫秒字段优先，其次秒字段。
pub(crate) fn duration_seconds(metadata: Option<&Value>) -> Option<f64> {
    let meta = record(metadata);
    if let Some(ms) = number_at(meta, &["durationMs", "duration_ms"]) {
        return Some(ms / 1000.0);
    }
    number_at(meta, &["duration", "durationSeconds"])
}

/// 摘要行的状态后缀：完成的调用显示耗时，其余显示状态文字。
pub(crate) fn duration_label(metadata: Option<&Value>) -> String {
    duration_seconds(metadata)
        .map(format_duration)
        .unwrap_or_default()
}


fn path_for<'a>(
    args: Option<&'a Map<String, Value>>,
    meta: Option<&'a Map<String, Value>>,
) -> &'a str {
    string_at(meta, &["path"]).or_str(string_at(args, &["path"]))
}

/// 活动标签的类别，决定回合摘要行行首的图标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    /// 新建文件。
    Created,
    /// 编辑文件或应用补丁。
    Edited,
    /// 读取文件。
    Read,
    /// 运行命令。
    Command,
    /// 搜索。
    Searched,
    /// 其余工具调用。
    Tool,
}

/// 助手回合里一条工具调用的活动文案，对应前端 `toolActivityFor`。
#[derive(Debug, Clone)]
pub(crate) struct ToolActivity<'a> {
    pub(crate) kind: ActivityKind,
    /// 活动标题，如「运行命令」。
    pub(crate) title: &'static str,
    /// 活动对象：文件名、命令行或搜索词。
    pub(crate) label: String,
    pub(crate) insertions: Option<f64>,
    pub(crate) deletions: Option<f64>,
    pub(crate) block: &'a ConversationBlockDto,
}

/// 工具调用块的活动文案；非工具块返回 `None`。
///
/// 判定与前端同一张表：只有 `shell` 带耗时，搜索词回退到作用路径再回退到工具名。
pub(crate) fn activity_for(block: &ConversationBlockDto) -> Option<ToolActivity<'_>> {
    let ConversationBlockDto::ToolCall {
        name,
        arguments_json,
        metadata,
        ..
    } = block
    else {
        return None;
    };
    let args = record(arguments_json.as_ref());
    let meta = record(metadata.as_ref());
    let insertions = number_at(meta, &["insertions"]);
    let deletions = number_at(meta, &["deletions"]);

    let activity = match name.as_str() {
        "write" => {
            let created = bool_at(meta, &["created"]).unwrap_or(false);
            ToolActivity {
                kind: if created {
                    ActivityKind::Created
                } else {
                    ActivityKind::Edited
                },
                title: if created {
                    "创建文件"
                } else {
                    "编辑文件"
                },
                label: filename_label(path_for(args, meta)),
                insertions,
                deletions,
                block,
            }
        },
        "edit" | "patch" => {
            let path = path_for(args, meta);
            ToolActivity {
                kind: ActivityKind::Edited,
                title: if name == "patch" {
                    "应用补丁"
                } else {
                    "编辑文件"
                },
                label: filename_label(if path.is_empty() { "patch" } else { path }),
                insertions,
                deletions,
                block,
            }
        },
        "read" => ToolActivity {
            kind: ActivityKind::Read,
            title: "读取文件",
            label: filename_label(path_for(args, meta)),
            insertions: None,
            deletions: None,
            block,
        },
        "shell" => ToolActivity {
            kind: ActivityKind::Command,
            title: "运行命令",
            label: compact_preview_line(
                string_at(meta, &["command"])
                    .or_str(string_at(args, &["command"]))
                    .or_str(string_at(meta, &["intent"]))
                    .or_str(string_at(args, &["intent"]))
                    .or_str(name),
                144,
            ),
            insertions: None,
            deletions: None,
            block,
        },
        "grep" | "find" => {
            let pattern = string_at(meta, &["pattern"]).or_str(string_at(args, &["pattern"]));
            let scope = path_for(args, meta);
            let fallback = if scope.is_empty() {
                name.as_str()
            } else {
                scope
            };
            ToolActivity {
                kind: ActivityKind::Searched,
                title: "搜索",
                label: truncate_middle(
                    if pattern.is_empty() {
                        fallback
                    } else {
                        pattern
                    },
                    72,
                ),
                insertions: None,
                deletions: None,
                block,
            }
        },
        _ => ToolActivity {
            kind: ActivityKind::Tool,
            title: "工具调用",
            label: if name.is_empty() {
                "工具".to_owned()
            } else {
                name.clone()
            },
            insertions: None,
            deletions: None,
            block,
        },
    };
    Some(activity)
}

/// 活动行里显示的文件名：只留最后一段，过长从中间截断。
fn filename_label(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let filename = normalized
        .split('/')
        .rfind(|part| !part.is_empty())
        .unwrap_or("");

    let name = if filename.is_empty() {
        normalized.as_str()
    } else {
        filename
    };
    if name.is_empty() {
        return "文件".to_owned();
    }
    truncate_middle(name, 72)
}

/// 变更量：优先行数增删，其次字节数变化。
fn changes_label(meta: Option<&Map<String, Value>>) -> String {
    let insertions = number_at(meta, &["insertions"]);
    let deletions = number_at(meta, &["deletions"]);
    if insertions.is_some() || deletions.is_some() {
        return format!(
            "+{} -{}",
            insertions.unwrap_or(0.0),
            deletions.unwrap_or(0.0)
        );
    }
    let old_bytes = number_at(meta, &["oldBytes"]);
    let new_bytes = number_at(meta, &["newBytes"]);
    match (old_bytes, new_bytes) {
        (Some(old), Some(new)) => format!("{} -> {}", format_bytes(old), format_bytes(new)),
        (None, Some(new)) => format_bytes(new),
        _ => String::new(),
    }
}

fn byte_change_label(old_bytes: Option<f64>, new_bytes: Option<f64>) -> String {
    match (old_bytes, new_bytes) {
        (Some(old), Some(new)) => format!("{} -> {}", format_bytes(old), format_bytes(new)),
        (None, Some(new)) => format_bytes(new),
        _ => String::new(),
    }
}

/// 还有下一页时的提示文字；`hasMore`/`truncated` 都为假时为空。
fn pagination_label(meta: Option<&Map<String, Value>>) -> String {
    let has_more = bool_at(meta, &["hasMore", "truncated"]).unwrap_or(false);
    if !has_more {
        return String::new();
    }
    if let Some(offset) = number_at(meta, &["nextOffset"]) {
        return format!("more at offset {offset}");
    }
    if let Some(offset) = number_at(meta, &["nextByteOffset"]) {
        return format!("more at byte {offset}");
    }
    if let Some(offset) = number_at(meta, &["nextCharOffset"]) {
        return format!("more at char {offset}");
    }
    "has more".to_owned()
}

fn format_bytes(bytes: f64) -> String {
    if bytes < 1024.0 {
        return format!("{bytes} B");
    }
    if bytes < 1024.0 * 1024.0 {
        return format!("{:.1} KB", bytes / 1024.0);
    }
    format!("{:.1} MB", bytes / 1024.0 / 1024.0)
}

fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return String::new();
    }
    if seconds < 1.0 {
        return format!("{}ms", (seconds * 1000.0).round());
    }
    if seconds < 60.0 {
        return format!("{seconds:.1}s");
    }
    format!(
        "{}m {}s",
        (seconds / 60.0).floor(),
        (seconds % 60.0).round()
    )
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.lines().count()
}

/// 压掉连续空白并截断到 `max` 个字符，超出部分以省略号结尾。
pub(crate) fn compact_preview_line(text: &str, max: usize) -> String {
    let compacted: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compacted.chars().count() <= max {
        return compacted;
    }
    let kept: String = compacted.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// 从中间截断：保留头尾两端，中间以省略号代替。
fn truncate_middle(text: &str, max: usize) -> String {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() <= max {
        return text.to_owned();
    }
    let head = ((max - 1) as f64 * 0.58).ceil() as usize;
    let tail = ((max - 1) as f64 * 0.42).floor() as usize;
    let head: String = characters[..head].iter().collect();
    let tail: String = characters[characters.len() - tail..].iter().collect();
    format!("{head}…{tail}")
}

/// 拼接摘要行的片段，空片段跳过。
fn join(parts: &[String]) -> String {
    let mut line = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        let _ = write!(line, "{part}");
    }
    line
}

fn push(rows: &mut Vec<(&'static str, String)>, label: &'static str, value: String) {
    if !value.is_empty() {
        rows.push((label, value));
    }
}

pub(crate) fn record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value?.as_object()
}

/// 取第一个存在的字符串字段；都不存在时返回空串。
pub(crate) fn string_at<'a>(record: Option<&'a Map<String, Value>>, keys: &[&str]) -> &'a str {
    let Some(record) = record else {
        return "";
    };
    for key in keys {
        if let Some(value) = record.get(*key).and_then(Value::as_str) {
            return value;
        }
    }
    ""
}

/// 取第一个存在的数值字段；JSON 里整数与浮点都可能出现，统一按 `f64` 取。
fn number_at(record: Option<&Map<String, Value>>, keys: &[&str]) -> Option<f64> {
    let record = record?;
    keys.iter()
        .find_map(|key| record.get(*key).and_then(Value::as_f64))
}

fn bool_at(record: Option<&Map<String, Value>>, keys: &[&str]) -> Option<bool> {
    let record = record?;
    keys.iter()
        .find_map(|key| record.get(*key).and_then(Value::as_bool))
}

/// 让 `string_at` 的返回值可以链式给出退路。
trait OrStr<'a> {
    fn or_str(self, fallback: &'a str) -> &'a str;
}

impl<'a> OrStr<'a> for &'a str {
    fn or_str(self, fallback: &'a str) -> &'a str {
        if self.is_empty() { fallback } else { self }
    }
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::http::{ConversationBlockDto, ToolCallStatusDto};
    use serde_json::json;

    use super::{
        ActivityKind, DetailBody, DiffLineKind, PreviewKind, ToolView, activity_for, detail_body,
        diff_line_kind, meta_rows, numbered_line, patch_files, summary_line, tool_view,
        truncate_preview,
    };


    fn tool_call(
        name: &str,
        arguments_json: serde_json::Value,
        metadata: Option<serde_json::Value>,
        text: &str,
    ) -> ConversationBlockDto {
        ConversationBlockDto::ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: arguments_json.to_string(),
            text: text.into(),
            status: ToolCallStatusDto::Complete,
            metadata,
            approval: None,
            arguments_json: Some(arguments_json),
        }
    }

    #[test]
    fn patch_files_carry_the_per_file_outcome() {
        let block = tool_call(
            "patch",
            json!({ "patch": "--- a/a.rs\n+++ b/a.rs\n" }),
            Some(json!({
                "filesApplied": 2,
                "filesFailed": 1,
                "files": [
                    { "path": "a.rs", "changeType": "created", "applied": true },
                    { "path": "b.rs", "applied": true },
                    { "applied": false, "error": "no such file" },
                ],
            })),
            "",
        );

        let files = patch_files(&block);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].label, "created");
        assert_eq!(files[0].path, "a.rs");
        // `changeType` 缺席按 changed 兜底；`applied` 缺席按已应用算（与前端 `=== false` 同口径）。
        assert_eq!(files[1].label, "changed");
        assert!(files[1].applied);
        // 没应用上的一律标 failed，路径缺席留给渲染层兜底。
        assert_eq!(files[2].label, "failed");
        assert!(!files[2].applied);
        assert_eq!(files[2].error, "no such file");

        // `files` 不是数组时没有这一段可渲染。
        let plain = tool_call("patch", json!({}), Some(json!({ "filesApplied": 1 })), "");
        assert!(patch_files(&plain).is_empty());
    }

    #[test]
    fn names_take_precedence_and_intent_is_the_fallback() {
        // 名字层认得出：即使声明了别的 intent，也按名字走（与前端 priority 100 > 50 一致）。
        let named = tool_call(
            "read_tool_result",
            json!({ "artifactId": "a1" }),
            Some(json!({ "presentation": "read" })),
            "1\tfoo",
        );
        assert_eq!(tool_view(&named), ToolView::ToolResult);

        // 名字层认不出：由 intent 决定。
        let intent = tool_call(
            "mcp__filesystem__diff",
            json!({}),
            Some(json!({ "presentation": "diff" })),
            "",
        );
        assert_eq!(tool_view(&intent), ToolView::File);

        // intent 缺失或无法识别：通用形态。
        let unknown = tool_call(
            "mcp__x__y",
            json!({}),
            Some(json!({ "presentation": "nope" })),
            "",
        );
        assert_eq!(tool_view(&unknown), ToolView::Generic);
        let absent = tool_call("mcp__x__y", json!({}), None, "");
        assert_eq!(tool_view(&absent), ToolView::Generic);
    }

    #[test]
    fn summary_lines_are_built_from_metadata() {
        let read = tool_call(
            "read",
            json!({ "path": "crates/astrcode-ui/src/views/chat.rs" }),
            Some(json!({
                "path": "crates/astrcode-ui/src/views/chat.rs",
                "shownLines": 120,
                "totalLines": 300
            })),
            "",
        );
        assert_eq!(
            summary_line(&read).as_deref(),
            Some("read crates/astrcode-ui/src/views/chat.rs 120/300 lines")
        );

        let shell = tool_call(
            "shell",
            json!({ "command": "cargo test  -p  astrcode-ui" }),
            Some(json!({ "shellId": "sh-1" })),
            "",
        );
        assert_eq!(
            summary_line(&shell).as_deref(),
            Some("$ cargo test -p astrcode-ui")
        );

        let unknown = tool_call("mcp__x__y", json!({}), None, "some result text");
        assert_eq!(summary_line(&unknown).as_deref(), Some("some result text"));
        assert_eq!(
            summary_line(&tool_call("mcp__x__y", json!({}), None, "")),
            None
        );
    }

    #[test]
    fn meta_rows_only_carry_values_that_exist() {
        let read = tool_call(
            "read",
            json!({ "path": "a.rs", "offset": 10 }),
            Some(json!({
                "path": "a.rs",
                "shownLines": 5,
                "totalLines": 9,
                "hasMore": true,
                "nextOffset": 15
            })),
            "1\tfn main() {}",
        );
        let rows = meta_rows(&read);
        assert_eq!(
            rows,
            vec![
                ("path", "a.rs".to_owned()),
                ("lines", "5/9".to_owned()),
                ("next", "more at offset 15".to_owned()),
            ]
        );

        let generic = tool_call("mcp__x__y", json!({}), None, "");
        assert!(meta_rows(&generic).is_empty());
    }

    #[test]
    fn detail_bodies_prefer_result_text_then_arguments() {
        let diff = tool_call(
            "edit",
            json!({ "path": "a.rs" }),
            Some(json!({ "diff": "@@ -1 +1 @@\n-old\n+new\n" })),
            "Edited a.rs",
        );
        assert_eq!(
            detail_body(&diff),
            DetailBody::Text("@@ -1 +1 @@\n-old\n+new\n", PreviewKind::Diff)
        );

        let streaming_edit = tool_call(
            "edit",
            json!({ "path": "a.rs", "oldText": "old", "newText": "new" }),
            None,
            "",
        );
        assert_eq!(
            detail_body(&streaming_edit),
            DetailBody::Replacement {
                old: "old",
                new: "new"
            }
        );

        let search = tool_call("grep", json!({ "pattern": "x" }), None, "a.rs:1:x");
        assert_eq!(
            detail_body(&search),
            DetailBody::Text("a.rs:1:x", PreviewKind::Plain)
        );

        let empty = tool_call("shell", json!({ "command": "true" }), None, "");
        assert_eq!(detail_body(&empty), DetailBody::Empty);
    }

    #[test]
    fn preview_truncation_stops_at_characters_or_lines() {
        let (text, omitted) = truncate_preview("abcdef", 4, 24);
        assert_eq!(text, "abcd");
        assert_eq!(omitted, 2);

        let (text, omitted) = truncate_preview("a\nb\nc\nd", 6000, 3);
        assert_eq!(text, "a\nb\nc");
        assert_eq!(omitted, 2);

        let (text, omitted) = truncate_preview("a\nb", 6000, 24);
        assert_eq!(text, "a\nb");
        assert_eq!(omitted, 0);
    }

    #[test]
    fn diff_lines_are_classified_like_the_reference() {
        assert_eq!(diff_line_kind("+++ b/a.rs"), DiffLineKind::FileHeader);
        assert_eq!(diff_line_kind("--- a/a.rs"), DiffLineKind::FileHeader);
        assert_eq!(diff_line_kind("+new"), DiffLineKind::Addition);
        assert_eq!(diff_line_kind("-old"), DiffLineKind::Deletion);
        assert_eq!(diff_line_kind("@@ -1 +1 @@"), DiffLineKind::Hunk);
        assert_eq!(diff_line_kind(" context"), DiffLineKind::Context);
    }

    #[test]
    fn numbered_lines_need_a_line_number_prefix() {
        assert_eq!(
            numbered_line("  42\tfn main() {}"),
            Some(("42", "fn main() {}"))
        );
        assert_eq!(numbered_line("no prefix"), None);
        assert_eq!(numbered_line("x\ty"), None);
    }

    #[test]
    fn activities_name_the_object_not_the_whole_call() {
        let write = tool_call(
            "write",
            json!({ "path": "/tmp/a/b/plan.md" }),
            Some(json!({ "created": true, "insertions": 12, "deletions": 3 })),
            "",
        );
        let activity = activity_for(&write).expect("工具块应有活动标签");
        assert_eq!(activity.kind, ActivityKind::Created);
        assert_eq!(activity.title, "创建文件");
        assert_eq!(activity.label, "plan.md", "只留文件名");
        assert_eq!(activity.insertions, Some(12.0));
        assert_eq!(activity.deletions, Some(3.0));

        let read = tool_call("read", json!({ "path": "crates/x.rs" }), None, "");
        let activity = activity_for(&read).expect("工具块应有活动标签");
        assert_eq!(activity.kind, ActivityKind::Read);
        assert_eq!(activity.label, "x.rs");
        assert_eq!(activity.insertions, None, "读取不报增删");
    }

    #[test]
    fn command_activity_falls_back_from_command_to_intent_to_name() {
        let command = tool_call("shell", json!({ "command": "cargo test" }), None, "");
        let activity = activity_for(&command).expect("工具块应有活动标签");
        assert_eq!(activity.kind, ActivityKind::Command);
        assert_eq!(activity.label, "cargo test");

        let intent = tool_call(
            "shell",
            json!({ "intent": "跑一下单元测试" }),
            None,
            "输出的结果文本不该出现在标签里",
        );
        let activity = activity_for(&intent).expect("工具块应有活动标签");
        assert_eq!(activity.label, "跑一下单元测试");

        let bare = tool_call("shell", json!({}), None, "");
        let activity = activity_for(&bare).expect("工具块应有活动标签");
        assert_eq!(activity.label, "shell", "都没给时回退到工具名");
    }

    #[test]
    fn search_activity_falls_back_to_scope_then_name() {
        let pattern = tool_call("grep", json!({ "pattern": "fn main" }), None, "");
        let activity = activity_for(&pattern).expect("工具块应有活动标签");
        assert_eq!(activity.kind, ActivityKind::Searched);
        assert_eq!(activity.label, "fn main");

        let scoped = tool_call("find", json!({ "path": "crates/ui" }), None, "");
        let activity = activity_for(&scoped).expect("工具块应有活动标签");
        assert_eq!(activity.label, "crates/ui");
    }

    #[test]
    fn non_tool_blocks_have_no_activity() {
        let assistant = ConversationBlockDto::Assistant {
            id: "a1".into(),
            text: "hi".into(),
            reasoning_content: None,
            storage_seq: None,
            status: astrcode_protocol::http::ConversationBlockStatusDto::Complete,
        };
        assert!(activity_for(&assistant).is_none());
    }
}
