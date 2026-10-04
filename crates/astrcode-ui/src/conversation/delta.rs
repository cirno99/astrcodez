//! 增量归并与块级应用。
//!
//! 对应 Web 前端的 `store/delta/coalesce.ts`。归并只是「相邻同目标 delta 的批内拼接」
//! （ADR 0001 第 13 轮），不是独立领域语义；真正必须保住的是 [`ConversationState`]
//! 的**孤儿 patch 不变式**：没有 start 或 durable request 时，增量不得造出 block。
//!
//! [`ConversationState`]: super::ConversationState

use std::collections::HashMap;

use astrcode_protocol::{
    http::{ConversationBlockDto, ConversationBlockStatusDto, ConversationDeltaDto},
    wire::ToolOutputStreamDto,
};

/// 归并后的增量：块级变更被合并，其余原样保留。
#[derive(Debug, Clone)]
pub enum Coalesced {
    PatchBlock {
        block_id: String,
        text_delta: String,
    },
    ThinkingDelta {
        block_id: String,
        delta: String,
    },
    PatchArguments {
        block_id: String,
        arguments: String,
        arguments_json: Option<serde_json::Value>,
    },
    ToolOutput {
        call_id: String,
        parts: Vec<(ToolOutputStreamDto, String)>,
    },
    Other(Box<ConversationDeltaDto>),
}

/// 取 block 的稳定 id。
///
/// 用领域 id 而不是列表下标做索引：同一块会随流式更新反复出现，下标不稳。
pub fn block_id(block: &ConversationBlockDto) -> &str {
    match block {
        ConversationBlockDto::User { id, .. }
        | ConversationBlockDto::Assistant { id, .. }
        | ConversationBlockDto::ToolCall { id, .. }
        | ConversationBlockDto::Error { id, .. }
        | ConversationBlockDto::Recap { id, .. }
        | ConversationBlockDto::SystemNote { id, .. }
        | ConversationBlockDto::CompactSummary { id, .. } => id,
    }
}

/// 按 `mergeBlock` 的字段回退规则合并两个同 id 的块。
///
/// 回退只在对方「没有提供内容」时生效：assistant 的 `text` 以空串视为未提供，
/// toolCall 的三个文本字段以 trim 后为空视为未提供。
pub fn merge_block(
    current: ConversationBlockDto,
    incoming: ConversationBlockDto,
) -> ConversationBlockDto {
    match (current, incoming) {
        (
            ConversationBlockDto::Assistant {
                text: current_text,
                reasoning_content: current_reasoning,
                ..
            },
            ConversationBlockDto::Assistant {
                id,
                text,
                reasoning_content,
                storage_seq,
                status,
            },
        ) => ConversationBlockDto::Assistant {
            id,
            text: if text.is_empty() { current_text } else { text },
            reasoning_content: reasoning_content.or(current_reasoning),
            storage_seq,
            status,
        },
        (
            ConversationBlockDto::ToolCall {
                name: current_name,
                arguments: current_arguments,
                text: current_text,
                metadata: current_metadata,
                arguments_json: current_arguments_json,
                ..
            },
            ConversationBlockDto::ToolCall {
                id,
                name,
                arguments,
                text,
                status,
                metadata,
                approval,
                arguments_json,
            },
        ) => ConversationBlockDto::ToolCall {
            id,
            name: if name.trim().is_empty() {
                current_name
            } else {
                name
            },
            arguments: if arguments.trim().is_empty() {
                current_arguments
            } else {
                arguments
            },
            text: if text.trim().is_empty() {
                current_text
            } else {
                text
            },
            status,
            metadata: metadata.or(current_metadata),
            approval,
            arguments_json: arguments_json.or(current_arguments_json),
        },
        (_, incoming) => incoming,
    }
}

/// 相邻同目标 delta 的批内拼接。
pub fn coalesce(deltas: &[ConversationDeltaDto]) -> Vec<Coalesced> {
    let mut result: Vec<Coalesced> = Vec::with_capacity(deltas.len());
    for delta in deltas {
        match delta {
            ConversationDeltaDto::PatchBlock {
                block_id,
                text_delta,
            } => match result.last_mut() {
                Some(Coalesced::PatchBlock {
                    block_id: last_id,
                    text_delta: last_delta,
                }) if last_id == block_id => last_delta.push_str(text_delta),
                _ => result.push(Coalesced::PatchBlock {
                    block_id: block_id.clone(),
                    text_delta: text_delta.clone(),
                }),
            },
            ConversationDeltaDto::ThinkingDelta { block_id, delta } => match result.last_mut() {
                Some(Coalesced::ThinkingDelta {
                    block_id: last_id,
                    delta: last_delta,
                }) if last_id == block_id => last_delta.push_str(delta),
                _ => result.push(Coalesced::ThinkingDelta {
                    block_id: block_id.clone(),
                    delta: delta.clone(),
                }),
            },
            ConversationDeltaDto::PatchArguments {
                block_id,
                arguments,
                arguments_json,
            } => match result.last_mut() {
                Some(Coalesced::PatchArguments {
                    block_id: last_id,
                    arguments: last_arguments,
                    arguments_json: last_json,
                }) if last_id == block_id => {
                    last_arguments.clone_from(arguments);
                    last_json.clone_from(arguments_json);
                },
                _ => result.push(Coalesced::PatchArguments {
                    block_id: block_id.clone(),
                    arguments: arguments.clone(),
                    arguments_json: arguments_json.clone(),
                }),
            },
            ConversationDeltaDto::ToolOutput {
                call_id,
                stream,
                delta,
            } => match result.last_mut() {
                Some(Coalesced::ToolOutput {
                    call_id: last_id,
                    parts,
                }) if last_id == call_id => parts.push((*stream, delta.clone())),
                _ => result.push(Coalesced::ToolOutput {
                    call_id: call_id.clone(),
                    parts: vec![(*stream, delta.clone())],
                }),
            },
            other => result.push(Coalesced::Other(Box::new(other.clone()))),
        }
    }
    result
}

/// 工具输出流的展示前缀，与前端 `applyCoalescedDeltas` 一致。
pub fn stream_prefix(stream: ToolOutputStreamDto) -> &'static str {
    match stream {
        ToolOutputStreamDto::Stdout => "\n",
        ToolOutputStreamDto::Stderr => "\n[stderr] ",
    }
}

/// 从一个块里取出「增量文本落在哪」。
fn text_field(block: &mut ConversationBlockDto) -> Option<&mut String> {
    match block {
        ConversationBlockDto::Assistant { text, .. }
        | ConversationBlockDto::ToolCall { text, .. } => Some(text),
        _ => None,
    }
}

fn text_of(block: &ConversationBlockDto) -> &str {
    match block {
        ConversationBlockDto::User { text, .. }
        | ConversationBlockDto::Assistant { text, .. }
        | ConversationBlockDto::ToolCall { text, .. }
        | ConversationBlockDto::Recap { text, .. }
        | ConversationBlockDto::SystemNote { text, .. } => text,
        ConversationBlockDto::CompactSummary { summary, .. } => summary,
        ConversationBlockDto::Error { .. } => "",
    }
}

/// 把归并后的块级变更应用到 `state.blocks`；返回是否有过实际改动。
///
/// 目标块不存在时跳过——这是孤儿 patch 不变式：增量只能改已有块，不能凭空造块。
pub fn apply_block_deltas(blocks: &mut [ConversationBlockDto], coalesced: &[Coalesced]) -> bool {
    let mut block_index: HashMap<&str, usize> = HashMap::new();
    let mut tool_call_index: HashMap<&str, usize> = HashMap::new();
    for (index, block) in blocks.iter().enumerate() {
        let id = block_id(block);
        block_index.entry(id).or_insert(index);
        if matches!(block, ConversationBlockDto::ToolCall { .. }) {
            tool_call_index.entry(id).or_insert(index);
        }
    }

    // 同一个块可能被多条增量命中，改动先攒在这里，最后一次性写回。
    let mut mutations: HashMap<usize, ConversationBlockDto> = HashMap::new();

    for item in coalesced {
        match item {
            Coalesced::PatchBlock {
                block_id,
                text_delta,
            } => {
                let Some(&index) = block_index.get(block_id.as_str()) else {
                    continue;
                };
                let mut target = mutations.get(&index).unwrap_or(&blocks[index]).clone();
                if let Some(text) = text_field(&mut target) {
                    text.push_str(text_delta);
                    mutations.insert(index, target);
                }
            },
            Coalesced::ThinkingDelta { block_id, delta } => {
                let Some(&index) = block_index.get(block_id.as_str()) else {
                    continue;
                };
                let mut target = mutations.get(&index).unwrap_or(&blocks[index]).clone();
                if let ConversationBlockDto::Assistant {
                    reasoning_content, ..
                } = &mut target
                {
                    reasoning_content
                        .get_or_insert_with(String::new)
                        .push_str(delta);
                    mutations.insert(index, target);
                }
            },
            Coalesced::PatchArguments {
                block_id,
                arguments,
                arguments_json,
            } => {
                let Some(&index) = tool_call_index.get(block_id.as_str()) else {
                    continue;
                };
                if arguments.trim().is_empty() {
                    continue;
                }
                let mut target = mutations.get(&index).unwrap_or(&blocks[index]).clone();
                if let ConversationBlockDto::ToolCall {
                    arguments: slot,
                    arguments_json: json_slot,
                    ..
                } = &mut target
                {
                    slot.clone_from(arguments);
                    *json_slot = arguments_json.clone();
                    mutations.insert(index, target);
                }
            },
            Coalesced::ToolOutput { call_id, parts } => {
                let Some(&index) = tool_call_index.get(call_id.as_str()) else {
                    continue;
                };
                let mut target = mutations.get(&index).unwrap_or(&blocks[index]).clone();
                if !matches!(target, ConversationBlockDto::ToolCall { .. }) {
                    continue;
                }
                let joined: String = parts
                    .iter()
                    .map(|(stream, delta)| format!("{}{delta}", stream_prefix(*stream)))
                    .collect();
                // 块本身还没有文本时，去掉首个输出自带的前导换行。
                let appended = if text_of(&target).is_empty() {
                    joined.strip_prefix('\n').unwrap_or(&joined).to_string()
                } else {
                    joined
                };
                if let ConversationBlockDto::ToolCall { text, .. } = &mut target {
                    text.push_str(&appended);
                    mutations.insert(index, target);
                }
            },
            Coalesced::Other(_) => {},
        }
    }

    if mutations.is_empty() {
        return false;
    }

    let mut ordered: Vec<(usize, ConversationBlockDto)> = mutations.into_iter().collect();
    ordered.sort_by_key(|(index, _)| *index);
    for (index, block) in ordered {
        blocks[index] = block;
    }
    true
}

/// 块的内容是否仍在到来，用于决定是否显示流式标记。
pub fn is_streaming(block: &ConversationBlockDto) -> bool {
    match block {
        ConversationBlockDto::Assistant { status, .. } => {
            matches!(status, ConversationBlockStatusDto::Streaming)
        },
        ConversationBlockDto::ToolCall { status, .. } => {
            matches!(
                status,
                astrcode_protocol::http::ToolCallStatusDto::Streaming
            )
        },
        _ => false,
    }
}
