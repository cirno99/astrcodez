//! 助手回合分组，对应 Web 前端的 `Chat/assistantRunModel.ts`。
//!
//! 转录不是扁平块列表：连续的助手块（assistant / toolCall）合并成一次回合，回合内按
//! 「有可见正文就断开」切段——思考与工具调用进 [`RunSegment::Process`]（渲染成一行可折叠
//! 的摘要），可见正文单独成 [`RunSegment::Content`]。其余块各自成行。
//!
//! 与前端有一处刻意差异：`buildMessageListItems` 的 32 块分片是 React 虚拟列表的行粒度，
//! gpui 侧没有对应物。
//!
//! 纯推导、不碰 gpui：渲染在 `views::chat`。

use astrcode_protocol::http::{
    ConversationBlockDto, ConversationBlockStatusDto, ToolCallStatusDto,
};
use serde_json::Value;

use crate::{
    ask_user,
    conversation::delta::block_id,
    tool_view::{ToolActivity, activity_for, duration_label, duration_seconds},
};
/// 转录列表的一项。
pub(crate) enum TranscriptItem<'a> {
    /// 单独成行的块：用户消息、错误、压缩摘要等。
    Block(&'a ConversationBlockDto),
    /// 一次连续的助手回合，按段渲染。
    Run(Run<'a>),
}

/// 一次助手回合：切好的段，外加回合级动作。
pub(crate) struct Run<'a> {
    /// 元素 id 用的键，取自回合首块的 id：同一会话里每段回合都要唯一。
    pub(crate) key: String,
    pub(crate) segments: Vec<RunSegment<'a>>,
    /// 回合级的两个动作；收尾不是完稿正文时为 `None`。
    pub(crate) actions: Option<RunActions>,
    /// 组成这一回合的块，顺序与转录一致。
    ///
    /// 段与动作都是它们的推导结果，而查找要的是原始文本：思考的正文在块上，不在段里。
    pub(crate) blocks: &'a [ConversationBlockDto],
}

/// 「复制此 Turn」与「从此 Turn 分叉」的输入。
pub(crate) struct RunActions {
    /// 回合内助手块的可见正文，按空行连接。
    pub(crate) copy_text: String,
    /// 从此 Turn 分叉的持久化点；块里没有 durable seq 时只能复制，不能分叉。
    pub(crate) fork_at: Option<u64>,
}

impl<'a> Run<'a> {
    /// 从一段连续的助手块推导回合；`key` 见 [`Run::key`]。
    pub(crate) fn new(key: &str, blocks: &'a [ConversationBlockDto]) -> Self {
        Self {
            key: key.to_owned(),
            segments: run_segments(blocks),
            actions: run_actions(blocks),
            blocks,
        }
    }
}

/// 回合内的一段。
pub(crate) enum RunSegment<'a> {
    /// 处理过程：思考与工具调用，折叠显示。
    Process(ProcessSegment<'a>),
    /// 助手可见正文。
    Content(&'a ConversationBlockDto),
}

/// 一段处理过程。
pub(crate) struct ProcessSegment<'a> {
    /// 折叠态的键，取自收拢时的首条条目；流式更新中同一条目 id 不变，折叠态因此不会丢。
    pub(crate) id: String,
    pub(crate) entries: Vec<ProcessEntry<'a>>,
    /// 段内待回答的 askUser 问卷。
    ///
    /// 它们不算过程条目：卡片要渲染在折叠区**外**（见 `views::chat`），收起来等于没问；
    /// 作答完成后块回到 `entries` 里当普通工具行。
    pub(crate) prompts: Vec<&'a ConversationBlockDto>,
    /// 段内工具耗时合计（秒）。
    pub(crate) duration_seconds: f64,
    pub(crate) has_streaming_work: bool,
    /// 段内有待决定的事：折叠区必须保持展开，否则审批按钮会被藏起来。
    pub(crate) has_attention: bool,
}

/// 处理过程里的一条。
pub(crate) enum ProcessEntry<'a> {
    /// 一段思考。
    Thinking {
        /// markdown 状态的键，见 [`thinking_key`]。
        key: String,
        streaming: bool,
    },

    /// 一次工具调用。
    Tool(ToolActivity<'a>),
}

impl ProcessEntry<'_> {
    /// 条目的稳定 id：段落折叠键由它派生。
    fn id(&self) -> &str {
        match self {
            Self::Thinking { key, .. } => key,
            Self::Tool(activity) => block_id(activity.block),
        }
    }
}

impl ProcessSegment<'_> {
    /// 摘要行主文案：还在跑就是「处理中」，跑完带上耗时。
    pub(crate) fn title(&self) -> String {
        if self.has_streaming_work {
            return "处理中".to_owned();
        }
        let duration = format_run_duration(self.duration_seconds);
        if duration.is_empty() {
            "已处理".to_owned()
        } else {
            format!("已处理 {duration}")
        }
    }

    /// 摘要行右侧的最近一条活动：收起时靠它交代这段在做什么。
    pub(crate) fn latest_label(&self) -> String {
        match self.entries.last() {
            Some(ProcessEntry::Tool(activity)) => {
                format!("{} {}", activity.title, activity.label)
            },
            Some(ProcessEntry::Thinking { streaming, .. }) => {
                if *streaming {
                    "正在思考".to_owned()
                } else {
                    "思考过程".to_owned()
                }
            },
            None => String::new(),
        }
    }

    /// 段内是否有失败的调用：有则摘要行整行标红。
    pub(crate) fn has_error(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, ProcessEntry::Tool(activity) if activity_failed(activity)))
    }
}

/// 这次调用是否失败。
pub(crate) fn activity_failed(activity: &ToolActivity<'_>) -> bool {
    matches!(tool_status(activity.block), Some(ToolCallStatusDto::Failed))
}

/// 把块列表切成转录项。
pub(crate) fn transcript_items(blocks: &[ConversationBlockDto]) -> Vec<TranscriptItem<'_>> {
    let mut items = Vec::new();
    let mut index = 0;
    while index < blocks.len() {
        if !is_assistant_like(&blocks[index]) {
            items.push(TranscriptItem::Block(&blocks[index]));
            index += 1;
            continue;
        }
        let start = index;
        while index < blocks.len() && is_assistant_like(&blocks[index]) {
            index += 1;
        }
        items.push(TranscriptItem::Run(Run::new(
            block_id(&blocks[start]),
            &blocks[start..index],
        )));
    }
    items
}

/// 列表末尾是否需要「分叉当前会话」这个入口。
///
/// 末项是自带分叉按钮的回合时不需要——与前端 `buildMessageListItems` 的 `forkRow` 同口径。
pub(crate) fn needs_session_fork_row(items: &[TranscriptItem<'_>]) -> bool {
    !matches!(
        items.last(),
        Some(TranscriptItem::Run(run))
            if run.actions.as_ref().is_some_and(|actions| actions.fork_at.is_some())
    )
}

/// 回合由助手块组成，其余块各自成行。
fn is_assistant_like(block: &ConversationBlockDto) -> bool {
    matches!(
        block,
        ConversationBlockDto::Assistant { .. } | ConversationBlockDto::ToolCall { .. }
    )
}

/// 切段：工具与思考归入当前过程段，遇到可见正文先收束过程段再起正文段。
pub(crate) fn run_segments<'a>(blocks: &'a [ConversationBlockDto]) -> Vec<RunSegment<'a>> {
    let mut segments = Vec::new();
    let mut pending: Vec<ProcessEntry<'a>> = Vec::new();

    for block in blocks {
        if let Some(activity) = activity_for(block) {
            pending.push(ProcessEntry::Tool(activity));
            continue;
        }
        if let ConversationBlockDto::Assistant { id, status, .. } = block {
            let streaming = matches!(status, ConversationBlockStatusDto::Streaming);
            // 序号由 `thinking_texts` 决定——`sync_markdown` 用同一个键把正文喂给 markdown，
            // 这里只需要数量。
            for index in 0..thinking_texts(block).len() {
                pending.push(ProcessEntry::Thinking {
                    key: thinking_key(id, index),
                    streaming,
                });
            }
        }
        if visible_text(block).is_empty() {
            continue;
        }
        push_process(&mut segments, &mut pending);
        segments.push(RunSegment::Content(block));
    }

    push_process(&mut segments, &mut pending);
    segments
}

/// 收束当前过程段；没有待收敛的条目时什么也不做。
fn push_process<'a>(segments: &mut Vec<RunSegment<'a>>, pending: &mut Vec<ProcessEntry<'a>>) {
    if pending.is_empty() {
        return;
    }
    let taken = std::mem::take(pending);
    // 折叠键先定下来：待回答的问卷稍后会被摘出去，键不能跟着条目列表一起变。
    let id = format!("process:{}", taken[0].id());

    let mut prompts = Vec::new();
    let entries: Vec<ProcessEntry<'a>> = taken
        .into_iter()
        .filter(|entry| match entry {
            ProcessEntry::Tool(activity) if ask_user::is_pending(activity.block) => {
                prompts.push(activity.block);
                false
            },
            _ => true,
        })
        .collect();

    let duration_seconds = entries
        .iter()
        .map(|entry| match entry {
            ProcessEntry::Tool(activity) => tool_duration(activity.block),
            ProcessEntry::Thinking { .. } => 0.0,
        })
        .sum();
    let has_streaming_work = entries.iter().any(|entry| match entry {
        ProcessEntry::Thinking { streaming, .. } => *streaming,
        ProcessEntry::Tool(activity) => matches!(
            tool_status(activity.block),
            Some(ToolCallStatusDto::Streaming)
        ),
    });
    // 待回答的问卷也算「有待决定的事」：与前端一致，此时摘要行整个展开，看得见它前面做过什么。
    let has_attention = !prompts.is_empty()
        || entries
            .iter()
            .any(|entry| matches!(entry, ProcessEntry::Tool(activity) if tool_needs_attention(activity.block)));

    segments.push(RunSegment::Process(ProcessSegment {
        id,
        entries,
        prompts,
        duration_seconds,
        has_streaming_work,
        has_attention,
    }));
}

/// 待审批且未终结的工具调用。
///
/// 待回答的 `askUser` 走另一条路：它们不算过程条目，由 [`ProcessSegment::prompts`] 单独带出来。
fn tool_needs_attention(block: &ConversationBlockDto) -> bool {
    matches!(
        block,
        ConversationBlockDto::ToolCall {
            status: ToolCallStatusDto::Streaming,
            approval: Some(_),
            ..
        }
    )
}

/// 工具块的耗时（秒）；没有耗时字段时为 0。
fn tool_duration(block: &ConversationBlockDto) -> f64 {
    duration_seconds(tool_metadata(block)).unwrap_or(0.0)
}

/// 工具块的 metadata 对象；非工具块为 `None`。
fn tool_metadata(block: &ConversationBlockDto) -> Option<&Value> {
    match block {
        ConversationBlockDto::ToolCall { metadata, .. } => metadata.as_ref(),
        _ => None,
    }
}

/// 工具块的状态；非工具块为 `None`。
pub(crate) fn tool_status(block: &ConversationBlockDto) -> Option<ToolCallStatusDto> {
    match block {
        ConversationBlockDto::ToolCall { status, .. } => Some(*status),
        _ => None,
    }
}

/// 活动行尾部的状态文字：完成的调用报耗时，其余报状态；非工具块没有可报的。
pub(crate) fn runtime_label(activity: &ToolActivity<'_>) -> Option<String> {
    let status = tool_status(activity.block)?;
    if matches!(status, ToolCallStatusDto::Complete) {
        let duration = duration_label(tool_metadata(activity.block));
        if !duration.is_empty() {
            return Some(duration);
        }
    }
    Some(status_label(status).to_owned())
}

fn status_label(status: ToolCallStatusDto) -> &'static str {
    match status {
        ToolCallStatusDto::Streaming => "执行中…",
        ToolCallStatusDto::Complete => "已完成",
        ToolCallStatusDto::Failed => "失败",
        ToolCallStatusDto::Cancelled => "已取消",
    }
}

/// 思考条目的 markdown 键。
pub(crate) fn thinking_key(block_id: &str, index: usize) -> String {
    format!("{block_id}:thinking:{index}")
}

/// 助手块的可见正文。
///
/// 思考现在走独立的 `reasoning_content`，只有旧会话的正文里才可能残留内联标记。
pub(crate) fn visible_text(block: &ConversationBlockDto) -> String {
    let ConversationBlockDto::Assistant {
        text,
        reasoning_content,
        ..
    } = block
    else {
        return String::new();
    };
    if reasoning_content.is_some() {
        return text.trim().to_owned();
    }
    extract_thinking(text).visible
}

/// 一个块里可查找的文本：转录上看得见的东西。
///
/// 查找要回答的是「我见过的那句话在哪」，因此取的就是展示用的文本：思考、正文、工具名与参数、
/// 工具结果、错误与提示。折叠区里的正文也在其中——它只是被折起来，不是不在。
///
/// 与 [`visible_text`] 分开：那个只回答「助手块说了什么」，要被拼进复制与分叉的输入里。
pub(crate) fn searchable_text(block: &ConversationBlockDto) -> String {
    match block {
        ConversationBlockDto::User { text, .. } => text.clone(),
        ConversationBlockDto::Assistant {
            reasoning_content, ..
        } => match reasoning_content {
            // 思考与正文是两段文字，中间补一个换行，免得两段粘成一个词。
            Some(reasoning) => format!("{reasoning}\n{}", visible_text(block)),
            None => visible_text(block),
        },
        ConversationBlockDto::ToolCall {
            name,
            arguments,
            text,
            ..
        } => format!("{name}\n{arguments}\n{text}"),
        ConversationBlockDto::Error { message, .. } => message.clone(),
        ConversationBlockDto::Recap { text, .. } => text.clone(),
        ConversationBlockDto::SystemNote { text, .. } => text.clone(),
        ConversationBlockDto::CompactSummary { summary, .. } => summary.clone(),
    }
}

/// 一项转录里可查找的文本：项内各块之间补换行。
pub(crate) fn transcript_item_text(item: &TranscriptItem<'_>) -> String {
    let blocks: &[ConversationBlockDto] = match item {
        TranscriptItem::Block(block) => std::slice::from_ref(*block),
        TranscriptItem::Run(run) => run.blocks,
    };
    blocks
        .iter()
        .map(searchable_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 回合级的两个动作的输入；收尾不是完稿正文时为 `None`。
///
/// 对应前端 `assistantRunCompletedReply`：只有收尾于完稿的助手块、且回合里确实有可见正文，
/// 才谈得上「复制这一回合」；分叉点也取自这一块。半截的回合两者都给不了。
fn run_actions(blocks: &[ConversationBlockDto]) -> Option<RunActions> {
    let copy_text = copy_text(blocks);
    if copy_text.is_empty() {
        return None;
    }
    let ConversationBlockDto::Assistant {
        storage_seq,
        status,
        ..
    } = blocks.last()?
    else {
        return None;
    };
    if !matches!(status, ConversationBlockStatusDto::Complete) {
        return None;
    }
    Some(RunActions {
        copy_text,
        fork_at: *storage_seq,
    })
}

/// 回合内助手块的可见正文，按空行连接。
///
/// [`visible_text`] 已经去过首尾空白，空正文的块在这里被丢掉——与前端
/// `assistantRunCopyText` 的 `trim` + `filter(Boolean)` 同口径。
fn copy_text(blocks: &[ConversationBlockDto]) -> String {
    let parts: Vec<String> = blocks
        .iter()
        .filter(|block| matches!(block, ConversationBlockDto::Assistant { .. }))
        .map(visible_text)
        .filter(|text| !text.is_empty())
        .collect();
    parts.join("\n\n")
}

/// 助手块的思考内容。
///
/// 有 `reasoning_content` 就用它；否则只有完稿的正文才值得提取——流式中的正文可能正停在
/// 标记中间，提取结果会一跳一跳。
pub(crate) fn thinking_texts(block: &ConversationBlockDto) -> Vec<String> {
    let ConversationBlockDto::Assistant {
        text,
        reasoning_content,
        status,
        ..
    } = block
    else {
        return Vec::new();
    };
    if let Some(reasoning) = reasoning_content.as_ref().filter(|text| !text.is_empty()) {
        return vec![reasoning.clone()];
    }
    if matches!(status, ConversationBlockStatusDto::Streaming) {
        return Vec::new();
    }
    extract_thinking(text).blocks
}

/// 回合耗时的显示形式：不足一分钟按秒取整（至少 1s），超过按「Nm Ss」。
fn format_run_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return String::new();
    }
    if seconds < 60.0 {
        return format!("{}s", (seconds.round() as i64).max(1));
    }
    let minutes = (seconds / 60.0).floor() as i64;
    let rest = (seconds % 60.0).round() as i64;
    if rest > 0 {
        format!("{minutes}m {rest}s")
    } else {
        format!("{minutes}m")
    }
}

/// 旧会话正文里的内联思考标记：Kimi 时代的线缆约定。
const THINKING_OPEN: &str = "<think-block>";
const THINKING_CLOSE: &str = "</think-block>";

/// 正文与其中被标记包起来的思考。
struct ThinkingParts {
    visible: String,
    blocks: Vec<String>,
}

/// 剥掉内联思考标记。
///
/// 未闭合的标记留着当正文——前端在流式末尾也是这么处理的：宁可在正文里看到半截标记，
/// 也不要在正文里凭空少一截。重复的思考内容只记一次。
fn extract_thinking(text: &str) -> ThinkingParts {
    let mut visible = String::with_capacity(text.len());
    let mut blocks: Vec<String> = Vec::new();
    let mut cursor = 0;

    while let Some(open) = find_tag(text, THINKING_OPEN, cursor) {
        let content_start = open + THINKING_OPEN.len();
        let Some(close) = find_tag(text, THINKING_CLOSE, content_start) else {
            break;
        };
        visible.push_str(&text[cursor..open]);
        let content = text[content_start..close].trim();
        if !content.is_empty() && !blocks.iter().any(|seen| seen == content) {
            blocks.push(content.to_owned());
        }
        cursor = close + THINKING_CLOSE.len();
    }
    visible.push_str(&text[cursor..]);

    ThinkingParts {
        visible: visible.trim().to_owned(),
        blocks,
    }
}

/// ASCII 大小写不敏感的子串查找。
///
/// 标记全是 ASCII，逐字节比较不会落在多字节字符的内部：非 ASCII 字节都 >= 0x80，
/// 与任何 ASCII 字节都不相等，命中位置必然也是字符边界。
fn find_tag(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || from >= bytes.len() || bytes.len() < needle.len() {
        return None;
    }
    (from..=bytes.len() - needle.len())
        .find(|start| bytes[*start..*start + needle.len()].eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::http::{
        ConversationBlockDto, ConversationBlockStatusDto, ToolApprovalDto, ToolCallStatusDto,
    };
    use serde_json::json;

    use super::{
        ProcessEntry, Run, RunSegment, ToolActivity, TranscriptItem, thinking_key, thinking_texts,
        transcript_items, visible_text,
    };

    fn user(id: &str, text: &str) -> ConversationBlockDto {
        ConversationBlockDto::User {
            id: id.into(),
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    fn assistant(id: &str, text: &str) -> ConversationBlockDto {
        assistant_with(id, text, None, ConversationBlockStatusDto::Complete)
    }

    fn assistant_with(
        id: &str,
        text: &str,
        reasoning_content: Option<&str>,
        status: ConversationBlockStatusDto,
    ) -> ConversationBlockDto {
        ConversationBlockDto::Assistant {
            id: id.into(),
            text: text.into(),
            reasoning_content: reasoning_content.map(str::to_owned),
            storage_seq: None,
            status,
        }
    }

    fn tool_call(
        id: &str,
        name: &str,
        metadata: serde_json::Value,
        approval: Option<ToolApprovalDto>,
    ) -> ConversationBlockDto {
        ConversationBlockDto::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: json!({}).to_string(),
            text: String::new(),
            status: if approval.is_some() {
                ToolCallStatusDto::Streaming
            } else {
                ToolCallStatusDto::Complete
            },
            metadata: Some(metadata),
            approval,
            arguments_json: Some(json!({})),
        }
    }

    fn shell_call(status: ToolCallStatusDto, metadata: serde_json::Value) -> ConversationBlockDto {
        ConversationBlockDto::ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            arguments: json!({}).to_string(),
            text: String::new(),
            status,
            metadata: Some(metadata),
            approval: None,
            arguments_json: Some(json!({})),
        }
    }

    /// 一道 askUser 问卷；`status` 决定它是否还在等回答。
    fn ask_user_call(
        id: &str,
        status: ToolCallStatusDto,
        arguments_json: serde_json::Value,
    ) -> ConversationBlockDto {
        ConversationBlockDto::ToolCall {
            id: id.into(),
            name: "askUser".into(),
            arguments: arguments_json.to_string(),
            text: String::new(),
            status,
            metadata: None,
            approval: None,
            arguments_json: Some(arguments_json),
        }
    }

    /// 一份最小可用的问卷参数。
    fn questionnaire_args() -> serde_json::Value {
        json!({
            "questions": [{
                "header": "数据库",
                "question": "选哪个？",
                "options": [
                    { "label": "Postgres", "description": "关系型" },
                    { "label": "SQLite", "description": "嵌入式" }
                ]
            }]
        })
    }

    /// 取出单个工具块的活动标签；块列表里只有一个工具调用时才成立。
    fn activity_of(blocks: &[ConversationBlockDto]) -> ToolActivity<'_> {
        let TranscriptItem::Run(Run { segments, .. }) = &transcript_items(blocks)[0] else {
            panic!("应是一段回合");
        };
        let RunSegment::Process(process) = &segments[0] else {
            panic!("应是一段过程");
        };
        let ProcessEntry::Tool(activity) = &process.entries[0] else {
            panic!("首条应是工具");
        };
        activity.clone()
    }

    #[test]
    fn each_run_is_cut_into_process_and_content_segments() {
        let blocks = vec![
            user("u1", "帮我改一下"),
            assistant_with(
                "a1",
                "先看看",
                Some("想一下"),
                ConversationBlockStatusDto::Complete,
            ),
            tool_call("c1", "read", json!({ "path": "src/a.rs" }), None),
            assistant("a2", "改好了"),
            user("u2", "再改一处"),
            tool_call("c2", "shell", json!({ "command": "cargo test" }), None),
        ];
        let items = transcript_items(&blocks);

        assert_eq!(items.len(), 4, "两条用户消息各自成行，两段回合各自成组");
        let TranscriptItem::Block(block) = items[0] else {
            panic!("首项应是用户消息");
        };
        assert_eq!(super::block_id(block), "u1");

        let TranscriptItem::Run(Run { segments, .. }) = &items[1] else {
            panic!("第二项应是一段回合");
        };
        assert_eq!(segments.len(), 4, "思考 → 正文 → 工具 → 正文，各自成段");
        let RunSegment::Process(process) = &segments[0] else {
            panic!("首段应是过程段");
        };
        assert_eq!(process.entries.len(), 1, "a1 的思考自成一段过程");
        assert!(matches!(process.entries[0], ProcessEntry::Thinking { .. }));
        assert_eq!(process.id, "process:a1:thinking:0", "折叠键取自首条条目");
        let RunSegment::Content(content) = &segments[1] else {
            panic!("次段应是正文段");
        };
        assert_eq!(super::block_id(content), "a1");
        let RunSegment::Process(process) = &segments[2] else {
            panic!("第三段应是过程段");
        };
        assert_eq!(process.entries.len(), 1);
        assert!(matches!(process.entries[0], ProcessEntry::Tool(_)));
        let RunSegment::Content(content) = &segments[3] else {
            panic!("末段应是正文段");
        };
        assert_eq!(super::block_id(content), "a2");


        let TranscriptItem::Block(block) = &items[2] else {
            panic!("第三项应是用户消息");
        };
        assert_eq!(super::block_id(block), "u2");
        let TranscriptItem::Run(Run { segments, .. }) = &items[3] else {
            panic!("末项应是新的一段回合");
        };
        assert_eq!(segments.len(), 1, "这一段里只有一个工具调用");
        let RunSegment::Process(process) = &segments[0] else {
            panic!("应是一段过程");
        };
        assert_eq!(process.entries.len(), 1);
    }


    #[test]
    fn a_run_without_prose_is_one_process_segment() {
        let blocks = vec![
            assistant_with(
                "a1",
                "",
                Some("只想没说"),
                ConversationBlockStatusDto::Complete,
            ),
            tool_call("c1", "read", json!({}), None),
        ];
        let items = transcript_items(&blocks);
        let TranscriptItem::Run(Run { segments, .. }) = &items[0] else {
            panic!("应是一段回合");
        };
        assert_eq!(segments.len(), 1);
        assert!(matches!(segments[0], RunSegment::Process(_)));
    }

    #[test]
    fn process_titles_and_latest_labels_track_the_run() {
        let done = vec![
            tool_call("c1", "read", json!({ "path": "src/a.rs" }), None),
            tool_call(
                "c2",
                "shell",
                json!({ "command": "cargo test", "durationMs": 2500 }),
                None,
            ),
        ];
        let TranscriptItem::Run(Run { segments, .. }) = &transcript_items(&done)[0] else {
            panic!("应是一段回合");
        };
        let RunSegment::Process(process) = &segments[0] else {
            panic!("应是一段过程");
        };
        assert_eq!(process.title(), "已处理 3s", "耗时不足 1s 的调用按 1s 计");
        assert!(process.latest_label().starts_with("运行命令 cargo test"));

        let streaming = vec![assistant_with(
            "a1",
            "",
            Some("还在想"),
            ConversationBlockStatusDto::Streaming,
        )];
        let TranscriptItem::Run(Run { segments, .. }) = &transcript_items(&streaming)[0] else {
            panic!("应是一段回合");
        };
        let RunSegment::Process(process) = &segments[0] else {
            panic!("应是一段过程");
        };
        assert_eq!(process.title(), "处理中");
        assert_eq!(process.latest_label(), "正在思考");
    }

    #[test]
    fn a_pending_approval_keeps_the_process_segment_visible() {
        let blocks = vec![tool_call(
            "c1",
            "shell",
            json!({ "command": "rm -rf build" }),
            Some(ToolApprovalDto {
                call_id: "c1".into(),
                prompt: "允许删除构建目录？".into(),
                rule_key: None,
            }),
        )];
        let TranscriptItem::Run(Run { segments, .. }) = &transcript_items(&blocks)[0] else {
            panic!("应是一段回合");
        };
        let RunSegment::Process(process) = &segments[0] else {
            panic!("应是一段过程");
        };
        assert!(process.has_attention, "等待审批的过程段必须保持展开");
        assert!(process.has_streaming_work);
    }

    #[test]
    fn legacy_thinking_markers_leave_the_visible_text() {
        let blocks = vec![assistant(
            "a1",
            "<think-block>先读文件</think-block>看完了\n<think-block>先读文件</think-block>",
        )];
        let TranscriptItem::Run(Run { segments, .. }) = &transcript_items(&blocks)[0] else {
            panic!("应是一段回合");
        };
        assert_eq!(segments.len(), 2, "标记剥掉后仍有正文，因此过程段 + 正文段");
        let RunSegment::Process(process) = &segments[0] else {
            panic!("首段应是过程段");
        };
        assert_eq!(process.entries.len(), 1, "重复的思考只留一条");
        assert!(matches!(process.entries[0], ProcessEntry::Thinking { .. }));
        let parts = super::extract_thinking(
            "<think-block>先读文件</think-block>看完了\n<think-block>先读文件</think-block>",
        );
        assert_eq!(parts.blocks, vec!["先读文件".to_owned()]);

        let RunSegment::Content(content) = &segments[1] else {
            panic!("次段应是正文段");
        };
        assert_eq!(visible_text(content), "看完了");
        assert_eq!(thinking_key("a1", 0), "a1:thinking:0");
    }

    #[test]
    fn an_unclosed_thinking_marker_stays_in_the_visible_text() {
        let parts = super::extract_thinking("正文<think-block>还没写完");
        assert_eq!(parts.visible, "正文<think-block>还没写完");
        assert!(parts.blocks.is_empty());
    }

    #[test]
    fn reasoning_content_wins_over_inline_markers() {
        let with_reasoning = assistant_with(
            "a1",
            "<think-block>不该被提取</think-block>正文",
            Some("独立字段里的思考"),
            ConversationBlockStatusDto::Complete,
        );
        assert_eq!(
            thinking_texts(&with_reasoning),
            vec!["独立字段里的思考".to_owned()]
        );
        assert_eq!(
            visible_text(&with_reasoning),
            "<think-block>不该被提取</think-block>正文",
            "有独立字段时正文原样显示"
        );

        let streaming = assistant_with(
            "a2",
            "半截<think-block>",
            None,
            ConversationBlockStatusDto::Streaming,
        );
        assert!(
            thinking_texts(&streaming).is_empty(),
            "流式中的正文不做提取"
        );
    }

    #[test]
    fn run_durations_round_to_seconds_then_minutes() {
        assert_eq!(super::format_run_duration(0.0), "");
        assert_eq!(super::format_run_duration(0.4), "1s", "不足 1s 也要看得见");
        assert_eq!(super::format_run_duration(3.4), "3s");
        assert_eq!(super::format_run_duration(59.6), "60s");
        assert_eq!(super::format_run_duration(65.0), "1m 5s");
        assert_eq!(super::format_run_duration(120.0), "2m");
    }

    #[test]
    fn activity_runtime_reports_duration_then_status() {
        let finished = tool_call("c1", "shell", json!({ "durationMs": 1500 }), None);
        assert_eq!(
            super::runtime_label(&activity_of(&[finished])).as_deref(),
            Some("1.5s"),
            "完成的调用报耗时"
        );

        let unmeasured = tool_call("c1", "shell", json!({}), None);
        assert_eq!(
            super::runtime_label(&activity_of(&[unmeasured])).as_deref(),
            Some("已完成"),
            "没有耗时字段时退回状态文字"
        );

        let running = shell_call(ToolCallStatusDto::Streaming, json!({}));
        assert_eq!(
            super::runtime_label(&activity_of(&[running])).as_deref(),
            Some("执行中…")
        );

        let failed = shell_call(ToolCallStatusDto::Failed, json!({}));
        assert_eq!(
            super::runtime_label(&activity_of(&[failed])).as_deref(),
            Some("失败")
        );
    }

    #[test]
    fn a_pending_questionnaire_is_lifted_out_of_the_process_segment() {
        let blocks = vec![
            tool_call("c1", "read", json!({ "path": "src/a.rs" }), None),
            ask_user_call("c2", ToolCallStatusDto::Streaming, questionnaire_args()),
        ];
        let TranscriptItem::Run(Run { segments, .. }) = &transcript_items(&blocks)[0] else {
            panic!("应是一段回合");
        };
        let RunSegment::Process(process) = &segments[0] else {
            panic!("应是一段过程");
        };

        assert_eq!(process.entries.len(), 1, "问卷不算过程条目");
        assert_eq!(process.prompts.len(), 1);
        assert_eq!(super::block_id(process.prompts[0]), "c2");
        assert_eq!(
            process.id, "process:c1",
            "折叠键取自收拢时的首条条目，不随问卷被摘走而变化"
        );
        assert!(process.has_attention, "有待回答的问卷时摘要行保持展开");
        assert_eq!(
            process.latest_label(),
            "读取文件 a.rs",
            "最近一条活动只看剩下的过程条目"
        );
    }

    #[test]
    fn an_answered_questionnaire_returns_to_the_process_entries() {
        let blocks = vec![
            tool_call("c1", "read", json!({ "path": "src/a.rs" }), None),
            ask_user_call("c2", ToolCallStatusDto::Complete, questionnaire_args()),
        ];
        let TranscriptItem::Run(Run { segments, .. }) = &transcript_items(&blocks)[0] else {
            panic!("应是一段回合");
        };
        let RunSegment::Process(process) = &segments[0] else {
            panic!("应是一段过程");
        };

        assert!(process.prompts.is_empty(), "作答完成后不再有问卷卡片");
        assert_eq!(process.entries.len(), 2, "问卷回到过程条目里");
        assert!(!process.has_attention);
        assert_eq!(
            process.latest_label(),
            "工具调用 askUser",
            "作答后的问卷按普通工具行显示"
        );
    }

    /// 带持久化点的完稿助手块：回合级动作能分叉的前提。
    fn assistant_at(id: &str, text: &str, storage_seq: u64) -> ConversationBlockDto {
        ConversationBlockDto::Assistant {
            id: id.into(),
            text: text.into(),
            reasoning_content: None,
            storage_seq: Some(storage_seq),
            status: ConversationBlockStatusDto::Complete,
        }
    }

    /// 首段回合是否给出回合级动作。
    fn has_run_actions(blocks: &[ConversationBlockDto]) -> bool {
        let TranscriptItem::Run(run) = &transcript_items(blocks)[0] else {
            panic!("应是一段回合");
        };
        run.actions.is_some()
    }

    #[test]
    fn a_completed_run_offers_copy_and_fork() {
        let blocks = vec![
            assistant("a1", "先看看"),
            tool_call("c1", "read", json!({ "path": "src/a.rs" }), None),
            assistant_at("a2", "改好了", 7),
        ];
        let items = transcript_items(&blocks);
        let TranscriptItem::Run(run) = &items[0] else {
            panic!("应是一段回合");
        };

        let actions = run.actions.as_ref().expect("收尾于完稿正文的回合给出动作");
        assert_eq!(
            actions.copy_text, "先看看\n\n改好了",
            "只拼助手正文，空行分隔"
        );
        assert_eq!(actions.fork_at, Some(7));
        assert_eq!(run.key, "a1", "元素 id 取自回合首块");
        assert!(
            !super::needs_session_fork_row(&items),
            "末项自带分叉按钮时不再补会话级入口"
        );
    }

    #[test]
    fn a_run_without_completed_prose_offers_no_action() {
        let streaming = vec![assistant_with(
            "a1",
            "写到一半",
            None,
            ConversationBlockStatusDto::Streaming,
        )];
        assert!(!has_run_actions(&streaming), "未完稿的回合没有可复制的内容");

        let trailing_tool = vec![
            assistant("a1", "看看"),
            tool_call("c1", "read", json!({}), None),
        ];
        assert!(
            !has_run_actions(&trailing_tool),
            "收尾在工具调用上时没有可复制的结论"
        );

        let empty_tail = vec![assistant("a1", "")];
        assert!(!has_run_actions(&empty_tail), "空正文没有可复制的内容");
    }

    #[test]
    fn the_session_fork_row_appears_when_the_tail_cannot_fork() {
        // 收尾在工具调用上：末项给不出分叉按钮，列表末尾补一个会话级入口。
        let trailing_tool = vec![
            assistant("a1", "看看"),
            tool_call("c1", "read", json!({}), None),
        ];
        assert!(super::needs_session_fork_row(&transcript_items(
            &trailing_tool
        )));

        // 有正文但没有持久化点：动作还在（能复制），分叉点为空，入口照样要补。
        let no_fork_point = vec![assistant("a1", "改好了")];
        assert!(has_run_actions(&no_fork_point));
        assert!(super::needs_session_fork_row(&transcript_items(
            &no_fork_point
        )));
    }
}
