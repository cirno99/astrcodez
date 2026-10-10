//! HTTP/SSE 入口使用的线缆 DTO。
//!
//! 这些类型只描述外部协议形状；server 负责把 storage read model 映射到这里，
//! storage 不依赖也不返回这些 DTO。

use astrcode_core::{message_attachment::MessageAttachment, tool::SessionToolSelection};
use serde::{Deserialize, Serialize};

use crate::wire::{
    ApprovalDecisionDto, ApprovalModeDto, ExtensionCapabilityDto, ExtensionHttpMethodDto,
    ExtensionSourceDto, ExtensionStageStatusDto, PhaseDto, ProviderAuthSchemeDto,
    ProviderWireFormatDto, ThinkingCapabilityDto, ToolOriginDto, ToolOutputStreamDto,
    impl_wire_values,
};
pub use crate::{
    agent_session_link::{AgentSessionLinkDto, AgentSessionStatusDto, AgentSessionUpdateDto},
    events::KeybindingDto,
};

/// 本机运行中 server 的发现文件（`run.json`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunInfoDto {
    pub port: u16,
}

/// 新建会话请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub working_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_selection: Option<ToolSelectionDto>,
}

/// 新建会话响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionResponseDto {
    pub session_id: String,
}

/// Session 工具可见性线缆契约。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolSelectionDto {
    /// 使用全部工具，但排除指定名称。
    All {
        #[serde(default)]
        except: Vec<String>,
    },
    /// 仅使用指定名称；空数组表示禁用全部工具。
    Only {
        #[serde(default)]
        names: Vec<String>,
    },
}

impl From<SessionToolSelection> for ToolSelectionDto {
    fn from(selection: SessionToolSelection) -> Self {
        match selection {
            SessionToolSelection::All { except } => Self::All { except },
            SessionToolSelection::Only { names } => Self::Only { names },
        }
    }
}

/// 配置 Session 后续 turn 工具边界的请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureSessionToolsRequest {
    pub selection: ToolSelectionDto,
}

/// 配置 Session 工具边界后的有效选择。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureSessionToolsResponse {
    pub selection: ToolSelectionDto,
}

/// Prompt 和 conversation block 共用的附件线缆形状。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptAttachmentDto {
    pub filename: String,
    pub content: String,
    pub media_type: String,
}

impl From<MessageAttachment> for PromptAttachmentDto {
    fn from(attachment: MessageAttachment) -> Self {
        Self {
            filename: attachment.filename,
            content: attachment.content,
            media_type: attachment.media_type,
        }
    }
}

impl From<PromptAttachmentDto> for MessageAttachment {
    fn from(attachment: PromptAttachmentDto) -> Self {
        Self {
            filename: attachment.filename,
            content: attachment.content,
            media_type: attachment.media_type,
        }
    }
}

/// 提交 prompt 或 mid-turn 注入请求（二者共用 `text` 字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<PromptAttachmentDto>,
}

/// 工具审批决议请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApprovalRequest {
    pub call_id: String,
    pub decision: ApprovalDecisionDto,
}

/// 当前挂起的核心工具审批。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApprovalDto {
    pub call_id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_key: Option<String>,
}

/// prompt 提交结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum PromptSubmitResponse {
    /// 已接受并异步执行。
    Accepted { session_id: String, turn_id: String },
    /// 请求已同步处理完成。
    Handled { session_id: String, message: String },
}

/// 手动 compact 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactSessionRequest {
    /// 保留最近 N 个完整 user turn group。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_recent_turns: Option<usize>,
}

/// 手动 compact 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactSessionResponse {
    pub compacted: bool,
    pub message: String,
}

/// 执行会话斜杠命令请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInvokeRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub arguments: String,
}

/// 执行会话斜杠命令响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum CommandInvokeResponse {
    Display {
        session_id: String,
        content: String,
        is_error: bool,
    },
    Handled {
        session_id: String,
        message: String,
    },
    Started {
        session_id: String,
        turn_id: String,
    },
}

/// 斜杠命令参数补全请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandCompletionRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub argument: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<usize>,
}

/// 斜杠命令参数补全响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandCompletionResponse {
    pub items: Vec<CommandCompletionItemDto>,
    pub truncated: bool,
}

/// 斜杠命令参数补全项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandCompletionItemDto {
    pub label: String,
    pub insert_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 斜杠命令列表响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommandListResponseDto {
    pub commands: Vec<SlashCommandInfoDto>,
    /// 插件注册的快捷键绑定。
    pub keybindings: Vec<KeybindingDto>,
    /// 插件注册的状态栏项（含初始值）。
    pub status_items: Vec<StatusItemDto>,
}

/// 状态栏项 DTO。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusItemDto {
    /// 唯一标识。
    pub id: String,
    /// 显示文本。
    pub text: String,
    /// 排序优先级。
    pub priority: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
}

/// 可执行斜杠命令信息（命令列表与扩展声明共用的全保真视图）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommandInfoDto {
    /// 命令名称（不含前导斜杠 `/`）。
    pub name: String,
    pub extension_id: String,
    pub description: String,
    /// 参数 JSON Schema。
    pub args_schema: Option<serde_json::Value>,
    /// 由 `args_schema` 派生的 UI 便利字段。
    pub needs_argument: bool,
    pub requires_idle: bool,
    pub argument_completions: bool,
    pub priority: i32,
    pub availability: crate::wire::CommandAvailabilityDto,
    pub execution: crate::wire::CommandExecutionDto,
}

/// 被遮蔽的命令诊断。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowedSlashCommandDto {
    pub name: String,
    pub active_extension_id: String,
    pub active_priority: i32,
    pub shadowed_extension_id: String,
    pub shadowed_priority: i32,
}

/// Fork a session from the latest durable event or an explicit durable sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSessionRequest {
    /// 可选来源 durable seq。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_seq: Option<u64>,
}

/// 会话列表项。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListItemDto {
    pub session_id: String,
    pub working_dir: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub phase: PhaseDto,
    /// 首条用户消息内容，无消息时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_user_message: Option<String>,
}

/// 会话列表响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListResponseDto {
    pub sessions: Vec<SessionListItemDto>,
}

/// conversation cursor。v1 中它是 snapshot 最新 durable seq 的十进制字符串。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCursorDto {
    pub value: String,
}

/// conversation 全量快照响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSnapshotResponseDto {
    pub session_id: String,
    pub session_title: String,
    pub cursor: ConversationCursorDto,
    pub control: ConversationControlStateDto,
    pub blocks: Vec<ConversationBlockDto>,
    pub agent_sessions: Vec<AgentSessionLinkDto>,
}

/// UI timeline 的不透明分页游标。客户端只负责原样回传。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTimelineCursorDto {
    pub value: String,
}

/// 与历史列表解耦的当前 conversation 状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationStateResponseDto {
    pub session_id: String,
    pub session_title: String,
    pub cursor: ConversationCursorDto,
    pub control: ConversationControlStateDto,
    pub transient_blocks: Vec<ConversationBlockDto>,
    pub agent_sessions: Vec<AgentSessionLinkDto>,
}

/// 一页只读 conversation timeline；新事件仍通过 SSE 增量发送。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationItemsPageResponseDto {
    pub items: Vec<ConversationBlockDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_cursor: Option<ConversationTimelineCursorDto>,
    pub has_older: bool,
    /// 生成本页时已提交的最新 durable event cursor。
    pub snapshot_cursor: ConversationCursorDto,
}

/// conversation 控制状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationControlStateDto {
    pub phase: PhaseDto,
    pub can_submit_prompt: bool,
    pub can_request_compact: bool,
    /// 活跃 turn ID，v1 snapshot 暂无。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    /// 当前 LLM HTTP 请求的瞬态重试状态；恢复或 turn 结束后清空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_status: Option<LlmRetryStatusDto>,
}

/// 会话累计的模型用量指标。
///
/// 这是 durable event log 的纯函数投影，客户端以最新值覆盖本地状态即可，无需增量累加。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMetricsDto {
    /// 上报过用量的模型请求数。
    pub requests: u64,
    /// 归一化后的完整 prompt token 数。
    pub prompt_tokens: u64,
    /// 其中命中缓存读取的 token 数。
    pub cached_tokens: u64,
    /// 写入缓存的 token 数（仅组成部分语义的 provider 会非零）。
    pub cache_creation_tokens: u64,
    /// 会话累计生成 token 数。
    pub output_tokens: u64,
    /// 其中推理 token 数。
    pub reasoning_output_tokens: u64,
    /// 最近一次模型请求归一化后的完整 prompt token 数；尚无请求时为空。
    ///
    /// 与 provider 账单里「输入」一栏同口径：单次请求重发的整段历史，不是累计值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt_tokens: Option<u64>,
    /// 最近一次模型请求中命中缓存读取的 token 数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cached_tokens: Option<u64>,
    /// 最近一次模型请求的生成 token 数；尚无请求或 provider 未上报时为空。
    ///
    /// 与 provider 账单里「输出」一栏同口径：单次请求的生成量，不是累计值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_tokens: Option<u64>,
    /// 最近一次模型请求生成 token 中的推理部分。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_output_tokens: Option<u64>,
    /// 最近一次响应结束后的上下文占用；上下文身份变化时为空。

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// 与 `context_tokens` 同一次上报的上下文窗口大小。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context_window: Option<u64>,
    /// 当前轮每秒生成 token 数；只统计模型请求时长，没有计时样本时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_per_second: Option<f64>,
}

/// LLM 请求的瞬态 HTTP 或传输重试状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmRetryStatusDto {
    /// HTTP 状态码；连接或响应流中断时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub attempt: u32,
    pub max_retries: u32,
    pub delay_ms: u64,
}

/// conversation 块。
// ToolCall 携带多个 serde_json::Value；Value 的表示大小随 feature 组合（如 preserve_order）变化，
// 在最宽的组合下会越过 large_enum_variant 阈值，而线缆格式与序列化行为不受影响。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum ConversationBlockDto {
    User {
        id: String,
        text: String,
        attachments: Vec<PromptAttachmentDto>,
    },
    Assistant {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        /// 该消息持久化后的 durable seq，可作为精确 fork 点。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        storage_seq: Option<u64>,
        status: ConversationBlockStatusDto,
    },
    ToolCall {
        id: String,
        name: String,
        /// LLM 对本次调用的参数（用于折叠摘要行显示）。
        arguments: String,
        /// 工具执行结果（展开后显示）。
        text: String,
        status: ToolCallStatusDto,
        /// 工具元数据（如 planContent、path 等），不进入 LLM 上下文。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
        /// 当前挂起的核心工具审批。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval: Option<ToolApprovalDto>,
        /// 原始 JSON 参数，供前端结构化解析（如 agent 工具的 task/agent 提取）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments_json: Option<serde_json::Value>,
    },
    Error {
        id: String,
        message: String,
    },
    Recap {
        id: String,
        text: String,
        source: String,
    },
    SystemNote {
        id: String,
        text: String,
    },
    CompactSummary {
        id: String,
        summary: String,
        trigger: String,
        pre_tokens: usize,
        post_tokens: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript_path: Option<String>,
    },
}

/// conversation 块状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversationBlockStatusDto {
    Streaming,
    Complete,
    Error,
}

impl_wire_values!(ConversationBlockStatusDto {
    Streaming,
    Complete,
    Error,
});

/// 工具调用生命周期状态。`Complete` 只表示调用正常结束；
/// 结果是否为错误属结果语义（见块文本/元数据），不体现在生命周期里。
/// `Failed` 表示执行基础设施失败，`Cancelled` 表示调用被取消。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallStatusDto {
    Streaming,
    Complete,
    Failed,
    Cancelled,
}

impl ToolCallStatusDto {
    pub const ALL: &'static [Self] = &[
        Self::Streaming,
        Self::Complete,
        Self::Failed,
        Self::Cancelled,
    ];
}

/// SSE 信封。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationStreamEnvelopeDto {
    pub session_id: String,
    pub cursor: ConversationCursorDto,
    pub delta: ConversationDeltaDto,
}

/// SSE conversation 增量。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum ConversationDeltaDto {
    AppendBlock {
        block: ConversationBlockDto,
    },
    /// 追加仅属于当前运行 turn 的临时 block；不会进入 durable snapshot。
    AppendTransientBlock {
        turn_id: String,
        block: ConversationBlockDto,
    },
    /// 清理指定 turn 尚未被 durable block 接管的临时内容。
    ClearTransientBlocks {
        turn_id: String,
    },
    PatchBlock {
        block_id: String,
        text_delta: String,
    },
    /// 丢弃失败流为该 assistant block 产生的临时文本与思考内容。
    ResetBlock {
        block_id: String,
    },
    /// 用持久化后的最终内容完成或补齐 block。
    FinalizeBlock {
        block: ConversationBlockDto,
    },
    UpdateControlState {
        control: ConversationControlStateDto,
    },
    /// 服务端检测到 receiver lag，客户端应重新拉全量 snapshot。
    RehydrateRequired,
    /// 更新 toolCall block 的 arguments 字段（用于折叠摘要行显示参数）。
    PatchArguments {
        block_id: String,
        arguments: String,
        /// 原始 JSON 参数，供前端结构化解析（如 agent 工具的 task/agent 提取）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments_json: Option<serde_json::Value>,
    },
    ToolOutput {
        call_id: String,
        stream: ToolOutputStreamDto,
        delta: String,
    },
    ThinkingDelta {
        block_id: String,
        delta: String,
    },
    /// Agent 子会话状态变更（新增 / 进行中刷新 / 完成 / 失败）。
    AgentSessionUpdated {
        agent_session: AgentSessionUpdateDto,
    },
    /// Agent 子会话已回收，前端应移除对应卡片。
    AgentSessionRemoved {
        child_session_id: String,
    },
    /// 插件状态栏项更新。
    StatusItemUpdate {
        id: String,
        text: String,
    },
    /// 扩展注册表发生变化，客户端应重新拉取命令/快捷键/状态栏快照。
    ExtensionRegistryChanged,
    /// 会话模型用量指标更新；携带绝对累计值，客户端直接覆盖本地状态。
    MetricsUpdated {
        metrics: ConversationMetricsDto,
    },
    ToolApprovalRequested {
        approval: ToolApprovalDto,
    },
    ToolApprovalResolved {
        call_id: String,
        decision: ApprovalDecisionDto,
    },
    /// 扩展发出的实时事件。客户端按 extension/event type 解释 payload。
    CustomEvent {
        extension_id: String,
        event_type: String,
        schema_version: u32,
        payload: serde_json::Value,
    },
}

/// HTTP 错误响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationErrorEnvelopeDto {
    pub code: String,
    pub message: String,
}

/// 删除项目响应（删除某工作目录下所有会话）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProjectResponseDto {
    pub deleted_count: usize,
}

// ── Config / Models DTOs ──

/// GET /api/config 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigViewResponseDto {
    pub config_path: String,
    pub active_profile: String,
    pub active_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_small_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_small_model: Option<String>,
    pub approval_mode: ApprovalModeDto,
    pub profiles: Vec<ProfileDto>,
    pub warning: Option<String>,
}

/// GET /api/extensions 响应中的单个扩展状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStateDto {
    pub extension_id: String,
    pub enabled: bool,
    pub loaded: bool,
    pub source: ExtensionSourceDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration: Option<ExtensionDeclarationDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<ExtensionDiagnosticsDto>,
}

/// 扩展可发射事件的声明。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomEventDeliveryDto {
    SessionDurable,
    SessionLive,
    GlobalLive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEventDeclarationDto {
    pub event_type: String,
    pub schema_version: u32,
    pub delivery: CustomEventDeliveryDto,
    pub max_payload_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CustomEventSourceFilterDto {
    Any,
    Extension { extension_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEventSubscriptionDto {
    pub id: String,
    pub event_type: String,
    pub source: CustomEventSourceFilterDto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomEventConsumerActionDto {
    Pause,
    Resume,
    ReplayFromBeginning,
    SkipToStreamHead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomEventConsumerControlRequest {
    pub extension_id: String,
    pub subscription_id: String,
    pub action: CustomEventConsumerActionDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEventConsumerStatusDto {
    pub extension_id: String,
    pub subscription: CustomEventSubscriptionDto,
    pub paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_head: Option<String>,
    pub pending_events: u64,
    pub in_flight: bool,
    pub failed_attempts: u64,
    pub consecutive_failures: u64,
    pub quarantined_events: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEventConsumerListResponseDto {
    pub consumers: Vec<CustomEventConsumerStatusDto>,
}

/// 扩展声明的完整描述。
///
/// 定位为开放 API 的自描述契约：除前端外，第三方调用方也可据此
/// 了解扩展提供的全部能力，因此各声明字段即使前端未消费也保留。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDeclarationDto {
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<ExtensionServiceDependencyDto>,
    #[serde(default)]
    pub service_permissions: Vec<String>,
    #[serde(default)]
    pub blocked_reasons: Vec<ExtensionServiceBlockDto>,
    pub id: String,
    pub capabilities: Vec<ExtensionCapabilityDto>,
    pub required_transport_features: Vec<TransportFeatureDto>,
    pub tools: Vec<ToolDefinitionDto>,
    pub dynamic_tools: bool,
    pub commands: Vec<SlashCommandInfoDto>,
    pub dynamic_commands: bool,
    pub keybindings: Vec<KeybindingDto>,
    pub status_items: Vec<StatusItemDto>,
    pub custom_events: Vec<CustomEventDeclarationDto>,
    pub custom_event_subscriptions: Vec<CustomEventSubscriptionDto>,
    pub http_routes: Vec<ExtensionHttpRouteDto>,
}

/// Host transport features required before an extension can be loaded.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportFeatureDto {
    AuthenticatedHttp,
}

/// 扩展注册的工具定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinitionDto {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub strict: bool,
    pub origin: ToolOriginDto,
}

impl From<astrcode_core::tool::ToolDefinition> for ToolDefinitionDto {
    fn from(value: astrcode_core::tool::ToolDefinition) -> Self {
        Self {
            name: value.name,
            description: value.description,
            parameters: value.parameters,
            strict: value.strict,
            origin: value.origin.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionHttpRouteDto {
    pub method: ExtensionHttpMethodDto,
    pub path: String,
    pub authenticated: bool,
    pub description: String,
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDiagnosticsDto {
    pub load: ExtensionStageDiagnosticsDto,
    pub register: ExtensionStageDiagnosticsDto,
    pub start: ExtensionStageDiagnosticsDto,
    pub hook_calls: u64,
    pub hook_timeouts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_hook: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStageDiagnosticsDto {
    pub status: ExtensionStageStatusDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// GET /api/extensions 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionListResponseDto {
    pub extensions: Vec<ExtensionStateDto>,
}

/// POST /api/extensions/reload 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionReloadResponseDto {
    pub reload_errors: Vec<String>,
}

/// POST /api/extensions/set-enabled 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetExtensionEnabledRequest {
    pub extension_id: String,
    pub enabled: bool,
}

/// POST /api/extensions/set-enabled 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetExtensionEnabledResponseDto {
    pub success: bool,
    pub reload_errors: Vec<String>,
}

/// 配置文件中的 Profile 信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDto {
    pub name: String,
    pub provider_kind: String,
    pub wire_format: ProviderWireFormatDto,
    pub auth_scheme: ProviderAuthSchemeDto,
    pub base_url: String,
    pub has_api_key: bool,
    pub models: Vec<ModelDto>,
}

/// GET /api/config/provider-catalog 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogResponseDto {
    pub providers: Vec<ProviderSpecDto>,
}

/// Provider catalog 中的单个 provider spec。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSpecDto {
    pub id: String,
    pub display_name: String,
    pub provider_kind: String,
    pub wire_format: ProviderWireFormatDto,
    pub auth_scheme: ProviderAuthSchemeDto,
    pub default_model: String,
    pub api_key_env_vars: Vec<String>,
    pub endpoints: Vec<ProviderEndpointPresetDto>,
    pub capabilities: ProviderSpecCapabilitiesDto,
}

/// Provider catalog 中的 endpoint preset。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEndpointPresetDto {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub is_default: bool,
}

/// Provider catalog 暴露给 UI 的能力摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSpecCapabilitiesDto {
    pub prompt_cache_key: bool,
    pub stream_usage: bool,
    pub reasoning_effort: bool,
    pub strict_tool_use: bool,
}

/// POST /api/config/provider-preset/apply 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyProviderPresetRequest {
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub activate: bool,
}

const fn is_false(value: &bool) -> bool {
    !*value
}

/// POST /api/config/provider-preset/apply 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyProviderPresetResponseDto {
    pub success: bool,
    pub profile_name: String,
    pub model_id: String,
    pub activated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// POST /api/config/provider-preset/remove 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveProviderPresetRequest {
    pub profile_name: String,
}

/// POST /api/config/provider-preset/remove 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveProviderPresetResponseDto {
    pub success: bool,
    pub removed_profile_name: String,
    pub active_profile: String,
    pub active_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// 标准化 thinking 配置 DTO（映射 core::llm::thinking::ThinkingConfig）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfigDto {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

impl From<astrcode_core::llm::thinking::ThinkingConfig> for ThinkingConfigDto {
    fn from(value: astrcode_core::llm::thinking::ThinkingConfig) -> Self {
        Self {
            enabled: value.enabled,
            effort: value.effort,
            budget_tokens: value.budget_tokens,
        }
    }
}

impl From<ThinkingConfigDto> for astrcode_core::llm::thinking::ThinkingConfig {
    fn from(value: ThinkingConfigDto) -> Self {
        Self {
            enabled: value.enabled,
            effort: value.effort,
            budget_tokens: value.budget_tokens,
        }
    }
}

/// Profile 中的模型选项（与 config.toml 的 `modelOptions` 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOptionsDto {
    /// 标准化 thinking 配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfigDto>,
}

/// Profile 中的模型信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDto {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_options: Option<ModelOptionsDto>,
    /// 当前模型的标准化 thinking 配置（归一化后）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfigDto>,
    /// 当前模型已解析的 thinking 能力（显式覆盖优先，其次内置查找）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_capability: Option<ThinkingCapabilityDto>,
}

/// POST /api/config/model-options 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateModelOptionsRequest {
    pub profile_name: String,
    pub model_id: String,
    /// 可为 null 或缺失以恢复默认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfigDto>,
}

/// POST /api/config/model-options 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateModelOptionsResponseDto {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// POST /api/config/active-selection 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateActiveSelectionRequest {
    pub active_profile: String,
    pub active_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_small_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_small_model: Option<String>,
    pub approval_mode: ApprovalModeDto,
}

/// POST /api/config/active-selection 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateActiveSelectionResponseDto {
    pub success: bool,
    pub warning: Option<String>,
}

/// POST /api/config/reload 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReloadResponseDto {
    pub active_profile: String,
    pub active_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_small_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_small_model: Option<String>,
}

/// GET /api/models/current 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentModelResponseDto {
    pub profile_name: String,
    pub model_id: String,
    pub provider_kind: String,
    pub wire_format: ProviderWireFormatDto,
}

/// GET /api/models 响应中的单个模型。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModelDto {
    pub profile_name: String,
    pub model_id: String,
    pub provider_kind: String,
    pub wire_format: ProviderWireFormatDto,
}

/// GET /api/models 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponseDto {
    pub models: Vec<AvailableModelDto>,
}

/// POST /api/models/test 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelTestResponseDto {
    pub success: bool,
    pub message: String,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionServiceDependencyDto {
    pub service: String,
    pub kind: ExtensionDependencyKindDto,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionDependencyKindDto {
    Required,
    Optional,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionServiceBlockDto {
    MissingService {
        service: String,
    },
    ProviderConflict {
        service: String,
        providers: Vec<String>,
    },
    DependencyCycle {
        members: Vec<String>,
    },
    DependencyBlocked {
        provider: String,
    },
}

/// GET /api/preferences 与 PUT /api/preferences 的响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferencesResponseDto {
    /// 服务端是否已存有偏好。
    ///
    /// 为假时界面才把旧 localStorage 迁移上来；迁移是一次性的，一旦存过就绝不再迁，
    /// 否则清空偏好后旧值会自己回来。
    pub stored: bool,
    pub sidebar_width: f64,
    pub collapsed_project_dirs: Vec<String>,
    pub kanban_project_paths: Vec<String>,
    pub kanban_ignored_project_paths: Vec<String>,
}

/// PUT /api/preferences 请求：整份替换。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUiPreferencesRequest {
    pub sidebar_width: f64,
    pub collapsed_project_dirs: Vec<String>,
    pub kanban_project_paths: Vec<String>,
    pub kanban_ignored_project_paths: Vec<String>,
}

/// 代码浏览：目录里的一项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileEntryDto {
    /// 名字，不含路径分隔符。
    pub name: String,
    /// 相对浏览根目录的路径，分隔符统一为 `/`。
    pub path: String,
    pub is_dir: bool,
}

/// GET /api/files/tree 的响应：被列举目录下的一层条目。
///
/// 只给一层：浏览器按展开动作逐层取，服务端因此不必递归一个可能很大的仓库。
/// 条目已排好序（目录在前，同类按名字），两个宿主不必各自再排一遍。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeResponseDto {
    /// 被列举目录相对根目录的路径；根目录为空串。
    pub path: String,
    pub entries: Vec<FileEntryDto>,
}

/// GET /api/files/content 的响应。
///
/// `binary` 为真时 `text` 为空、`total_lines` 为 0：文件不是 UTF-8 文本，无从按代码展示。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileContentResponseDto {
    pub path: String,
    pub size_bytes: u64,
    pub binary: bool,
    /// 文件超出单次读取上限，`text` 只是前面的若干行。
    pub truncated: bool,
    /// 整个文件的行数；`truncated` 为真时它大于 `text` 的行数。
    pub total_lines: usize,
    pub text: String,
}

/// 工作区文件相对 git HEAD 的状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FileChangeStateDto {
    /// 已被 git 跟踪，且与 HEAD 不同。
    Modified,
    /// 未被跟踪的新文件，整份内容都算新增。
    Untracked,
    /// 已被跟踪且与 HEAD 一致。
    Unchanged,
    /// 文件不是 UTF-8 文本，无法按行比较。
    Binary,
    /// 根目录不在 git 工作树里。
    NotARepository,
    /// 找不到可用的 `git` 命令。
    GitUnavailable,
}

/// GET /api/files/diff 的响应：文件相对 git HEAD 的未提交改动。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileDiffResponseDto {
    pub path: String,
    pub state: FileChangeStateDto,
    /// 统一 diff 文本；只有 `Modified` 与 `Untracked` 会带正文。
    pub unified_diff: String,
    /// HEAD 侧的正文，供界面逐行对齐；HEAD 里没有这个路径（新文件）时为空串。
    pub original: String,
    /// `original` 超出单次返回上限，已被截断。
    pub original_truncated: bool,
    pub insertions: usize,
    pub deletions: usize,
    /// diff 超出单次返回上限，`unified_diff` 被截断。
    pub truncated: bool,
}

/// 工作区清单里的一项相对 git HEAD 的处境。
///
/// 与逐文件的 [`FileChangeStateDto`] 分开：这里只描述清单里的条目，没有「读不出内容」这类
/// 单文件才有的处境，却多了「被删掉」「换了路径」这些清单才看得见的处境。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitStatusEntryStateDto {
    /// 已被跟踪，且与 HEAD 不同。
    Modified,
    /// 相对 HEAD 是新出现的（索引里已暂存的新文件）。
    Added,
    /// 已被跟踪，但工作区里已经没有它。
    Deleted,
    /// 相对 HEAD 换了路径；清单里按新路径给出。
    Renamed,
    /// 未被跟踪的新文件，整份内容都算新增。
    Untracked,
    /// 合并冲突：工作区里的内容不是任何一方的定稿。
    Conflicted,
}

/// 工作区清单能否取到。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GitStatusAvailabilityDto {
    /// 拿到了清单；`entries` 为空表示工作区没有未提交的改动。
    Available,
    /// 根目录不在 git 工作树里。
    NotARepository,
    /// 找不到可用的 `git` 命令，或命令执行失败。
    GitUnavailable,
}

/// 工作区清单里的一项改动。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusEntryDto {
    /// 相对浏览根目录的路径，分隔符统一为 `/`。
    pub path: String,
    pub state: GitStatusEntryStateDto,
}

/// GET /api/files/status 的响应：整个工作区相对 git HEAD 的未提交改动清单。
///
/// 条目顺序就是 git 自己的输出顺序（被跟踪的改动在前、未跟踪的在后），两个宿主不必各自
/// 再排一遍；`Renamed` 只带新路径，原路径在清单里没有可点的动作，因此不上线缆。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusResponseDto {
    pub availability: GitStatusAvailabilityDto,
    pub entries: Vec<GitStatusEntryDto>,
    /// 改动条目超出单次返回上限，`entries` 只是前面的一部分。
    pub truncated: bool,
}

/// GET /api/files/search 的响应：整个工作区里匹配查询串的位置。
///
/// 按文件分组，命中在文件内按行号升序——界面按文件列出一组命中，同一份文件的路径因此只上
/// 一次线缆。文件的先后是遍历顺序（同一目录内按名字排），同一次搜索的结果是稳定的。
/// `truncated` 为真表示命中或文件数触到了上限，后面还有没返回的部分。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchResponseDto {
    pub files: Vec<FileSearchFileDto>,
    pub truncated: bool,
}

/// 一份文件里的全部命中。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchFileDto {
    /// 相对浏览根目录的路径，分隔符统一为 `/`。
    pub path: String,
    pub matches: Vec<FileSearchMatchDto>,
}

/// 一处命中。
///
/// `text` 是命中所在行；整行太长时只给命中附近的一段，`column` 因此是命中相对**返回的这一段**
/// 的字节偏移：界面拿 `column` 与查询串长度直接画高亮，不必自己再找一遍。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchMatchDto {
    /// 行号，从 1 起。
    pub line: usize,
    pub column: usize,
    pub text: String,
}

#[cfg(test)]
mod tests;
