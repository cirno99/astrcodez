//! Compact summary 的上下文消息封装。
//!
//! Parser 负责校验模型输出；assembler 负责把摘要变成后续 provider request
//! 能稳定识别的 synthetic user message。

use std::borrow::Cow;

use super::{COMPACT_SUMMARY_END, COMPACT_SUMMARY_MARKER, parse::extract_summary_for_context};
use crate::CompactSummaryRenderOptions;

const COMPACT_CONTINUATION_PREAMBLE: &str = "This session is being continued from a previous \
                                             conversation that ran out of context. The summary \
                                             below covers the earlier portion of the conversation.";
const COMPACT_CONTINUATION_INSTRUCTIONS: &str =
    "Continue the conversation from where it left off without asking the user any further \
     questions. Resume directly: do not acknowledge this summary, do not recap it, and do not \
     preface your response with \"I'll continue\" or similar. Pick up the last task as if the \
     context break never happened.";
/// 提示行的识别前缀：已持久化的产物靠它被 `strip_compact_transcript_hint` 认出来，
/// 改这段文字会让旧产物的提示行无法剥离、累积进下一次摘要；path 之后的说明子句可改。
const COMPACT_TRANSCRIPT_HINT_PREFIX: &str = "If you need specific details from before compaction \
                                              (like exact code snippets, error messages, or \
                                              content you generated), read the full transcript at ";
const COMPACT_EXTENSION_INSTRUCTIONS_HEADER: &str = "Extension instructions to preserve:";

/// 摘要产物标注的可回溯区间：snapshot 文件里被本摘要取代的头 N 条消息，
/// 以及紧随其后仍逐字存在于上下文中的 M 条消息。
pub(super) struct CompactCoverage {
    pub(super) compressed_messages: usize,
    pub(super) retained_messages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompactSummaryEnvelope {
    /// 已去掉 `<compact_summary>` 包装和 `Summary:` 前缀的正文。
    pub(super) summary: String,
}

/// 将任意摘要文本标准化为 `Summary:\n...` 形态。
pub(super) fn format_compact_summary(summary: &str) -> String {
    let summary = extract_summary_for_context(summary);
    if summary
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("summary:")
    {
        summary.trim().to_string()
    } else {
        format!("Summary:\n{}", summary.trim())
    }
}

/// 构造压缩后重新注入 provider history 的 synthetic user message 文本。
pub(super) fn compact_summary_message_text(
    summary: &str,
    options: &CompactSummaryRenderOptions,
    coverage: CompactCoverage,
) -> String {
    let mut body = vec![
        COMPACT_CONTINUATION_PREAMBLE.to_string(),
        COMPACT_CONTINUATION_INSTRUCTIONS.to_string(),
        String::new(),
        format_compact_summary(summary),
    ];

    if let Some(path) = options
        .transcript_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        body.extend([
            String::new(),
            format!(
                "{COMPACT_TRANSCRIPT_HINT_PREFIX}{path} — it holds the {} message(s) (index 0..) \
                 that this summary replaced;{}.",
                coverage.compressed_messages,
                if coverage.retained_messages == 0 {
                    " no earlier message is kept verbatim".to_string()
                } else {
                    format!(
                        " the next {} message(s) are still present verbatim below",
                        coverage.retained_messages
                    )
                }
            ),
        ]);
    }

    if !options.custom_instructions.is_empty() {
        body.push(String::new());
        body.push(COMPACT_EXTENSION_INSTRUCTIONS_HEADER.to_string());
        for instruction in &options.custom_instructions {
            body.push(format!("- {instruction}"));
        }
    }

    format!(
        "{COMPACT_SUMMARY_MARKER}\n{}\n{COMPACT_SUMMARY_END}",
        body.join("\n")
    )
}

/// 从 synthetic compact message 中取回摘要正文。
pub(super) fn parse_compact_summary_message(content: &str) -> Option<CompactSummaryEnvelope> {
    let trimmed = content.trim();
    let body = trimmed
        .strip_prefix(COMPACT_SUMMARY_MARKER)
        .and_then(|value| value.trim().strip_suffix(COMPACT_SUMMARY_END))
        .map(str::trim)
        .unwrap_or(trimmed);
    let body = strip_compact_preamble(body);
    let body = strip_compact_transcript_hint(body);
    let body = strip_compact_extension_block(&body);
    let summary = body
        .trim_start()
        .strip_prefix("Summary:")
        .unwrap_or(body)
        .trim();
    (!summary.is_empty()).then(|| CompactSummaryEnvelope {
        summary: summary.to_string(),
    })
}

/// 删除 synthetic 包装里的 extension instructions 块。
///
/// 这些指令每次压缩都由 PreCompact 重新贡献；留在取回的摘要正文里会随 incremental
/// 压缩逐轮重复。块总是渲染在末尾，因此按行定位最后一处标题行即可。
fn strip_compact_extension_block(body: &str) -> &str {
    let mut cut = None;
    let mut offset = 0usize;
    for line in body.lines() {
        if line.trim_end() == COMPACT_EXTENSION_INSTRUCTIONS_HEADER {
            cut = Some(offset);
        }
        offset += line.len() + 1;
    }
    cut.map_or(body, |index| body[..index].trim_end())
}

fn strip_compact_preamble(body: &str) -> &str {
    let stripped = body
        .trim_start()
        .strip_prefix(COMPACT_CONTINUATION_PREAMBLE)
        .map(str::trim)
        .unwrap_or(body);
    stripped
        .strip_prefix(COMPACT_CONTINUATION_INSTRUCTIONS)
        .map(str::trim)
        .unwrap_or(stripped)
}

/// 删除 snapshot 提示行。
///
/// 该行不一定在正文末尾：extension instructions 会被追加在它之后。按行反向搜索，
/// 否则旧摘要里的提示行会随每次 incremental 累积进 previous summary。
fn strip_compact_transcript_hint(body: &str) -> Cow<'_, str> {
    let lines = body.lines().collect::<Vec<_>>();
    let Some(index) = lines.iter().rposition(|line| {
        line.trim_start()
            .starts_with(COMPACT_TRANSCRIPT_HINT_PREFIX)
    }) else {
        return Cow::Borrowed(body);
    };
    // 提示行前的空行只为分隔它而存在，一并移除。
    let separator = index
        .checked_sub(1)
        .filter(|&previous| lines[previous].trim().is_empty());
    Cow::Owned(
        lines
            .iter()
            .enumerate()
            .filter(|(line_index, _)| *line_index != index && Some(*line_index) != separator)
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// 摘要进入长期上下文前的最后清理。
pub(super) fn sanitize_compact_summary(summary: &str) -> String {
    let collapsed = collapse_compaction_whitespace(summary);
    redact_route_sensitive_tokens(&collapsed)
}

/// 合并多余空行和行尾空白，避免 compact summary 自身继续膨胀。
pub(super) fn collapse_compaction_whitespace(content: &str) -> String {
    let mut output = String::new();
    let mut blank_seen = false;
    for line in content.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            if !blank_seen && !output.trim().is_empty() {
                output.push('\n');
                output.push('\n');
            }
            blank_seen = true;
        } else {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(line);
            blank_seen = false;
        }
    }
    output.trim().to_string()
}

/// 避免把运行时路由 ID 写进长期摘要后继续传播。
fn redact_route_sensitive_tokens(content: &str) -> String {
    let mut redacted = String::with_capacity(content.len());
    let mut token = String::new();
    for ch in content.chars() {
        if ch.is_whitespace() {
            if !token.is_empty() {
                redacted.push_str(&redact_route_token(&token));
                token.clear();
            }
            redacted.push(ch);
        } else {
            token.push(ch);
        }
    }
    if !token.is_empty() {
        redacted.push_str(&redact_route_token(&token));
    }
    redacted
}

fn redact_route_token(token: &str) -> String {
    let trimmed =
        token.trim_matches(|ch: char| matches!(ch, '`' | '"' | '\'' | ',' | ';' | ')' | ']' | '}'));
    if trimmed.starts_with("root-agent:") || trimmed.starts_with("agent-") {
        token.replace(trimmed, "<agent-id>")
    } else if trimmed.starts_with("subrun-") {
        token.replace(trimmed, "<subrun-id>")
    } else if trimmed.starts_with("session-") {
        token.replace(trimmed, "<session-id>")
    } else {
        token.to_string()
    }
}
