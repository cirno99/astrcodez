//! 会话状态层：流生命周期与增量应用。
//!
//! 这一层不依赖窗口与 GPU，因此可以脱离 gpui 测试（ADR 0001 第 13 轮）。
//!
//! 与 Web 前端的对应关系：
//! - `reduceConversationDeltas` → [`ConversationState::apply_batch`]，保留控制态同值
//!   判断（避免未变化的控制态反复打扰 UI）与 transient block 的归属语义。
//! - `coalesceDeltas` / `applyCoalescedDeltas` → [`delta`]，保留孤儿 patch 不变式。
//! - `frameBuffer` 的按帧冲刷不重建，改由 gpui 实体通知节奏驱动。

use std::collections::HashMap;

use astrcode_protocol::http::{
    AgentSessionLinkDto, AgentSessionUpdateDto, ConversationBlockDto, ConversationControlStateDto,
    ConversationDeltaDto, ConversationMetricsDto, ToolApprovalDto,
};

use crate::agent_session::{self, AgentSession};

pub mod delta;

use delta::Coalesced;
pub use delta::coalesce;

/// 会话的可渲染状态。
#[derive(Debug, Default)]
pub struct ConversationState {
    blocks: Vec<ConversationBlockDto>,
    /// transient block 的归属：block id → 拥有它的 turn id。
    transient_owners: HashMap<String, String>,
    control: Option<ConversationControlStateDto>,
    cursor: Option<String>,
    metrics: Option<ConversationMetricsDto>,
    /// 当前挂起的审批所属的工具调用。
    pending_approval: Option<String>,
    /// 子 Agent 会话链接；快照建基线，增量按 `child_session_id` 归并。
    agent_sessions: Vec<AgentSession>,
    /// 块列表的修订号，任何块变更都会递增。
    blocks_revision: u64,
    /// 控制态的修订号，仅在控制态实际变化时递增。
    control_revision: u64,
}

impl ConversationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn blocks(&self) -> &[ConversationBlockDto] {
        &self.blocks
    }

    pub fn control(&self) -> Option<&ConversationControlStateDto> {
        self.control.as_ref()
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    pub fn metrics(&self) -> Option<&ConversationMetricsDto> {
        self.metrics.as_ref()
    }

    /// 当前挂起审批的工具调用 id。
    pub fn pending_approval(&self) -> Option<&str> {
        self.pending_approval.as_deref()
    }

    /// 某个工具调用派生的子 Agent 会话；没有则 `None`。
    ///
    /// 卡片靠它挂回工具行，对应前端 `state.agentSessions.find(agent => agent.toolCallId ===
    /// block.id)`。
    pub(crate) fn agent_session_for_tool_call(&self, call_id: &str) -> Option<&AgentSession> {
        self.agent_sessions
            .iter()
            .find(|session| session.tool_call_id.as_deref() == Some(call_id))
    }

    /// 块列表修订号。视图用它判断是否需要重建消息列表。
    pub fn blocks_revision(&self) -> u64 {
        self.blocks_revision
    }

    /// 控制态修订号。
    pub fn control_revision(&self) -> u64 {
        self.control_revision
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 用服务端快照整体替换本地状态。
    pub fn reset(
        &mut self,
        blocks: Vec<ConversationBlockDto>,
        agent_sessions: Vec<AgentSessionLinkDto>,
        control: ConversationControlStateDto,
        cursor: &str,
    ) {
        self.blocks = blocks;
        self.transient_owners.clear();
        self.control = Some(control);
        self.cursor = Some(cursor.to_string());
        self.pending_approval = None;
        self.agent_sessions = agent_sessions.iter().map(AgentSession::from_link).collect();
        self.blocks_revision += 1;
        self.control_revision += 1;
    }

    /// 应用一个通知周期内累积的全部 delta。
    pub fn apply_batch(&mut self, deltas: &[ConversationDeltaDto], cursor: Option<&str>) {
        let coalesced = delta::coalesce(deltas);
        let mut blocks_changed = delta::apply_block_deltas(&mut self.blocks, &coalesced);

        for item in &coalesced {
            let Coalesced::Other(application) = item else {
                continue;
            };
            match &**application {
                ConversationDeltaDto::AppendBlock { block } => {
                    blocks_changed |= self.promote(block.clone());
                },
                ConversationDeltaDto::AppendTransientBlock { turn_id, block } => {
                    let id = delta::block_id(block).to_string();
                    blocks_changed |= self.upsert(block.clone());
                    if self.transient_owners.insert(id, turn_id.clone()).is_none() {
                        blocks_changed = true;
                    }
                },
                ConversationDeltaDto::ClearTransientBlocks { turn_id } => {
                    blocks_changed |= self.clear_transient(turn_id);
                },
                ConversationDeltaDto::FinalizeBlock { block } => {
                    blocks_changed |= self.promote(block.clone());
                },
                ConversationDeltaDto::ResetBlock { block_id } => {
                    blocks_changed |= self.reset_block(block_id);
                },
                ConversationDeltaDto::UpdateControlState { control } => {
                    if !same_control_state(self.control.as_ref(), control) {
                        self.control = Some(control.clone());
                        self.control_revision += 1;
                    }
                },
                ConversationDeltaDto::ToolApprovalRequested { approval } => {
                    self.pending_approval = Some(approval.call_id.clone());
                    blocks_changed |= self.set_approval(&approval.call_id, Some(approval.clone()));
                },
                ConversationDeltaDto::ToolApprovalResolved { call_id, .. } => {
                    if self.pending_approval.as_deref() == Some(call_id.as_str()) {
                        self.pending_approval = None;
                    }
                    blocks_changed |= self.set_approval(call_id, None);
                },
                ConversationDeltaDto::MetricsUpdated { metrics } => {
                    self.metrics = Some(metrics.clone());
                },
                ConversationDeltaDto::AgentSessionUpdated { agent_session } => {
                    self.upsert_agent_session(agent_session);
                },
                ConversationDeltaDto::AgentSessionRemoved { child_session_id } => {
                    self.agent_sessions
                        .retain(|session| session.child_session_id != *child_session_id);
                },
                // 已由 coalesce 归并进块级变更，不再单独处理。
                ConversationDeltaDto::PatchBlock { .. }
                | ConversationDeltaDto::ThinkingDelta { .. }
                | ConversationDeltaDto::PatchArguments { .. }
                | ConversationDeltaDto::ToolOutput { .. } => {},
                // Rehydrate 由流生命周期接管；其余增量本片尚未承接。
                _ => {},
            }
        }

        if blocks_changed {
            self.blocks_revision += 1;
        }
        if let Some(cursor) = cursor {
            self.cursor = Some(cursor.to_string());
        }
    }

    /// 写入一条 durable block，并撤销它的 transient 归属。
    fn promote(&mut self, block: ConversationBlockDto) -> bool {
        let changed = self.upsert(block.clone());
        let id = delta::block_id(&block);
        changed | self.transient_owners.remove(id).is_some()
    }

    fn upsert(&mut self, incoming: ConversationBlockDto) -> bool {
        let id = delta::block_id(&incoming).to_string();
        match self
            .blocks
            .iter_mut()
            .find(|block| delta::block_id(block) == id)
        {
            Some(current) => {
                *current = delta::merge_block(current.clone(), incoming);
                true
            },
            None => {
                // compactSummary 是整篇替换语义：新的一份到达时丢掉旧的。
                if matches!(incoming, ConversationBlockDto::CompactSummary { .. }) {
                    self.blocks.retain(|block| {
                        !matches!(block, ConversationBlockDto::CompactSummary { .. })
                    });
                }
                self.blocks.push(incoming);
                true
            },
        }
    }

    fn clear_transient(&mut self, turn_id: &str) -> bool {
        let owned: Vec<String> = self
            .transient_owners
            .iter()
            .filter(|(_, owner)| owner.as_str() == turn_id)
            .map(|(block_id, _)| block_id.clone())
            .collect();
        if owned.is_empty() {
            return false;
        }
        self.blocks
            .retain(|block| !owned.iter().any(|id| id == delta::block_id(block)));
        for block_id in owned {
            self.transient_owners.remove(&block_id);
        }
        true
    }

    /// 丢弃失败流为 assistant block 产生的临时文本与思考内容。
    fn reset_block(&mut self, block_id: &str) -> bool {
        let mut changed = false;
        for block in &mut self.blocks {
            if let ConversationBlockDto::Assistant {
                id,
                text,
                reasoning_content,
                ..
            } = block
                && id == block_id
            {
                changed = true;
                text.clear();
                *reasoning_content = None;
            }
        }
        changed
    }

    fn set_approval(&mut self, call_id: &str, approval: Option<ToolApprovalDto>) -> bool {
        let mut changed = false;
        for block in &mut self.blocks {
            if let ConversationBlockDto::ToolCall {
                id, approval: slot, ..
            } = block
                && id == call_id
            {
                changed = true;
                *slot = approval.clone();
            }
        }
        changed
    }

    /// 按 `child_session_id` 归并一条子会话增量。
    ///
    /// 同值时不动：子会话的阶段会随每个步骤上报，而卡片只画变了的那几项。
    fn upsert_agent_session(&mut self, update: &AgentSessionUpdateDto) {
        let id = agent_session::updated_child_session_id(update);
        let index = self
            .agent_sessions
            .iter()
            .position(|session| session.child_session_id == id);
        let current = index.map(|index| self.agent_sessions[index].clone());
        let Some(next) = agent_session::apply_update(current.as_ref(), update) else {
            return;
        };
        match index {
            Some(index) if self.agent_sessions[index] == next => {},
            Some(index) => self.agent_sessions[index] = next,
            None => self.agent_sessions.push(next),
        }
    }
}

/// 控制态同值判断。
///
/// 重试状态每次上报都会随 SSE 到达，逐字段比较可以避免为未变化的控制态重渲染。
fn same_control_state(
    current: Option<&ConversationControlStateDto>,
    incoming: &ConversationControlStateDto,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    let retry = |control: &ConversationControlStateDto| {
        control.retry_status.as_ref().map(|retry| {
            (
                retry.status,
                retry.attempt,
                retry.max_retries,
                retry.delay_ms,
            )
        })
    };
    current.phase == incoming.phase
        && current.can_submit_prompt == incoming.can_submit_prompt
        && current.can_request_compact == incoming.can_request_compact
        && current.active_turn_id == incoming.active_turn_id
        && retry(current) == retry(incoming)
}
/// 通知周期的 delta 缓冲。
///
/// Web 前端靠 `requestAnimationFrame` 每帧冲刷 `frameBuffer`；gpui 已经有实体通知
/// 节奏，所以这里只保留缓冲与内存上限（ADR 0001 第 13 轮），不再自己按帧驱动。
///
/// 上限是硬性的：超过上限时立刻冲刷，宁可多一次渲染也不让缓冲无界增长。
pub struct DeltaBuffer {
    pending: Vec<ConversationDeltaDto>,
    /// 累计的文本字节数，用于第二个上限。
    pending_text_bytes: usize,
    cursor: Option<String>,
}

/// 单个通知周期内最多累积的 delta 条数。
pub const MAX_BUFFERED_DELTAS: usize = 1024;
/// 单个通知周期内最多累积的增量文本字节数。
pub const MAX_BUFFERED_TEXT_BYTES: usize = 256 * 1024;

impl Default for DeltaBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl DeltaBuffer {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            pending_text_bytes: 0,
            cursor: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.cursor.is_none()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// 推入一条 delta；返回 `true` 表示已触及上限、应当立即冲刷。
    pub fn push(&mut self, delta: &ConversationDeltaDto, cursor: Option<&str>) -> bool {
        // patch 与 thinking 是高频小片段，只有它们计入文本字节上限。
        self.pending_text_bytes += delta_text_len(delta);
        self.pending.push(delta.clone());
        if let Some(cursor) = cursor {
            self.cursor = Some(cursor.to_string());
        }
        self.pending.len() >= MAX_BUFFERED_DELTAS
            || self.pending_text_bytes >= MAX_BUFFERED_TEXT_BYTES
    }

    /// 取出缓冲内容，并重置上限计数。
    pub fn take(&mut self) -> (Vec<ConversationDeltaDto>, Option<String>) {
        self.pending_text_bytes = 0;
        (std::mem::take(&mut self.pending), self.cursor.take())
    }
}

/// 单条 delta 携带的增量文本长度，用于内存上限。
fn delta_text_len(delta: &ConversationDeltaDto) -> usize {
    match delta {
        ConversationDeltaDto::PatchBlock { text_delta, .. } => text_delta.len(),
        ConversationDeltaDto::ThinkingDelta { delta, .. } => delta.len(),
        ConversationDeltaDto::PatchArguments { arguments, .. } => arguments.len(),
        ConversationDeltaDto::ToolOutput { delta, .. } => delta.len(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::http::{
        ConversationBlockDto, ConversationDeltaDto, ConversationStreamEnvelopeDto,
    };

    use super::{ConversationState, DeltaBuffer, MAX_BUFFERED_DELTAS};

    fn assistant(id: &str, text: &str, streaming: bool) -> ConversationBlockDto {
        use astrcode_protocol::http::ConversationBlockStatusDto;
        ConversationBlockDto::Assistant {
            id: id.to_string(),
            text: text.to_string(),
            reasoning_content: None,
            storage_seq: None,
            status: if streaming {
                ConversationBlockStatusDto::Streaming
            } else {
                ConversationBlockStatusDto::Complete
            },
        }
    }

    #[test]
    fn orphan_patch_must_not_manufacture_a_block() {
        let mut state = ConversationState::new();
        state.apply_batch(
            &[ConversationDeltaDto::PatchBlock {
                block_id: "missing".into(),
                text_delta: "ghost".into(),
            }],
            None,
        );
        assert!(state.blocks().is_empty());
    }

    #[test]
    fn adjacent_patches_are_joined_in_one_batch() {
        let mut state = ConversationState::new();
        state.apply_batch(
            &[ConversationDeltaDto::AppendBlock {
                block: assistant("a", "", true),
            }],
            None,
        );
        state.apply_batch(
            &[
                ConversationDeltaDto::PatchBlock {
                    block_id: "a".into(),
                    text_delta: "你好".into(),
                },
                ConversationDeltaDto::PatchBlock {
                    block_id: "a".into(),
                    text_delta: "世界".into(),
                },
            ],
            Some("2"),
        );
        let ConversationBlockDto::Assistant { text, .. } = &state.blocks()[0] else {
            panic!("expected an assistant block");
        };
        assert_eq!(text, "你好世界");
        assert_eq!(state.cursor(), Some("2"));
    }

    #[test]
    fn finalize_falls_back_to_previous_text_when_incoming_is_empty() {
        let mut state = ConversationState::new();
        state.apply_batch(
            &[ConversationDeltaDto::AppendBlock {
                block: assistant("a", "streamed", true),
            }],
            None,
        );
        state.apply_batch(
            &[ConversationDeltaDto::FinalizeBlock {
                block: assistant("a", "", false),
            }],
            None,
        );
        let ConversationBlockDto::Assistant { text, status, .. } = &state.blocks()[0] else {
            panic!("expected an assistant block");
        };
        assert_eq!(text, "streamed", "空内容不得覆盖已有文本");
        assert!(!super::delta::is_streaming(&state.blocks()[0]));
        let _ = status;
    }

    #[test]
    fn transient_blocks_are_dropped_with_their_turn() {
        let mut state = ConversationState::new();
        state.apply_batch(
            &[ConversationDeltaDto::AppendTransientBlock {
                turn_id: "t1".into(),
                block: assistant("tmp", "partial", true),
            }],
            None,
        );
        assert_eq!(state.blocks().len(), 1);
        state.apply_batch(
            &[ConversationDeltaDto::ClearTransientBlocks {
                turn_id: "t1".into(),
            }],
            None,
        );
        assert!(state.blocks().is_empty());
    }

    #[test]
    fn durable_append_promotes_a_transient_block_instead_of_duplicating_it() {
        let mut state = ConversationState::new();
        state.apply_batch(
            &[ConversationDeltaDto::AppendTransientBlock {
                turn_id: "t1".into(),
                block: assistant("a", "partial", true),
            }],
            None,
        );
        state.apply_batch(
            &[ConversationDeltaDto::FinalizeBlock {
                block: assistant("a", "final", false),
            }],
            None,
        );
        // 提升后再清 turn 不得把已经 durable 的块删掉。
        state.apply_batch(
            &[ConversationDeltaDto::ClearTransientBlocks {
                turn_id: "t1".into(),
            }],
            None,
        );
        assert_eq!(state.blocks().len(), 1);
    }

    #[test]
    fn reset_block_clears_streaming_text() {
        let mut state = ConversationState::new();
        state.apply_batch(
            &[ConversationDeltaDto::AppendBlock {
                block: assistant("a", "broken", true),
            }],
            None,
        );
        state.apply_batch(
            &[ConversationDeltaDto::ResetBlock {
                block_id: "a".into(),
            }],
            None,
        );
        let ConversationBlockDto::Assistant { text, .. } = &state.blocks()[0] else {
            panic!("expected an assistant block");
        };
        assert!(text.is_empty());
    }

    #[test]
    fn buffer_flushes_at_the_delta_cap() {
        let mut buffer = DeltaBuffer::new();
        let delta = ConversationDeltaDto::PatchBlock {
            block_id: "a".into(),
            text_delta: "x".into(),
        };
        for index in 0..MAX_BUFFERED_DELTAS {
            let should_flush = buffer.push(&delta, None);
            let last = index + 1 == MAX_BUFFERED_DELTAS;
            assert_eq!(should_flush, last, "在第 {index} 条上判断错误");
        }
    }

    #[test]
    fn buffer_drops_the_flag_after_take() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(
            &ConversationDeltaDto::PatchBlock {
                block_id: "a".into(),
                text_delta: "x".into(),
            },
            Some("7"),
        );
        let (deltas, cursor) = buffer.take();
        assert_eq!(deltas.len(), 1);
        assert_eq!(cursor.as_deref(), Some("7"));
        assert!(buffer.is_empty());
    }

    /// 信封形状的回归测试：解析失败会静默丢事件，因为它只在流循环里被忽略。
    #[test]
    fn stream_envelope_decodes_from_the_wire_shape() {
        let json = r#"{"sessionId":"s1","cursor":{"value":"3"},"delta":{"kind":"patchBlock","blockId":"a","textDelta":"hi"}}"#;
        let envelope: ConversationStreamEnvelopeDto = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.cursor.value, "3");
        assert!(matches!(
            envelope.delta,
            ConversationDeltaDto::PatchBlock { ref block_id, .. } if block_id == "a"
        ));
    }
}
