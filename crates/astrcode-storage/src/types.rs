use astrcode_core::llm::LlmMessage;
use serde::{Deserialize, Serialize};

/// 一次 compact 落盘的被取代上下文。
///
/// - `provider_messages` 只含本次压缩要取代的前缀：保留区此刻仍逐字在上下文里，
///   再抄一份会让每份快照按窗口大小重复膨胀。
/// - 消息按 provider 原文落盘、不脱敏：同一批字节已由 durable journal 明文保存，
///   只脱敏快照不减少暴露，只会让它无法还原被摘要取代的原文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactSnapshotInput {
    pub trigger: String,
    pub model_id: String,
    pub working_dir: String,
    pub system_prompt: Option<String>,
    pub provider_messages: Vec<LlmMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultArtifactInput {
    pub call_id: String,
    pub tool_name: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResultArtifactRef {
    pub bytes: usize,
    pub artifact_id: String,
}
