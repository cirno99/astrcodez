//! LLM 驱动的上下文压缩模块。
//!
//! 当上下文窗口接近容量上限时，通过 LLM 对历史对话进行摘要压缩，
//! 保留关键信息的同时释放 token 空间。
//!
//! 这里定义 compact 的语义边界：如何选择要压缩的消息、如何渲染摘要
//! request、如何校验模型返回的 `<summary>`，以及如何把摘要重新组装成
//! provider 可见的 synthetic user message。真正的工具权限、hook 和 provider
//! 调用细节由调用方通过闭包承担。

use std::future::Future;

use astrcode_core::llm::{LlmContent, LlmError, LlmMessage, LlmRole};

use crate::ContextSettings;

const COMPACT_SUMMARY_END: &str = "</compact_summary>";
const MAX_PTL_RETRIES: usize = 3;
const MAX_SUMMARY_LINE_CHARS: usize = 320;

mod assemble;
mod parse;
mod plan;
mod post_compact;
mod prompt;

use parse::{
    CompactParseError, USER_MESSAGES_SECTION, listed_user_message_count, parse_compact_output,
};
use plan::{PreparedCompactInput, visible_message_text};
pub use post_compact::append_compact_retained_context;

use crate::{
    COMPACT_SUMMARY_MARKER, CompactError, CompactResult, CompactSkipReason,
    CompactSummaryRenderOptions, is_prompt_too_long_message, is_synthetic_context_message,
};

pub struct CompactExecution {
    pub result: CompactResult,
    pub llm_attempt: LlmCompactAttempt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmCompactAttempt {
    NotAttempted,
    Succeeded,
    Failed,
}

struct PreparedCompactParts {
    prefix: Vec<LlmMessage>,
    prepared_input: PreparedCompactInput,
    retained_messages: Vec<LlmMessage>,
    pre_tokens: usize,
    messages_removed: usize,
}

impl From<CompactParseError> for CompactError {
    fn from(value: CompactParseError) -> Self {
        Self::Parse(value.to_string())
    }
}

/// 不调用 LLM 的 compact fallback，并使用指定的 summary 渲染选项。
#[cfg(test)]
fn compact_messages_with_render_options(
    messages: &[LlmMessage],
    system_prompt: Option<&str>,
    render_options: &CompactSummaryRenderOptions,
) -> Result<CompactResult, CompactSkipReason> {
    compact_messages_with_render_options_and_keep(messages, system_prompt, render_options, None)
}

/// 本次压缩会被摘要取代的 provider 前缀长度，与两个 compact 入口的
/// `NothingToCompact` 判定同源；`None` 表示这次压缩会直接跳过。
///
/// 调用方在落 compact snapshot 之前用它把关，并用它裁剪要落盘的消息：
/// - auto 每个 step 都会重新规划压缩，被跳过时不该留下无人引用的孤儿快照。
/// - 快照只含被取代的前缀，边界另算一次就会与产物标注的 `index 0..N` 漂移。
pub fn compactible_prefix_len(
    messages: &[LlmMessage],
    keep_recent_turns: Option<usize>,
) -> Option<usize> {
    let keep_start = split_compact_start(messages, keep_recent_turns)?;
    (!plan::prepare_compact_input(&messages[..keep_start])
        .messages
        .is_empty())
    .then_some(keep_start)
}

fn compact_messages_with_render_options_and_keep(
    messages: &[LlmMessage],
    system_prompt: Option<&str>,
    render_options: &CompactSummaryRenderOptions,
    keep_recent_turns: Option<usize>,
) -> Result<CompactResult, CompactSkipReason> {
    let parts = prepare_compact_parts(messages, system_prompt, keep_recent_turns)?;
    let summary = summarize_prefix(&parts.prefix);
    Ok(finish_compact_summary(
        summary,
        parts.retained_messages,
        parts.pre_tokens,
        parts.messages_removed,
        parts.prefix.len(),
        system_prompt,
        render_options,
    ))
}

/// 使用调用方提供的文本请求函数生成 compact summary。
async fn compact_messages_with_request<F, Fut>(
    messages: &[LlmMessage],
    system_prompt: Option<&str>,
    settings: &ContextSettings,
    custom_instructions: &[String],
    render_options: &CompactSummaryRenderOptions,
    keep_recent_turns: Option<usize>,
    mut request_text: F,
) -> Result<CompactResult, CompactError>
where
    F: FnMut(Vec<LlmMessage>) -> Fut,
    Fut: Future<Output = Result<String, CompactError>>,
{
    let parts = prepare_compact_parts(messages, system_prompt, keep_recent_turns)?;
    let round_starts = api_round_starts(&parts.prepared_input.messages);
    let mut repair_feedback: Option<String> = None;
    let mut ptl_rounds_dropped = 0usize;
    let mut repair_attempts = 0u8;
    let max_attempts = settings.compact_max_retry_attempts.max(1);
    let mut last_error: Option<CompactError> = None;
    let mut accepted_summary: Option<String> = None;
    // 格式合格但覆盖不足的摘要：修复回路用尽后仍提交它，它比 deterministic 模板保留更多事实。
    let mut best_effort_summary: Option<String> = None;
    let mut last_coverage_gap: Option<(usize, usize)> = None;

    while repair_attempts < max_attempts {
        let Some(message_start) = round_starts.get(ptl_rounds_dropped).copied() else {
            break;
        };
        let compact_messages = request_messages(
            &parts.prepared_input,
            message_start,
            system_prompt,
            settings,
            repair_feedback.as_deref(),
            custom_instructions,
        );
        let output = match request_text(compact_messages).await {
            Ok(output) => output,
            Err(error) if should_retry_prompt_too_long(&error) => {
                let next_drop = ptl_rounds_dropped + 1;
                if next_drop > MAX_PTL_RETRIES || next_drop >= round_starts.len() {
                    last_error = Some(error);
                    break;
                }
                ptl_rounds_dropped = next_drop;
                continue;
            },
            Err(error) => {
                last_error = Some(error);
                break;
            },
        };
        repair_attempts += 1;
        match parse_compact_output(&output) {
            Ok(parsed) => {
                let candidate = assemble::sanitize_compact_summary(&parsed.summary);
                // 覆盖基线只在整段前缀都进了请求时成立；PTL 丢轮后模型无从复述没收到的消息。
                let expected_user_messages =
                    (message_start == 0).then(|| plan::user_messages_to_restate(&parts.prefix));
                let restated = listed_user_message_count(&candidate);
                let coverage_gap = expected_user_messages
                    .filter(|expected| restated < *expected)
                    .map(|expected| (expected, restated));
                match coverage_gap {
                    None => {
                        accepted_summary = Some(candidate);
                        break;
                    },
                    Some((expected, restated)) => {
                        last_coverage_gap = Some((expected, restated));
                        best_effort_summary = Some(candidate);
                        repair_feedback = Some(format!(
                            "{USER_MESSAGES_SECTION} listed {restated} entries, but this \
                             conversation contains {expected} user messages. Give every user \
                             message its own list item there, keeping the entries already \
                             inherited from the previous summary."
                        ));
                        // 故意不 break：由 `repair_attempts < max_attempts`
                        // 决定是否还有预算再问一次。
                    },
                }
            },
            Err(error) => {
                repair_feedback = Some(error.to_string());
                last_error = Some(error.into());
            },
        }
    }

    let summary = match accepted_summary {
        Some(summary) => summary,
        None => {
            let summary = best_effort_summary.ok_or_else(|| {
                last_error.unwrap_or_else(|| {
                    CompactParseError::new("compact response did not contain a summary").into()
                })
            })?;
            if let Some((expected, restated)) = last_coverage_gap {
                // 提交这版覆盖不足的 LLM 摘要：缺口由产物里的 snapshot 指针兜住，
                // 降级成 deterministic 占位符只会丢更多事实。
                tracing::warn!(
                    expected,
                    restated,
                    "compact summary under-covers user messages after contract repair; committing \
                     it instead of falling back to the deterministic template"
                );
            }
            summary
        },
    };
    Ok(finish_compact_summary(
        summary,
        parts.retained_messages,
        parts.pre_tokens,
        parts.messages_removed,
        parts.prefix.len(),
        system_prompt,
        render_options,
    ))
}

/// LLM compact + deterministic fallback 的统一入口。
///
/// 先尝试调用 LLM 生成摘要，失败时降级到确定性模板。
/// 用于 auto-compact 和 manual compact 两条路径。
pub async fn compact_messages_with_fallback<F, Fut>(
    messages: &[LlmMessage],
    system_prompt: Option<&str>,
    settings: &ContextSettings,
    custom_instructions: &[String],
    render_options: &CompactSummaryRenderOptions,
    keep_recent_turns: Option<usize>,
    request_text: F,
) -> Result<CompactExecution, CompactSkipReason>
where
    F: FnMut(Vec<LlmMessage>) -> Fut,
    Fut: Future<Output = Result<String, CompactError>>,
{
    match compact_messages_with_request(
        messages,
        system_prompt,
        settings,
        custom_instructions,
        render_options,
        keep_recent_turns,
        request_text,
    )
    .await
    {
        Ok(result) => Ok(CompactExecution {
            result,
            llm_attempt: LlmCompactAttempt::Succeeded,
        }),
        Err(CompactError::Skip(reason)) => Err(reason),
        Err(error) => {
            let llm_attempt = if matches!(error, CompactError::Llm(_)) {
                LlmCompactAttempt::Failed
            } else {
                LlmCompactAttempt::Succeeded
            };
            tracing::warn!(%error, "LLM compact failed, falling back to deterministic");
            compact_messages_with_render_options_and_keep(
                messages,
                system_prompt,
                render_options,
                keep_recent_turns,
            )
            .map(|result| CompactExecution {
                result,
                llm_attempt,
            })
        },
    }
}

/// 仅使用确定性模板压缩，不调用 LLM。
pub fn compact_messages_deterministic(
    messages: &[LlmMessage],
    system_prompt: Option<&str>,
    render_options: &CompactSummaryRenderOptions,
    keep_recent_turns: Option<usize>,
) -> Result<CompactExecution, CompactSkipReason> {
    compact_messages_with_render_options_and_keep(
        messages,
        system_prompt,
        render_options,
        keep_recent_turns,
    )
    .map(|result| CompactExecution {
        result,
        llm_attempt: LlmCompactAttempt::NotAttempted,
    })
}

fn should_retry_prompt_too_long(error: &CompactError) -> bool {
    matches!(
        error,
        CompactError::Llm(LlmError::ContextWindowExceeded { .. })
    ) || is_prompt_too_long_message(&error.to_string())
}

/// 是否可在 `split_after` 所指的 message 之后切分压缩边界（Kimi `canSplitAfter` 语义）。
///
/// `keep_start` 为保留区首条消息下标时，应对 `split_after = keep_start - 1` 调用本函数。
fn can_split_after(messages: &[LlmMessage], split_after: usize) -> bool {
    let Some(message) = messages.get(split_after) else {
        return true;
    };
    if message.role == LlmRole::User && !is_synthetic_context_message(message) {
        return false;
    }
    if message.role == LlmRole::Assistant {
        let has_tool_calls = message
            .content
            .iter()
            .any(|content| matches!(content, LlmContent::ToolCall { .. }));
        if has_tool_calls {
            return false;
        }
    }
    !messages
        .get(split_after + 1)
        .is_some_and(|next| next.role == LlmRole::Tool)
}

fn can_compact_before(messages: &[LlmMessage], keep_start: usize) -> bool {
    if keep_start == 0 {
        return false;
    }
    can_split_after(messages, keep_start - 1)
}

fn adjust_keep_start_to_safe_boundary(
    messages: &[LlmMessage],
    turn_starts: &[usize],
    mut keep_start: usize,
) -> Option<usize> {
    while keep_start > 0 && !can_compact_before(messages, keep_start) {
        let previous = turn_starts
            .iter()
            .rev()
            .copied()
            .find(|index| *index < keep_start)?;
        keep_start = previous;
    }
    can_compact_before(messages, keep_start).then_some(keep_start)
}

fn split_compact_start(messages: &[LlmMessage], keep_recent_turns: Option<usize>) -> Option<usize> {
    let has_compressible = messages
        .iter()
        .any(|m| m.role == LlmRole::Assistant && !is_synthetic_context_message(m));
    if !has_compressible {
        return None;
    }

    let turn_starts = user_turn_starts(messages);
    // 将 `keep_recent_turns` 兜底为1，确保默认保留最近一轮llm消息，避免压缩掉所有消息导致信息丢失
    let keep_turns = keep_recent_turns.unwrap_or(1);
    if keep_turns >= turn_starts.len() {
        return None;
    }

    if keep_turns == 0 {
        return Some(messages.len());
    }

    let candidate = turn_starts
        .get(turn_starts.len().saturating_sub(keep_turns))
        .copied()?;
    adjust_keep_start_to_safe_boundary(messages, &turn_starts, candidate)
}

fn removed_visible_messages(messages: &[LlmMessage]) -> usize {
    messages
        .iter()
        .filter(|message| !is_synthetic_context_message(message))
        .count()
}

fn prepare_compact_parts(
    messages: &[LlmMessage],
    system_prompt: Option<&str>,
    keep_recent_turns: Option<usize>,
) -> Result<PreparedCompactParts, CompactSkipReason> {
    if messages.is_empty() {
        return Err(CompactSkipReason::Empty);
    }
    let keep_start = split_compact_start(messages, keep_recent_turns)
        .ok_or(CompactSkipReason::NothingToCompact)?;

    let prefix = messages[..keep_start].to_vec();
    let prepared_input = plan::prepare_compact_input(&prefix);
    if prepared_input.messages.is_empty() {
        return Err(CompactSkipReason::NothingToCompact);
    }

    let retained_messages = messages[keep_start..].to_vec();
    let pre_tokens =
        crate::token_budget::estimate_request_tokens_with_prompt(messages, system_prompt);
    let messages_removed = removed_visible_messages(&prefix);
    Ok(PreparedCompactParts {
        prefix,
        prepared_input,
        retained_messages,
        pre_tokens,
        messages_removed,
    })
}

fn user_turn_starts(messages: &[LlmMessage]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            (message.role == LlmRole::User && !is_synthetic_context_message(message))
                .then_some(index)
        })
        .collect()
}

fn request_messages(
    prepared_input: &PreparedCompactInput,
    message_start: usize,
    system_prompt: Option<&str>,
    settings: &ContextSettings,
    repair_feedback: Option<&str>,
    custom_instructions: &[String],
) -> Vec<LlmMessage> {
    let input_messages = &prepared_input.messages[message_start..];
    let mut messages = Vec::with_capacity(input_messages.len() + 2);
    if let Some(system_prompt) = system_prompt {
        messages.extend(crate::prompt_engine::system_messages_from_prompt(
            system_prompt,
        ));
    }
    messages.extend_from_slice(input_messages);
    messages.push(LlmMessage::user(prompt::render_compact_request(
        &prepared_input.prompt_mode,
        settings,
        repair_feedback,
        custom_instructions,
    )));
    messages
}

fn api_round_starts(messages: &[LlmMessage]) -> Vec<usize> {
    if messages.is_empty() {
        return Vec::new();
    }
    std::iter::once(0)
        .chain(
            messages
                .iter()
                .enumerate()
                .skip(1)
                .filter_map(|(index, message)| (message.role == LlmRole::User).then_some(index)),
        )
        .collect()
}

fn finish_compact_summary(
    summary: String,
    retained_messages: Vec<LlmMessage>,
    pre_tokens: usize,
    messages_removed: usize,
    compressed_message_count: usize,
    system_prompt: Option<&str>,
    render_options: &CompactSummaryRenderOptions,
) -> CompactResult {
    let summary_messages = vec![LlmMessage::user(assemble::compact_summary_message_text(
        &summary,
        render_options,
        assemble::CompactCoverage {
            compressed_messages: compressed_message_count,
            retained_messages: retained_messages.len(),
        },
    ))];
    let post_tokens = crate::token_budget::estimate_request_tokens_with_prompt(
        &[summary_messages.clone(), retained_messages.clone()].concat(),
        system_prompt,
    );

    CompactResult {
        pre_tokens,
        post_tokens,
        summary,
        messages_removed,
        compressed_message_count,
        summary_messages,
        retained_messages,
        transcript_path: render_options.transcript_path.clone(),
    }
}

fn summarize_prefix(messages: &[LlmMessage]) -> String {
    let mut lines = vec![
        "1. Primary Request and Intent:".to_string(),
        format!("   - Compacted {} earlier messages.", messages.len()),
        String::new(),
        "2. Key Technical Concepts:".to_string(),
        "   - (unknown from deterministic fallback)".to_string(),
        String::new(),
        "3. Files and Code Sections:".to_string(),
        "   - (none)".to_string(),
        String::new(),
        "4. Errors and fixes:".to_string(),
        "   - (none)".to_string(),
        String::new(),
        "5. Problem Solving:".to_string(),
        "   - Deterministic fallback summary was used because provider-backed compact was \
         unavailable."
            .to_string(),
        String::new(),
        "6. All user messages:".to_string(),
    ];

    for message in messages.iter().rev().take(12).rev() {
        let role = message.role.as_str();
        let text = visible_message_text(message);
        if text.trim().is_empty() {
            continue;
        }
        let text = summary_line(&text);
        if message.role == LlmRole::User {
            lines.push(format!("   - {text}"));
        } else {
            lines.push(format!("   - {role}: {text}"));
        }
    }
    lines.extend([
        String::new(),
        "7. Pending Tasks:".to_string(),
        "   - (unknown)".to_string(),
        String::new(),
        "8. Current Work:".to_string(),
        "   - (unknown)".to_string(),
        String::new(),
        "9. Optional Next Step:".to_string(),
        "   - (none)".to_string(),
    ]);

    lines.join("\n")
}

fn summary_line(text: &str) -> String {
    let mut line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((byte_index, _)) = line.char_indices().nth(MAX_SUMMARY_LINE_CHARS) {
        line.truncate(byte_index);
        line.push('…');
    }
    line
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    fn assistant_tool_call(call_id: &str, name: &str, arguments: serde_json::Value) -> LlmMessage {
        LlmMessage {
            role: LlmRole::Assistant,
            content: vec![LlmContent::ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                arguments,
                raw_arguments: None,
            }],
            name: None,
            reasoning_content: None,
        }
    }

    fn valid_compact_summary() -> &'static str {
        r#"<analysis>
The summary should preserve the compact contract and omit this scratchpad later.
</analysis>

<summary>
1. Primary Request and Intent:
   preserve structure

2. Key Technical Concepts:
   - compact

3. Files and Code Sections:
   - crates/astrcode-context/src/compaction/mod.rs

4. Errors and fixes:
   - (none)

5. Problem Solving:
   compacted

6. All user messages:
   - user asked for compact

7. Pending Tasks:
   - (none)

8. Current Work:
   compact parser

9. Optional Next Step:
   - (none)
</summary>"#
    }

    #[test]
    fn summary_formatting_and_message_round_trip_strip_scratchpad_markup() {
        let formatted = assemble::format_compact_summary(
            r#"
<analysis>
scratchpad that should not survive
</analysis>

<summary>
1. Primary Request and Intent:
   migrate context-window
</summary>
"#,
        );
        assert_eq!(
            formatted,
            "Summary:\n1. Primary Request and Intent:\n   migrate context-window"
        );
        assert!(!formatted.contains("<analysis>"));
        assert!(!formatted.contains("<summary>"));

        let message = assemble::compact_summary_message_text(
            "1. Primary Request and Intent:\n   keep user intent",
            &CompactSummaryRenderOptions {
                transcript_path: Some("C:\\Users\\18794\\.astrcode\\compact.jsonl".into()),
                custom_instructions: Vec::new(),
            },
            assemble::CompactCoverage {
                compressed_messages: 7,
                retained_messages: 2,
            },
        );
        assert!(message.starts_with("<compact_summary>\nThis session is being continued"));
        assert!(message.contains("Resume directly: do not acknowledge this summary"));
        assert!(message.contains("read the full transcript at C:\\Users\\18794"));
        assert!(message.contains("it holds the 7 message(s)"));
        assert!(message.contains("next 2 message(s) are still present verbatim below"));
        assert_eq!(
            assemble::parse_compact_summary_message(&message)
                .unwrap()
                .summary,
            "1. Primary Request and Intent:\n   keep user intent"
        );
    }

    /// snapshot 提示行与 extension instructions 块都排在摘要正文之后：取回 previous
    /// summary 时必须整块剥掉，否则每次 incremental 都累积陈旧路径与重复指令。
    #[test]
    fn parse_compact_summary_message_strips_wrappers_after_summary_body() {
        let message = assemble::compact_summary_message_text(
            "1. Primary Request and Intent:\n   keep user intent",
            &CompactSummaryRenderOptions {
                transcript_path: Some("/root/.astrcode/compact.jsonl".into()),
                custom_instructions: vec!["preserve the plan".into()],
            },
            assemble::CompactCoverage {
                compressed_messages: 4,
                retained_messages: 0,
            },
        );
        let parsed = assemble::parse_compact_summary_message(&message).unwrap();
        assert_eq!(
            parsed.summary,
            "1. Primary Request and Intent:\n   keep user intent"
        );
    }

    #[test]
    fn compact_prompts_keep_the_required_analysis_and_summary_contract() {
        let settings = ContextSettings::default();
        let prompt = prompt::render_compact_contract(
            Some("system prompt"),
            &plan::CompactPromptMode::Fresh,
            &settings,
            None,
            &[],
        );
        for section in parse::REQUIRED_SUMMARY_SECTIONS {
            assert!(prompt.contains(section), "missing {section}");
        }
        assert!(prompt.contains("<summary>"));
        assert!(prompt.contains("<analysis>"));
        assert!(prompt.contains("scratchpad"));
        assert!(!prompt.contains("<recent_user_context_digest>"));

        let repair = prompt::render_compact_contract(
            None,
            &plan::CompactPromptMode::Fresh,
            &settings,
            Some("missing section"),
            &[],
        );
        assert!(
            repair.contains("Return one <analysis> scratchpad block followed by the <summary>")
        );
        assert!(!repair.contains("Return the <summary> block exactly"));
    }

    #[test]
    fn compact_keeps_recent_user_turns_and_builds_context_message() {
        let messages = vec![
            LlmMessage::user("old one"),
            LlmMessage::assistant("answer"),
            LlmMessage::user("old two"),
            LlmMessage::assistant("answer"),
            LlmMessage::user("recent"),
        ];
        let result =
            compact_messages_with_render_options(&messages, None, &Default::default()).unwrap();

        assert_eq!(result.messages_removed, 4);
        assert_eq!(result.retained_messages.len(), 1);
        assert_eq!(visible_message_text(&result.retained_messages[0]), "recent");
        assert!(crate::is_compact_summary_message(
            &result.summary_messages[0]
        ));
        assert!(visible_message_text(&result.summary_messages[0]).contains("Summary:\n"));
    }

    #[test]
    fn compact_turn_split_ignores_synthetic_summary_messages() {
        let messages = vec![
            LlmMessage::user(assemble::compact_summary_message_text(
                "old compacted work",
                &CompactSummaryRenderOptions::default(),
                assemble::CompactCoverage {
                    compressed_messages: 1,
                    retained_messages: 3,
                },
            )),
            LlmMessage::user("old real"),
            LlmMessage::assistant("answer"),
            LlmMessage::user("recent real"),
        ];
        let result =
            compact_messages_with_render_options(&messages, None, &Default::default()).unwrap();

        assert_eq!(result.retained_messages.len(), 1);
        assert_eq!(
            visible_message_text(&result.retained_messages[0]),
            "recent real"
        );
        assert_eq!(result.messages_removed, 2);
    }

    #[test]
    fn can_split_after_rejects_unsafe_boundaries() {
        let messages = vec![
            LlmMessage::user("u1"),
            LlmMessage::assistant("a1"),
            LlmMessage::user("u2"),
            assistant_tool_call("call-1", "tool", json!({})),
            LlmMessage::tool("tool", "call-1", "ok", false),
            LlmMessage::user("u3"),
        ];
        assert!(!can_split_after(&messages, 0));
        assert!(!can_split_after(&messages, 2));
        assert!(!can_split_after(&messages, 3));
        assert!(can_split_after(&messages, 4));
    }

    #[test]
    fn split_compact_start_skips_unsafe_user_turn_boundary() {
        let messages = vec![
            LlmMessage::user("old"),
            assistant_tool_call("c1", "read", json!({"path": "a"})),
            LlmMessage::tool("read", "c1", "done", false),
            LlmMessage::user("recent"),
            LlmMessage::assistant("done"),
        ];
        let keep_start = split_compact_start(&messages, Some(1)).unwrap();
        assert_eq!(keep_start, 3);
        assert_eq!(visible_message_text(&messages[keep_start]), "recent");
    }

    #[test]
    fn compact_keep_recent_turns_supports_zero_and_exact_tail_count() {
        let messages = vec![
            LlmMessage::user("u1"),
            LlmMessage::assistant("a1"),
            LlmMessage::user("u2"),
            LlmMessage::assistant("a2"),
            LlmMessage::user("u3"),
            LlmMessage::assistant("a3"),
        ];

        let full = compact_messages_with_render_options_and_keep(
            &messages,
            None,
            &Default::default(),
            Some(0),
        )
        .unwrap();
        assert!(full.retained_messages.is_empty());
        assert_eq!(full.messages_removed, 6);

        let keep_two = compact_messages_with_render_options_and_keep(
            &messages,
            None,
            &Default::default(),
            Some(2),
        )
        .unwrap();
        assert_eq!(keep_two.retained_messages.len(), 4);
        assert_eq!(visible_message_text(&keep_two.retained_messages[0]), "u2");
        assert_eq!(keep_two.messages_removed, 2);

        let nothing = compact_messages_with_render_options_and_keep(
            &messages,
            None,
            &Default::default(),
            Some(3),
        );
        assert!(matches!(nothing, Err(CompactSkipReason::NothingToCompact)));
    }

    #[test]
    fn prompt_too_long_classifier_ignores_rate_limits() {
        assert!(is_prompt_too_long_message(
            "maximum context length exceeded"
        ));
        assert!(!is_prompt_too_long_message(
            "rate limit: too many tokens per minute"
        ));
    }

    #[test]
    fn parse_compact_output_accepts_required_nine_section_summary() {
        let parsed = parse_compact_output(valid_compact_summary()).unwrap();

        assert!(parsed.summary.contains("Primary Request and Intent"));
        assert!(!parsed.summary.contains("scratchpad"));
        assert!(!parsed.summary.contains("<analysis>"));
    }

    #[test]
    fn parse_compact_output_rejects_missing_required_section() {
        let raw = r#"
<summary>
1. Primary Request and Intent:
   preserve structure
</summary>
"#;

        let error = parse_compact_output(raw).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("compact summary missing required section title")
        );
    }

    #[tokio::test]
    async fn compact_request_closure_receives_forked_prompt() {
        let settings = ContextSettings::default();
        let messages = vec![
            LlmMessage::user("old user"),
            LlmMessage::assistant("old answer"),
            LlmMessage::user("recent user"),
        ];
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_request = Arc::clone(&captured);

        let result = compact_messages_with_request(
            &messages,
            Some("main system prompt"),
            &settings,
            &[String::from("preserve compact instruction")],
            &CompactSummaryRenderOptions::default(),
            None,
            move |request| {
                *captured_for_request.lock().unwrap() = request;
                async { Ok(valid_compact_summary().to_string()) }
            },
        )
        .await
        .unwrap();

        assert_eq!(result.messages_removed, 2);
        let request = captured.lock().unwrap();

        assert_eq!(request[0].role, LlmRole::System);
        assert_eq!(visible_message_text(&request[0]), "main system prompt");
        assert_eq!(request.last().unwrap().role, LlmRole::User);
        let summary_request = visible_message_text(request.last().unwrap());
        assert!(summary_request.contains("Do not call tools"));
        assert!(summary_request.contains("<analysis>"));
        assert!(summary_request.contains("1. Primary Request and Intent:"));
        assert!(summary_request.contains("<summary>"));
        assert!(summary_request.contains("preserve compact instruction"));
        assert!(!summary_request.contains("Current runtime system prompt"));
    }

    #[tokio::test]
    async fn compact_request_renders_tool_results_as_transcript_text() {
        let settings = ContextSettings::default();
        let messages = vec![
            LlmMessage::user("read a file"),
            LlmMessage {
                role: LlmRole::Assistant,
                content: vec![LlmContent::ToolCall {
                    call_id: "call-read".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "src/lib.rs" }),
                    raw_arguments: None,
                }],
                name: None,
                reasoning_content: None,
            },
            LlmMessage::tool("read", "call-read", "pub fn compact_fixture() {}", false),
            LlmMessage::assistant("The file defines compact_fixture."),
            LlmMessage::user("current request"),
        ];
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_request = Arc::clone(&captured);

        compact_messages_with_request(
            &messages,
            None,
            &settings,
            &[],
            &CompactSummaryRenderOptions::default(),
            None,
            move |request| {
                *captured_for_request.lock().unwrap() = request;
                async { Ok(valid_compact_summary().to_string()) }
            },
        )
        .await
        .unwrap();

        let request = captured.lock().unwrap();
        assert!(
            request.iter().all(|message| message.role != LlmRole::Tool),
            "compact request should be plain transcript text, not provider tool protocol"
        );
        assert!(request.iter().any(|message| {
            visible_message_text(message).contains("tool read result")
                && visible_message_text(message).contains("pub fn compact_fixture()")
        }));
    }

    #[tokio::test]
    async fn compact_prompt_too_long_drops_oldest_api_round_and_retries() {
        let settings = ContextSettings::default();
        let messages = vec![
            LlmMessage::user("round one user"),
            LlmMessage::assistant("round one assistant"),
            LlmMessage::user("round two user"),
            LlmMessage::assistant("round two assistant"),
            LlmMessage::user("round three user"),
            LlmMessage::assistant("round three assistant"),
            LlmMessage::user("current user"),
        ];
        let attempts = Arc::new(Mutex::new(0usize));
        let requests = Arc::new(Mutex::new(Vec::<Vec<LlmMessage>>::new()));
        let attempts_for_request = Arc::clone(&attempts);
        let requests_for_request = Arc::clone(&requests);

        let result = compact_messages_with_request(
            &messages,
            None,
            &settings,
            &[],
            &CompactSummaryRenderOptions::default(),
            None,
            move |request| {
                requests_for_request.lock().unwrap().push(request);
                let attempts = Arc::clone(&attempts_for_request);
                async move {
                    let mut attempts = attempts.lock().unwrap();
                    *attempts += 1;
                    if *attempts == 1 {
                        Err(LlmError::ContextWindowExceeded {
                            message: "compact request exceeded context".into(),
                        }
                        .into())
                    } else {
                        Ok(valid_compact_summary().to_string())
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(result.messages_removed, 6);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0]
                .iter()
                .any(|message| { visible_message_text(message).contains("round one user") })
        );
        assert!(!requests[1].iter().any(|message| {
            visible_message_text(message).contains("round one user")
                || visible_message_text(message).contains("round one assistant")
        }));
        assert!(
            requests[1]
                .iter()
                .any(|message| { visible_message_text(message).contains("round two user") })
        );
    }

    /// 从共用 fixture 派生一份「第 6 段有 N 条」的摘要，让覆盖校验的用例只差条目数。
    fn compact_summary_with_user_messages(entries: &[&str]) -> String {
        let listing = entries
            .iter()
            .map(|entry| format!("   - {entry}"))
            .collect::<Vec<_>>()
            .join("\n");
        valid_compact_summary().replace("   - user asked for compact", &listing)
    }

    fn three_turn_transcript() -> Vec<LlmMessage> {
        vec![
            LlmMessage::user("第一条用户消息"),
            LlmMessage::assistant("第一条回复"),
            LlmMessage::user("第二条用户消息"),
            LlmMessage::assistant("第二条回复"),
            LlmMessage::user("最近的用户消息"),
        ]
    }

    /// 摘要漏列用户消息时，必须经 `{{CONTRACT_REPAIR}}` 追问一次，而不是静默提交缺条目的产物。
    #[tokio::test]
    async fn compact_summary_coverage_gate_repairs_missing_user_messages() {
        let settings = ContextSettings::default();
        let attempts = Arc::new(Mutex::new(0usize));
        let requests = Arc::new(Mutex::new(Vec::<Vec<LlmMessage>>::new()));
        let attempts_for_request = Arc::clone(&attempts);
        let requests_for_request = Arc::clone(&requests);

        let result = compact_messages_with_request(
            &three_turn_transcript(),
            None,
            &settings,
            &[],
            &CompactSummaryRenderOptions::default(),
            None,
            move |request| {
                requests_for_request.lock().unwrap().push(request);
                let mut attempts = attempts_for_request.lock().unwrap();
                *attempts += 1;
                let entries: &[&str] = if *attempts == 1 {
                    &["第一条用户消息"]
                } else {
                    &["第一条用户消息", "第二条用户消息"]
                };
                let summary = compact_summary_with_user_messages(entries);
                async move { Ok(summary) }
            },
        )
        .await
        .unwrap();

        assert_eq!(result.compressed_message_count, 4);
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2, "覆盖不足应触发一次契约修复重试");
            let repair_request = visible_message_text(requests[1].last().unwrap());
            assert!(repair_request.contains("## Contract Repair"));
            assert!(repair_request.contains("conversation contains 2 user messages"));
        }
        assert_eq!(listed_user_message_count(&result.summary), 2);
    }

    /// 修复预算用尽后仍缺口时，提交的必须是这版 LLM 摘要；降级成 deterministic 占位符
    /// 会丢更多事实，而缺口已由产物里的 snapshot 指针兜住。
    #[tokio::test]
    async fn compact_summary_keeps_under_covering_llm_summary_when_repairs_exhaust() {
        let settings = ContextSettings {
            compact_max_retry_attempts: 2,
            ..ContextSettings::default()
        };
        let attempts = Arc::new(Mutex::new(0usize));
        let requests = Arc::new(Mutex::new(Vec::<Vec<LlmMessage>>::new()));
        let attempts_for_request = Arc::clone(&attempts);
        let requests_for_request = Arc::clone(&requests);

        let result = compact_messages_with_request(
            &three_turn_transcript(),
            None,
            &settings,
            &[],
            &CompactSummaryRenderOptions::default(),
            None,
            move |request| {
                requests_for_request.lock().unwrap().push(request);
                let mut attempts = attempts_for_request.lock().unwrap();
                *attempts += 1;
                let summary = compact_summary_with_user_messages(&["第一条用户消息"]);
                async move { Ok(summary) }
            },
        )
        .await
        .unwrap();

        assert_eq!(requests.lock().unwrap().len(), 2);
        assert_eq!(listed_user_message_count(&result.summary), 1);
        assert!(
            !result.summary.contains("Compacted 4 earlier messages"),
            "覆盖不足不得退回 deterministic 模板：{}",
            result.summary
        );
    }

    /// 输入侧基线不含 synthetic 注入；否则每次 auto-compact 都会把注入文本当成
    /// 一条必须复述的用户消息，门禁永远无法通过。
    #[test]
    fn user_messages_to_restate_skips_synthetic_and_tool_bodies() {
        let messages = vec![
            LlmMessage::user("真实用户消息"),
            LlmMessage::assistant("回答"),
            LlmMessage::user(assemble::compact_summary_message_text(
                "1. Primary Request and Intent:\n   旧摘要",
                &CompactSummaryRenderOptions::default(),
                assemble::CompactCoverage {
                    compressed_messages: 2,
                    retained_messages: 1,
                },
            )),
            LlmMessage::tool("read", "call-1", "文件正文", false),
            LlmMessage::user("   "),
        ];

        assert_eq!(plan::user_messages_to_restate(&messages), 1);
    }

    /// 快照落盘范围必须与 compact 入口同源：判为可压却在入口被跳过会让产物缺可回溯
    /// 指针，判为不可压却实际压缩会留下无人引用的孤儿快照；前缀长度还必须等于产物
    /// 标注的覆盖条目数，否则指针里的 `index 0..N` 会与实际落盘的条目错位。
    #[test]
    fn compactible_prefix_len_matches_compact_skip_and_coverage() {
        let cases: Vec<(&str, Vec<LlmMessage>, Option<usize>)> = vec![
            (
                "多轮可压",
                vec![
                    LlmMessage::user("old"),
                    LlmMessage::assistant("answer"),
                    LlmMessage::user("recent"),
                ],
                None,
            ),
            (
                "只有一轮",
                vec![LlmMessage::user("only"), LlmMessage::assistant("answer")],
                None,
            ),
            (
                "仅 synthetic 与 assistant",
                vec![
                    LlmMessage::user(COMPACT_SUMMARY_MARKER),
                    LlmMessage::assistant("answer"),
                    LlmMessage::user("recent"),
                ],
                None,
            ),
            ("空 transcript", Vec::new(), None),
            (
                "keep_recent_turns 为 0",
                vec![LlmMessage::user("old"), LlmMessage::assistant("answer")],
                Some(0),
            ),
        ];
        for (name, messages, keep_recent_turns) in cases {
            let compacted = compact_messages_with_render_options_and_keep(
                &messages,
                None,
                &CompactSummaryRenderOptions::default(),
                keep_recent_turns,
            );
            assert_eq!(
                compactible_prefix_len(&messages, keep_recent_turns),
                compacted.ok().map(|result| result.compressed_message_count),
                "{name}"
            );
        }
    }
}
