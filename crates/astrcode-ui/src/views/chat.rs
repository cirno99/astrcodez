//! 会话视图：块列表、流式渲染、输入与审批。
//!
//! 渲染只负责把 [`ConversationState`] 的当前值摆出来；增量归并、孤儿 patch 不变式
//! 都在状态层（`conversation`），这里不重复判断。
//!
//! 事件流由订阅任务直接消费：一次唤醒里先把已解出的帧一次抽干，作为一批交给状态层，
//! 最后只 `cx.notify()` 一次（ADR 0001 第 13 轮）。

use std::{collections::HashMap, time::Duration};

use astrcode_protocol::{
    http::{
        AgentSessionLinkDto, AgentSessionStatusDto, AvailableModelDto, CommandCompletionItemDto,
        ConfigViewResponseDto, ConversationBlockDto, ConversationControlStateDto,
        ConversationDeltaDto, ConversationStreamEnvelopeDto, CurrentModelResponseDto,
        LlmRetryStatusDto, PromptSubmitResponse, SlashCommandInfoDto, StatusItemDto,
        ToolApprovalDto, ToolCallStatusDto,
    },
    wire::{ApprovalDecisionDto, ApprovalModeDto, PhaseDto},
};
use gpui_kit::{
    AnyElement, App, AppContext as _, AsyncApp, ClipboardItem, Context, Entity, EventEmitter,
    Focusable as _, FollowMode, FontWeight, Hsla, InteractiveElement as _, IntoElement, Keystroke,
    ListAlignment, ListOffset, ListState, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window,
    component::{
        ActiveTheme as _, Disableable as _, Sizable as _, Size, ThemeStyled as _,
        bubble::{Bubble, BubbleVariant},
        button::{Button, ButtonVariants as _, Toggle},
        h_flex,
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
        message::MessageAlignment,
        spinner::Spinner,
        text::{TextView, TextViewState},
        v_flex,
    },
    div, list, px, radians,
};
use serde_json::Value;

use crate::{
    agent_session::{self, AgentSession},
    api::Api,
    ask_user,
    assistant_run::{
        ProcessEntry, ProcessSegment, Run, RunActions, RunSegment, TranscriptItem, activity_failed,
        needs_session_fork_row, runtime_label, thinking_key, thinking_texts, transcript_item_text,
        transcript_items, visible_text,
    },
    composer_config,
    composer_queue::{self, DeliveryMode, PendingMessage, PendingQueue},
    conversation::{ConversationState, DeltaBuffer, delta::block_id},
    find::{self, Find},
    icons::IconName,
    metrics,
    pending_ask_user::{self, PendingQuestion, PendingQuestions},
    session_list,
    slash_command::{self, ArgTrigger, SlashTrigger},
    todo_list,
    tool_view::{
        ActivityKind, DetailBody, DiffLineKind, PreviewKind, ToolActivity, ToolView, detail_body,
        diff_line_kind, meta_rows, numbered_line, patch_files, summary_line, tool_view,
        truncate_preview,
    },
    views::{ask_user_card::AskUserCard, icon_button, page_header},
};

/// 工具卡正文的预览上限：字符数与行数谁先到谁生效，与前端 `previewText` 同口径。
const TOOL_PREVIEW_MAX_CHARS: usize = 6000;
const TOOL_PREVIEW_MAX_LINES: usize = 24;

/// 展开后仍然最多渲染的行数。
///
/// 这是渲染成本护栏：每行是一个元素，不设上限时一条大输出就能拖垮一帧。
const TOOL_MAX_RENDERED_LINES: usize = 400;

/// 补丁卡里逐文件列出的条数上限，其余收成一行计数（与前端 `PatchToolDetails` 同值）。
const PATCH_FILES_SHOWN: usize = 12;

/// 事件流断开后重新订阅前的等待时间。
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// 「已复制」保持多久后落回「复制」。
///
/// 这个标签是复制成功与否的唯一反馈：剪贴板写入没有回执可等（`write_to_clipboard` 不返回结果）。
const COPIED_LABEL_HOLD: Duration = Duration::from_secs(2);

/// 跨会话待回答问卷的轮询间隔，与前端 `PENDING_ASK_USER_POLL_INTERVAL_MS` 同值。
const PENDING_ASK_USER_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// 单次问卷快照请求的等待上限，与前端 `PENDING_ASK_USER_REFRESH_TIMEOUT_MS` 同值。
///
/// 轮询是一趟一趟串起来的：某一次请求挂死会让后面的轮询再也发不出去，与前端那个
/// 「in-flight 标记不落回」的处境一样，所以同样给一个上限。
const PENDING_ASK_USER_POLL_TIMEOUT: Duration = Duration::from_secs(5);

/// 参数补全的防抖时长，与前端 `InputBar` 里那个 `setTimeout(..., 250)` 同值。
const ARG_COMPLETION_DEBOUNCE: Duration = Duration::from_millis(250);

/// 命令浮层的宽度上限；比阅读列窄一点，免得压住顶栏。
const COMMAND_PANEL_MAX_WIDTH: f32 = 560.0;

/// 状态行上项目名的宽度上限，与前端 `max-w-[220px]` 同值。
const PROJECT_LABEL_MAX_WIDTH: f32 = 220.0;

/// 状态行上分支名的宽度上限，与前端 `max-w-[180px]` 同值。
const BRANCH_LABEL_MAX_WIDTH: f32 = 180.0;

/// 状态行上插件状态栏项的宽度上限，与前端 `max-w-[160px]` 同值。
const STATUS_ITEM_MAX_WIDTH: f32 = 160.0;

/// 模型面板的宽度，与前端 `ModelSelector` 的 `w-[240px]` 同值。
const MODEL_PANEL_WIDTH: f32 = 240.0;

/// 模型列表的高度上限，与前端 `max-h-[240px]` 同值。
const MODEL_PANEL_MAX_HEIGHT: f32 = 240.0;

/// 触发器上模型名的宽度上限，与前端 `max-w-[140px]` 同值。
const MODEL_LABEL_MAX_WIDTH: f32 = 140.0;
/// 浮层最多画多少行：再多就该用过滤而不是滚动来找。
const COMMAND_PANEL_MAX_ROWS: usize = 10;

/// 会话面板对外的事件。
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// 分叉出了新会话；会话列表在外壳手里，切过去与标题注入都由它做。
    SessionForked(String),
    /// 要切到另一个会话（目前只有子 Agent 卡会发）；切过去仍由外壳做。
    OpenSession(String),
    /// 用户要浏览当前会话项目里的文件；浏览根目录在外壳手里，切页也由它做。
    OpenFiles,
    /// 用户要求展开侧边栏（收起时页头上的那枚按钮）。
    ToggleSidebar,
}

impl EventEmitter<ChatEvent> for ChatView {}

/// 命令面板：`/` 触发的上下文与当前选中项。
struct CommandPanel {
    trigger: SlashTrigger,
    selected: usize,
}

/// 参数补全面板：`/name ` 触发的上下文与已取到的候选。
struct ArgumentPanel {
    trigger: ArgTrigger,
    items: Vec<CommandCompletionItemDto>,
    truncated: bool,
    loading: bool,
    selected: usize,
}

pub struct ChatView {
    api: Api,
    session_id: Option<String>,
    state: ConversationState,
    buffer: DeltaBuffer,
    /// 助手文本的 markdown 状态，按块 id 索引。
    markdown: HashMap<String, Entity<TextViewState>>,
    /// 每个助手块上一次已同步给 markdown 的文本。
    ///
    /// 用它只在文本真正变化时写入：否则每批都要把所有块的文本重拷一遍。
    synced_text: HashMap<String, String>,
    input: Entity<TextareaState>,
    /// 用户显式设过展开态的折叠区，按块 id 索引。
    ///
    /// 没设过时按块的当前状态推导默认值（见 [`ChatView::is_expanded`]）；用户一动手就以这里为准。
    expanded: HashMap<String, bool>,
    /// 用户显式点开过「完整输出」的长正文，按「块 id:下标」索引。
    ///
    /// 与 [`ChatView::expanded`] 分开：工具卡本身的展开与正文预览的展开是两件事。
    preview_expanded: HashMap<String, bool>,
    /// askUser 问卷卡片，按工具调用 id 索引。
    ///
    /// 问卷还在等回答时卡片渲染在过程折叠区外；作答完成后随工具块回到折叠区的详情里。
    ask_user: HashMap<String, Entity<AskUserCard>>,
    /// 顶栏显示的对象名；由外壳在切会话时注入。
    title: Option<SharedString>,
    /// 转录的虚拟列表状态：只物化可见项，长会话不再随块数线性长内存。
    transcript: ListState,
    /// 上次同步进 [`Self::transcript`] 的块修订号，用来区分「尾部追加」与「整体替换」。
    transcript_synced_revision: u64,
    error: Option<String>,
    /// 事件流消费任务；换会话时整体丢弃以停掉旧流。
    stream_task: Option<Task<()>>,
    /// 刚复制过的文本键（回合或提示块）；按钮据此把「复制」显示成「已复制」。
    copied: Option<String>,
    /// 「已复制」的回落任务；再复制一次即换掉它，上一次的回落随之取消。
    copy_timer: Option<Task<()>>,
    /// 跨会话的待回答问卷。直播事件与轮询快照都汇到这里，因此**不随切会话清空**。
    pending_ask_user: PendingQuestions,
    /// 当前会话里丢了工具块、只能靠快照恢复的问卷卡片，按工具调用 id 索引。
    ///
    /// 与 [`ChatView::ask_user`] 分开：那一份跟着块走，这一份跟着全局问卷表走。
    recovered_ask_user: HashMap<String, Entity<AskUserCard>>,
    /// 会话 id → 显示名；外壳注入。横幅里的其他会话只有 id 可用时退回短 id。
    session_titles: HashMap<String, String>,
    /// 事件流是否连着。连着时全局问卷事件会自己送到，轮询让路。
    stream_connected: bool,
    /// 跨会话问卷的轮询任务；与视图同寿命。
    poll_task: Option<Task<()>>,
    /// 当前会话可用的斜杠命令；切会话与扩展注册表变化时重取。
    commands: Vec<SlashCommandInfoDto>,
    /// 命令列表是否在拉取中；面板首帧据此画「加载中」。
    commands_loading: bool,
    /// 插件注册的状态栏项；与命令列表同一次取，切会话即清空。
    status_items: Vec<StatusItemDto>,
    /// 当前会话的工作目录；状态行上的项目名取它末段。由外壳在切会话时注入。
    working_dir: Option<String>,
    /// 忙时按下的发送往哪去；切换按钮只在这个状态下出现。
    delivery: DeliveryMode,
    /// 待发队列；换会话即清空（与前端 `resetSessionView` 同口径）。
    queue: PendingQueue,
    /// 待发面板是否展开。
    queue_expanded: bool,
    /// 输入区下方的一次性提示：队列回落、排队提交失败这类需要说一声但不阻塞的事。
    hint: Option<String>,
    /// 队列冲刷任务；一趟一趟串着跑，换会话时丢掉。
    flush_task: Option<Task<()>>,
    /// 命令面板；`/` 触发期间才有值。
    command_panel: Option<CommandPanel>,
    /// 参数补全面板；`/name ` 触发期间才有值。
    argument_panel: Option<ArgumentPanel>,
    /// 参数补全的防抖任务；换一次触发上下文即换掉它，上一次的响应随之作废。
    arg_task: Option<Task<()>>,
    /// 输入区是否持有焦点；两块面板只在有焦点时接管方向键与 Tab。
    input_focused: bool,
    /// 配置视图：当前选区与工具权限模式；改动后重取。
    config: Option<ConfigViewResponseDto>,
    /// 权限模式是否在提交中；按钮据此置灰。
    approval_saving: bool,
    /// 配置取数任务；重取时换掉上一个，于是慢响应盖不住新结果。
    config_task: Option<Task<()>>,
    /// 宿主上全部可选模型；打开面板时重取。
    models: Vec<AvailableModelDto>,
    /// 当前选中的模型：触发器上写它的 id，面板据此给那一行画勾。
    current_model: Option<CurrentModelResponseDto>,
    /// 模型清单是否在拉取中。
    models_loading: bool,
    /// 模型面板是否展开。
    model_panel_open: bool,
    /// 模型面板的搜索框。
    model_query: Entity<InputState>,
    /// 侧边栏是否显示；由外壳建视图后告知，收起时页头给展开入口。
    sidebar_open: bool,
    /// 选中模型后的写回是否在提交中。
    model_saving: bool,
    /// 模型清单取数任务；重取时换掉上一个。
    models_task: Option<Task<()>>,
    /// 会话内查找：查询框、大小写口径与当前那一项。
    search: Find,
    /// 命中的转录项下标（升序）；一项里命中多处也只算一项——跳转以「一条消息」为单位。
    search_matches: Vec<usize>,
    _subscriptions: Vec<Subscription>,
}

impl ChatView {
    pub fn new(api: Api, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 转录默认贴着末尾：跟随交给列表自己的「尾随」模式，用户往上滚就暂停、滚回底部自动接上
        // （见 [`Self::after_state_change`]）。
        let transcript = ListState::new(0, ListAlignment::Bottom, px(600.));
        transcript.set_follow_mode(FollowMode::Tail);
        // 输入区是多行的：回车提交、Shift+Enter 换行，高度随内容长到 8 行。
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("给 astrcode 一条指令…")
                .submit_on_enter(true)
                .auto_grow(1, 8)
        });
        // 模型面板的搜索框：单行，回车不提交。
        let model_query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索模型…"));
        // 会话内查找的查询框；它常驻页头，不像代码页那条要 Ctrl+F 才出现。
        let search_query = cx.new(|cx| InputState::new(window, cx).placeholder("在会话里查找…"));
        let view = cx.weak_entity();
        let subscriptions = vec![
            cx.subscribe_in(&input, window, {
                let input = input.clone();
                move |this, _, event: &InputEvent, window, cx| match event {
                    // 面板开着时回车已经被拦下，走不到这里。
                    InputEvent::PressEnter { .. } => this.submit(&input, window, cx),
                    InputEvent::Change => this.refresh_panels(cx),
                    InputEvent::Focus => this.input_focused = true,
                    InputEvent::Blur => {
                        this.input_focused = false;
                        this.close_panels(cx);
                    },
                }
            }),
            // 搜索框只管重画：过滤是渲染时现算的，没有要在状态里维护的东西。
            cx.subscribe_in(&model_query, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            // 会话内查找：每敲一个字重算一次命中。文本都在内存里，不必防抖。
            cx.subscribe_in(
                &search_query,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.refresh_search(cx);
                    }
                },
            ),
            // 面板开着时把方向键、Tab 与回车从输入区手里接过来：拦截发生在动作派发之前，
            // 停掉派发就等于输入区收不到这次按键——上下键不会移光标，Tab 不会跳焦点。
            cx.intercept_keystrokes(move |event, window, cx| {
                let handled = view
                    .update(cx, |this, cx| {
                        this.handle_panel_key(&event.keystroke, window, cx)
                    })
                    .unwrap_or(false);
                if handled {
                    cx.stop_propagation();
                }
            }),
        ];
        let poll_api = api.clone();

        let mut view = Self {
            api,
            session_id: None,
            state: ConversationState::new(),
            buffer: DeltaBuffer::new(),
            markdown: HashMap::new(),
            synced_text: HashMap::new(),
            input,
            expanded: HashMap::new(),
            preview_expanded: HashMap::new(),
            ask_user: HashMap::new(),
            title: None,
            transcript,
            transcript_synced_revision: 0,
            error: None,
            stream_task: None,
            copied: None,
            copy_timer: None,
            pending_ask_user: PendingQuestions::default(),
            recovered_ask_user: HashMap::new(),
            session_titles: HashMap::new(),
            stream_connected: false,
            poll_task: None,
            commands: Vec::new(),
            commands_loading: false,
            status_items: Vec::new(),
            working_dir: None,
            delivery: DeliveryMode::default(),
            queue: PendingQueue::default(),
            queue_expanded: true,
            hint: None,
            flush_task: None,
            command_panel: None,
            argument_panel: None,
            arg_task: None,
            input_focused: false,
            config: None,
            approval_saving: false,
            config_task: None,
            models: Vec::new(),
            current_model: None,
            models_loading: false,
            model_panel_open: false,
            model_query,
            sidebar_open: true,
            model_saving: false,
            models_task: None,
            search: Find::new(search_query),
            search_matches: Vec::new(),
            _subscriptions: subscriptions,
        };
        // 没打开任何会话时也要能看见别的会话在等回答，所以轮询从建视图那一刻就起。
        view.poll_task = Some(Self::spawn_pending_poll(poll_api, cx));
        // 配置是全局的（不跟会话走），同样从建视图那一刻就取一次。
        view.load_config(cx);
        // 模型清单也是全局的：触发器上要写当前模型的名字，不等面板打开。
        view.load_models(cx);
        view
    }

    /// 侧边栏是否显示；外壳切换时告知。
    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open == open {
            return;
        }
        self.sidebar_open = open;
        cx.notify();
    }

    /// 切到某个会话：先取快照铺满，再从快照 cursor 接上事件流。
    pub fn open_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.reset_session_view(cx);
        self.session_id = Some(session_id.clone());
        // 换会话从末尾看起：上一个会话里往上翻过的话，跟随已经停着，新会话不该继承那个位置。
        self.transcript.set_follow_mode(FollowMode::Tail);

        let api = self.api.clone();
        self.stream_task = Some(cx.spawn(async move |this, cx| {
            let snapshot = match api.conversation(&session_id).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    this.update(cx, |this, cx| this.fail(error.to_string(), cx))
                        .ok();
                    return;
                },
            };
            let cursor = snapshot.cursor.value.clone();
            let applied = this
                .update(cx, |this, cx| {
                    this.apply_snapshot(
                        snapshot.blocks,
                        snapshot.agent_sessions,
                        snapshot.control,
                        cursor.clone(),
                        cx,
                    );
                })
                .is_ok();
            if !applied {
                return;
            }
            follow(api, session_id, Some(cursor), this, cx).await;
        }));
        self.load_commands(cx);
        cx.notify();
    }

    /// 把视图恢复成「还没打开任何会话」。
    ///
    /// 切会话与「列表里已经没有会话可显示」都走这里：两边要清的东西完全一样，漏一样就会把
    /// 上一个会话的现场带过来。
    fn reset_session_view(&mut self, cx: &mut Context<Self>) {
        // 先停掉旧的消费任务：否则上一个会话的残余增量会落到新状态上。
        self.stream_task = None;
        self.state.clear();
        self.markdown.clear();
        self.synced_text.clear();
        self.expanded.clear();
        self.preview_expanded.clear();
        self.ask_user.clear();
        // 恢复卡片按「当前会话 + 工具调用 id」建，切了会话就不再成立；全局问卷表则相反，
        // 它是跨会话的，不动。
        self.recovered_ask_user.clear();
        self.stream_connected = false;
        // 命令面跟着会话（工作目录）走：清空重取，别把上一个会话的命令留在面板里。
        self.commands.clear();
        self.commands_loading = false;
        // 状态栏项与待发队列同样跟着会话走：上一个会话的项目名、分支与排队中的输入
        // 带到新会话上都不成立（前端 `resetSessionView` 同口径）。
        self.status_items.clear();
        self.working_dir = None;
        self.delivery = DeliveryMode::default();
        self.queue = PendingQueue::default();
        self.queue_expanded = true;
        self.hint = None;
        self.flush_task = None;
        self.close_panels(cx);
        self.title = None;
        // 回合键换了会话就作废：上一个会话的「已复制」不该跟过来。
        self.copied = None;
        self.copy_timer = None;
        self.buffer = DeltaBuffer::new();
        self.error = None;
    }

    /// 清空会话面板：列表里已经没有任何可显示的会话了（会话或项目被删光）。
    ///
    /// 全局问卷表不动：它本来就是跨会话的，别的会话在等回答时这里照样要显示。
    pub fn clear_session(&mut self, cx: &mut Context<Self>) {
        self.reset_session_view(cx);
        self.session_id = None;
        cx.notify();
    }

    /// 应用首帧快照。
    fn apply_snapshot(
        &mut self,
        blocks: Vec<ConversationBlockDto>,
        agent_sessions: Vec<AgentSessionLinkDto>,
        control: ConversationControlStateDto,
        cursor: String,
        cx: &mut Context<Self>,
    ) {
        self.state.reset(blocks, agent_sessions, control, &cursor);
        self.after_state_change(cx);
    }

    /// 应用一批增量信封；一次唤醒里攒到的帧一次提交，只通知一次。
    fn apply_envelopes(
        &mut self,
        envelopes: Vec<ConversationStreamEnvelopeDto>,
        cx: &mut Context<Self>,
    ) {
        let mut flush = false;
        let mut rehydrate = false;
        let mut ask_user_changed = false;
        let mut commands_changed = false;
        for envelope in envelopes {
            // 扩展的实时事件不属于任何块，直接归并进全局问卷表。`GlobalLive` 只发给**别的**
            // 会话的流，所以这里看到的通常是别的会话刚问出的问卷。
            if let ConversationDeltaDto::CustomEvent {
                extension_id,
                event_type,
                payload,
                ..
            } = &envelope.delta
            {
                ask_user_changed |=
                    self.pending_ask_user
                        .apply_event(extension_id, event_type, payload);
            }
            // 压缩/分叉等结构性改写后服务端要求重拉全量快照（前端 `rehydrateRequired` 同源）。
            if matches!(&envelope.delta, ConversationDeltaDto::RehydrateRequired) {
                rehydrate = true;
            }
            // 扩展注册表变了，命令面就过期了；快捷键与状态栏项同样过期，但那两样还没落地。
            if matches!(
                &envelope.delta,
                ConversationDeltaDto::ExtensionRegistryChanged
            ) {
                commands_changed = true;
            }
            let cursor = envelope.cursor.value.clone();
            if self.buffer.push(&envelope.delta, Some(&cursor)) {
                flush = true;
            }
        }
        if commands_changed {
            self.load_commands(cx);
        }
        if flush || !self.buffer.is_empty() {
            let (deltas, cursor) = self.buffer.take();
            self.state.apply_batch(&deltas, cursor.as_deref());
            self.after_state_change(cx);
        } else if ask_user_changed {
            self.sync_recovered_cards(cx);
            cx.notify();
        } else {
            cx.notify();
        }
        // 压缩把整篇转录换掉了：批内的增量针对的是重写前的转录，重开当前会话取全量快照，
        // 「上下文已压缩」的摘要块随快照进来（前端 `rehydrateRequired → switchSession` 同口径）。
        // 重开自己会停掉旧的流任务，所以旧循环到此为止。
        if rehydrate && let Some(session_id) = self.session_id.clone() {
            self.open_session(session_id, cx);
        }
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message);
        cx.notify();
    }

    fn after_state_change(&mut self, cx: &mut Context<Self>) {
        self.sync_markdown(cx);
        self.sync_ask_user_cards(cx);
        // 块一变，「这条问卷有没有可见卡片」的答案就可能变。
        self.sync_recovered_cards(cx);
        self.prune_expanded();
        // 控制态一变就可能从「执行中」落回空闲：那是队列出队的时候。
        self.flush_queue(cx);
        self.sync_transcript_list();
        // 只在用户还贴着末尾时才钉到最新一条：往上翻看历史时，新到的输出不该把视角拽回去。
        // 「还贴着末尾」由列表的尾随模式维护——用户往上滚即暂停，滚回底部自动接上。
        if self.transcript.is_following_tail() {
            self.transcript.scroll_to_end();
        }
        cx.notify();
    }

    /// 把虚拟列表的项数与修订号同步到当前会话状态。
    ///
    /// 同步决策见 [`transcript_sync_plan`]；这里只负责执行。
    fn sync_transcript_list(&mut self) {
        let items = transcript_items(self.state.blocks());
        let count = items.len() + usize::from(needs_session_fork_row(&items));
        let revision = self.state.blocks_revision();
        let old_count = self.transcript.item_count();
        match transcript_sync_plan(old_count, count, revision, self.transcript_synced_revision) {
            TranscriptSync::None => {},
            TranscriptSync::Splice {
                at,
                replaced,
                count,
            } => {
                self.transcript.splice(at..at + replaced, count);
            },
            TranscriptSync::Remeasure => self.transcript.remeasure_items(0..count),
        }
        self.transcript_synced_revision = revision;
    }

    /// 跨会话问卷的轮询：只在事件流没连着时才去拉全局快照。
    ///
    /// 与前端 `PendingAskUserPoller` 同判据——流连着时全局事件本身就是权威，再问一次只是白跑；
    /// 没打开任何会话时这里照样在跑，那正是「别的会话在等回答」唯一能被看见的路径。
    fn spawn_pending_poll(api: Api, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(PENDING_ASK_USER_POLL_INTERVAL)
                    .await;
                let Ok(poll) = this.update(cx, |this, _| !this.stream_connected) else {
                    return;
                };
                if !poll {
                    continue;
                }
                let Ok(start_seq) = this.update(cx, |this, _| this.pending_ask_user.live_seq())
                else {
                    return;
                };

                let fetch = api.pending_ask_user_questions();
                let timeout = cx
                    .background_executor()
                    .timer(PENDING_ASK_USER_POLL_TIMEOUT);
                futures_util::pin_mut!(fetch, timeout);
                let futures_util::future::Either::Left((response, _)) =
                    futures_util::future::select(fetch, timeout).await
                else {
                    // 超时：这一趟拿不到就算了，下一趟再来。
                    continue;
                };
                let Ok(response) = response else {
                    // 端点不可用（扩展没装、server 换了）等：保留现有条目，等下一趟。
                    continue;
                };
                let Some(snapshot) = pending_ask_user::decode_snapshot(&response) else {
                    continue;
                };
                if this
                    .update(cx, |this, cx| {
                        this.pending_ask_user.merge_snapshot(snapshot, start_seq);
                        this.sync_recovered_cards(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
    }

    /// 让恢复卡片追上全局问卷表：当前会话里没有可见工具块的问卷才需要恢复。
    ///
    /// 卡片自带作答态，不能每帧重建：这里原地刷新，问卷没变就不通知（`set_block` 自己判断）。
    fn sync_recovered_cards(&mut self, cx: &mut Context<Self>) {
        let live: Vec<(String, ConversationBlockDto)> = match self.session_id.as_deref() {
            Some(session_id) => {
                let blocks = self.state.blocks();
                self.pending_ask_user
                    .for_session(session_id)
                    .filter(|question| !pending_ask_user::has_visible_block(blocks, question))
                    .map(|question| (question.call_id.clone(), question.recovered_block()))
                    .collect()
            },
            None => Vec::new(),
        };

        for (call_id, block) in &live {
            match self.recovered_ask_user.get(call_id) {
                Some(card) => {
                    card.update(cx, |card, cx| card.set_block(block, cx));
                },
                None => {
                    let Some(session_id) = self.session_id.clone() else {
                        return;
                    };
                    let card =
                        cx.new(|cx| AskUserCard::new(self.api.clone(), session_id, block, cx));
                    self.recovered_ask_user.insert(call_id.clone(), card);
                },
            }
        }
        self.recovered_ask_user
            .retain(|id, _| live.iter().any(|(key, _)| key == id));
    }

    /// 让每张问卷卡片追平它的工具块；块消失后卡片一起丢掉。
    ///
    /// 卡片自带交互态，不能每帧重建：这里的更新是原地刷新，只在块真的变化时才通知。
    fn sync_ask_user_cards(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let api = self.api.clone();
        let live: Vec<(String, ConversationBlockDto)> = self
            .state
            .blocks()
            .iter()
            .filter(|block| ask_user::is_ask_user(block))
            .map(|block| (block_id(block).to_owned(), block.clone()))
            .collect();

        for (id, block) in &live {
            match self.ask_user.get(id) {
                Some(card) => {
                    card.update(cx, |card, cx| card.set_block(block, cx));
                },
                None => {
                    let card =
                        cx.new(|cx| AskUserCard::new(api.clone(), session_id.clone(), block, cx));
                    self.ask_user.insert(id.clone(), card);
                },
            }
        }
        self.ask_user
            .retain(|id, _| live.iter().any(|(key, _)| key == id));
    }

    /// 让每个块的 markdown 状态追平它的当前文本。
    ///
    /// 用户与助手的正文都走 markdown（与 Web 前端一致）；`set_text` 在「新文本是旧文本
    /// 的延伸」时退化为追加，因此流式追加不会重解析整篇。
    fn sync_markdown(&mut self, cx: &mut Context<Self>) {
        // 每块一份 markdown 状态：正文用块 id，思考另占一份键（见 `thinking_key`）。
        let mut keys: Vec<String> = Vec::new();
        let mut changed: Vec<(String, String)> = Vec::new();
        for block in self.state.blocks() {
            let id = block_id(block);
            let text = match block {
                ConversationBlockDto::User { text, .. } => text.clone(),
                ConversationBlockDto::Assistant { .. } => {
                    for (index, text) in thinking_texts(block).into_iter().enumerate() {
                        let key = thinking_key(id, index);
                        if self.text_changed(&key, &text) {
                            changed.push((key.clone(), text));
                        }
                        keys.push(key);
                    }
                    // 正文只取可见部分：旧会话的正文里可能还留着内联思考标记。
                    visible_text(block)
                },
                _ => continue,
            };
            if self.text_changed(id, &text) {
                changed.push((id.to_owned(), text));
            }
            keys.push(id.to_owned());
        }
        for (id, text) in &changed {
            match self.markdown.get(id) {
                Some(state) => {
                    state.update(cx, |state, cx| state.set_text(text, cx));
                },
                None => {
                    let state = cx.new(|cx| TextViewState::markdown(text, cx).selectable(true));
                    self.markdown.insert(id.clone(), state);
                },
            }
            self.synced_text.insert(id.clone(), text.clone());
        }
        // 被移除的块（如失败的 transient 流）不再保留 markdown 状态。
        self.markdown.retain(|id, _| keys.contains(id));
        self.synced_text.retain(|id, _| keys.contains(id));
    }

    /// 这段文本是否还没同步给 markdown。
    fn text_changed(&self, id: &str, text: &str) -> bool {
        self.synced_text.get(id).map(String::as_str) != Some(text)
    }

    /// 折叠区是否展开：用户设过就以用户的为准，否则按块的当前状态推导。
    fn is_expanded(&self, block_id: &str, default_open: bool) -> bool {
        self.expanded.get(block_id).copied().unwrap_or(default_open)
    }

    fn toggle_expanded(&mut self, block_id: String, default_open: bool, cx: &mut Context<Self>) {
        let open = !self.is_expanded(&block_id, default_open);
        self.expanded.insert(block_id, open);
        cx.notify();
    }

    /// 块消失后丢掉它的折叠态，与 markdown 状态同寿命。
    fn prune_expanded(&mut self) {
        let live: Vec<&str> = self.state.blocks().iter().map(block_id).collect();
        let is_live = |key: &str| live.contains(&key_owner(key));
        self.expanded.retain(|key, _| is_live(key));
        self.preview_expanded.retain(|key, _| is_live(key));
    }


    /// 把一段转录文本写进剪贴板，按钮随即切成「已复制」，两秒后落回。
    fn copy_run(&mut self, key: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(key.to_owned());
        self.copy_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_LABEL_HOLD).await;
            this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// 分叉会话：`at` 是源会话里的持久化点，为空时从末尾分叉。
    ///
    /// 新会话的展示交给外壳——会话列表在它手里，切过去与标题注入都由它做。
    fn fork_session(&mut self, at: Option<u64>, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let api = self.api.clone();
        cx.spawn(
            async move |this, cx| match api.fork_session(&session_id, at).await {
                Ok(forked) => {
                    this.update(cx, |_, cx| cx.emit(ChatEvent::SessionForked(forked)))
                        .ok();
                },
                Err(error) => {
                    this.update(cx, |this, cx| this.fail(error.to_string(), cx))
                        .ok();
                },
            },
        )
        .detach();
    }

    /// 顶栏显示的对象名；由外壳在切会话时注入。
    pub fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        self.title = Some(title.into());
        cx.notify();
    }

    /// 由外壳注入当前会话的工作目录；状态行上的项目名取它末段。
    ///
    /// Web 宿主交空串（浏览器无从得知服务端的启动目录），那时状态行只画「本地」。
    pub fn set_working_dir(&mut self, working_dir: Option<String>, cx: &mut Context<Self>) {
        if self.working_dir == working_dir {
            return;
        }
        self.working_dir = working_dir;
        cx.notify();
    }

    /// 会话列表里的显示名；由外壳在刷新列表时注入，横幅靠它把别的会话写成名字而不是 id。
    pub fn set_session_titles(&mut self, titles: HashMap<String, String>, cx: &mut Context<Self>) {
        self.session_titles = titles;
        cx.notify();
    }

    /// 按输入区当前的文本与光标重算两块面板。
    ///
    /// 只在文本真的变了（`InputEvent::Change`）时才被叫到，所以每次都全量判定：斜杠触发生效时
    /// 参数补全作废，退出触发区间就收起面板——与前端 `updateSlashTrigger` 同一顺序。
    fn refresh_panels(&mut self, cx: &mut Context<Self>) {
        let (text, caret) = {
            let state = self.input.read(cx);
            (state.value().to_string(), state.cursor())
        };

        if let Some(trigger) = slash_command::find_slash_trigger(&text, caret) {
            self.argument_panel = None;
            self.arg_task = None;
            let opened = match &self.command_panel {
                Some(panel) => panel.trigger != trigger,
                None => true,
            };
            if opened {
                self.command_panel = Some(CommandPanel {
                    trigger,
                    selected: 0,
                });
                // 命令面随会话（工作目录）与扩展加载变化，每次开面板都重取一次。
                self.load_commands(cx);
            }
            cx.notify();
            return;
        }
        self.command_panel = None;

        let trigger = slash_command::find_arg_trigger(&text, caret, &self.commands);
        if self.argument_panel.as_ref().map(|panel| &panel.trigger) == trigger.as_ref() {
            return;
        }
        self.arg_task = None;
        self.argument_panel = trigger.map(|trigger| ArgumentPanel {
            trigger,
            items: Vec::new(),
            truncated: false,
            loading: true,
            selected: 0,
        });
        if self.argument_panel.is_some() {
            self.start_arg_fetch(text, cx);
        }
        cx.notify();
    }

    /// 收起两块面板，并取消在飞的参数补全取数。
    fn close_panels(&mut self, cx: &mut Context<Self>) {
        if self.command_panel.is_none() && self.argument_panel.is_none() {
            return;
        }
        self.command_panel = None;
        self.argument_panel = None;
        self.arg_task = None;
        cx.notify();
    }

    /// 拉取当前会话的斜杠命令列表。
    fn load_commands(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        if self.commands_loading {
            return;
        }
        self.commands_loading = true;
        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            let loaded = api.list_commands(&session_id).await;
            this.update(cx, |this, cx| {
                this.commands_loading = false;
                // 慢响应跮了会话就丢掉：命令面跟着会话的工作目录走。
                if this.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                match loaded {
                    Ok(response) => {
                        this.commands = response.commands;
                        this.status_items = response.status_items;
                    },
                    Err(error) => tracing::warn!(%error, "拉取斜杠命令失败"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 重取宿主配置（当前选区与权限模式）。
    ///
    /// 后一次调用换掉前一次的任务，于是慢响应盖不住新结果——对应前端 `modelRefreshKey`
    /// 那次 effect 重跑里的取消标记。
    fn load_config(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.config_task = Some(cx.spawn(async move |this, cx| {
            let config = api.config().await;
            this.update(cx, |this, cx| {
                match config {
                    Ok(config) => this.config = Some(config),
                    // 取不到就维持上一次的值：按钮还指着它。
                    Err(error) => tracing::warn!(%error, "拉取配置失败"),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 重取模型清单与当前选中项。
    ///
    /// 两样一起取：面板要拿当前项给那一行画勾，触发器上要写当前模型的名字。
    /// 后一次调用换掉前一次的任务，于是慢响应盖不住新结果。
    pub(crate) fn load_models(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.models_loading = true;
        self.models_task = Some(cx.spawn(async move |this, cx| {
            let (models, current) =
                futures_util::future::join(api.list_models(), api.current_model()).await;
            this.update(cx, |this, cx| {
                this.models_loading = false;
                match models {
                    Ok(models) => this.models = models,
                    // 取不到就维持上一次的清单：面板画的是它。
                    Err(error) => tracing::warn!(%error, "拉取模型清单失败"),
                }
                if let Ok(current) = current {
                    this.current_model = Some(current);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 配置在别处（设置页）被改过：配置视图与模型清单一起重取。
    ///
    /// 工具条上的模型按钮与权限按钮都读这两份，因此必须一起刷新——对应前端
    /// `bumpModelRefreshKey` 让那个 effect 重跑一次。
    pub(crate) fn refresh_model_config(&mut self, cx: &mut Context<Self>) {
        self.load_config(cx);
        self.load_models(cx);
    }

    /// 开合模型面板：打开时清空搜索词、重取一次清单，并把焦点交给搜索框。
    ///
    /// 清单还在拉时不开：面板里会是空的，而前端在那一刻也把触发器置灰（`disabled={loading}`）。
    fn toggle_model_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.model_panel_open && self.models_loading {
            return;
        }
        self.model_panel_open = !self.model_panel_open;
        if self.model_panel_open {
            self.model_query
                .update(cx, |state, cx| state.set_value("", window, cx));
            self.load_models(cx);
            let handle = self.model_query.read(cx).focus_handle(cx).clone();
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn close_model_panel(&mut self, cx: &mut Context<Self>) {
        if !self.model_panel_open {
            return;
        }
        self.model_panel_open = false;
        cx.notify();
    }

    /// 选中一个模型：整套选区写回，权限模式与小模型一起带上。
    fn select_model(&mut self, profile_name: String, model_id: String, cx: &mut Context<Self>) {
        if self.model_saving {
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        let request = composer_config::selection_request(
            &config,
            &profile_name,
            &model_id,
            config.approval_mode,
        );
        self.model_panel_open = false;
        self.model_saving = true;
        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            let result = api.update_active_selection(&request).await;
            this.update(cx, |this, cx| {
                this.model_saving = false;
                match result {
                    Ok(_) => {
                        // 两处都要追平：配置里那份当前选区（权限开关拿它当底），
                        // 和模型面板的当前项。
                        this.load_config(cx);
                        this.load_models(cx);
                    },
                    Err(error) => this.fail(error.to_string(), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 切换工具权限模式：同样是整套选区请求，当前模型原样带上。
    fn toggle_approval(&mut self, cx: &mut Context<Self>) {
        if self.approval_saving {
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        let request = composer_config::selection_request(
            &config,
            &config.active_profile,
            &config.active_model,
            composer_config::toggled_approval(config.approval_mode),
        );
        self.approval_saving = true;
        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            let result = api.update_active_selection(&request).await;
            this.update(cx, |this, cx| {
                this.approval_saving = false;
                match result {
                    Ok(_) => this.load_config(cx),
                    Err(error) => this.fail(error.to_string(), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 参数补全：防抖后取一次候选。
    ///
    /// 每换一个触发上下文就换掉上一个任务，于是上一次请求的响应落不到新面板上：前端靠自增
    /// 序号丢弃过期响应，这里用任务句柄的取消。
    fn start_arg_fetch(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(trigger) = self
            .argument_panel
            .as_ref()
            .map(|panel| panel.trigger.clone())
        else {
            return;
        };
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let api = self.api.clone();
        self.arg_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(ARG_COMPLETION_DEBOUNCE)
                .await;
            let argument = text
                .get(trigger.argument_start..trigger.cursor)
                .unwrap_or_default();
            // 服务端的 `cursor` 是参数字符数（`complete_command` 的默认值就是这么算的）。
            let result = api
                .complete_command(
                    &session_id,
                    &trigger.command_name,
                    argument,
                    argument.chars().count(),
                )
                .await;
            this.update(cx, |this, cx| {
                let Some(panel) = &mut this.argument_panel else {
                    return;
                };
                // 触发上下文换过了：这份结果属于上一次输入。
                if panel.trigger != trigger {
                    return;
                }
                panel.loading = false;
                match result {
                    Ok(response) => {
                        panel.items = response.items;
                        panel.truncated = response.truncated;
                        panel.selected = 0;
                    },
                    Err(error) => {
                        tracing::warn!(%error, "参数补全失败");
                        panel.items = Vec::new();
                        panel.truncated = false;
                    },
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 面板开着时接管按键；返回真表示这次按键已被面板消化。
    ///
    /// 没人可选项时不接管：回车仍归输入区（与前端一致——过滤为空时它不拦按键）。
    fn handle_panel_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // 模型面板不靠输入区的焦点活：它一展开，焦点就交给搜索框了。
        if self.model_panel_open {
            if keystroke.key.as_str() == "escape" {
                self.close_model_panel(cx);
                return true;
            }
            return false;
        }
        if !self.input_focused || (self.command_panel.is_none() && self.argument_panel.is_none()) {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" => {
                self.close_panels(cx);
                true
            },
            "up" => {
                self.move_panel_selection(false, cx);
                true
            },
            "down" => {
                self.move_panel_selection(true, cx);
                true
            },
            // Tab 与回车选中当前项；Shift+Enter 仍归输入区换行。
            "tab" => self.accept_panel_selection(window, cx),
            "enter" if !keystroke.modifiers.shift => self.accept_panel_selection(window, cx),
            _ => false,
        }
    }

    /// 上下移动选中项，两端环绕；空面板不动。
    fn move_panel_selection(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = match (&self.command_panel, &self.argument_panel) {
            (Some(panel), _) => {
                slash_command::visible_commands(&self.commands, &panel.trigger.query).len()
            },
            (None, Some(panel)) => panel.items.len(),
            (None, None) => return,
        };
        if count == 0 {
            return;
        }
        let step = |selected: usize| {
            if forward {
                (selected + 1) % count
            } else {
                (selected + count - 1) % count
            }
        };
        if let Some(panel) = self.command_panel.as_mut() {
            panel.selected = step(panel.selected);
        } else if let Some(panel) = self.argument_panel.as_mut() {
            panel.selected = step(panel.selected);
        }
        cx.notify();
    }

    /// 选中当前高亮的那一行；没有可选项时返回假，把这次按键还给输入区。
    fn accept_panel_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).value().to_string();
        let edit = match (&self.command_panel, &self.argument_panel) {
            (Some(panel), _) => {
                slash_command::visible_commands(&self.commands, &panel.trigger.query)
                    .get(panel.selected)
                    .map(|command| {
                        slash_command::slash_insert(&text, &panel.trigger, &command.name)
                    })
            },
            (None, Some(panel)) => panel
                .items
                .get(panel.selected)
                .map(|item| slash_command::arg_insert(&text, &panel.trigger, &item.insert_text)),
            (None, None) => None,
        };
        let Some((next, caret)) = edit else {
            return false;
        };
        self.close_panels(cx);
        self.apply_input(next, caret, window, cx);
        // 插入后光标可能正好落进参数补全的区间（选中 `/name` 之后就是），立刻重算一次。
        self.refresh_panels(cx);
        true
    }

    /// 把一段新文本与光标写回输入区。
    ///
    /// `set_value` 会把多行输入的选择重置到 `0..0` 且不发 `Change` 事件，所以光标得单独设、
    /// 面板得手工重算（见 [`ChatView::accept_panel_selection`]）。
    fn apply_input(
        &mut self,
        text: String,
        caret: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_selected_range(caret..caret, cx);
            state.focus(window, cx);
        });
    }

    /// 提交输入区里的这条文本。
    ///
    /// 两条路：空闲时直接提交；忙的时候按投递模式注入当前 turn 或排进待发队列。例外是
    /// `/compact` 与已注册的斜杠命令——它们由宿主就地处理，忙时照旧直接提交，否则
    /// 「现在就想做的事」要等一整个 turn。
    fn submit(
        &mut self,
        input: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let text = input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        if !self.can_submit() {
            return;
        }
        // 提交即收起面板：这条文本已经交出去了，面板里的候选不再成立。
        self.close_panels(cx);
        // 上一次提交留下的提示到这里已经过期。
        self.hint = None;
        input.update(cx, |state, cx| state.set_value("", window, cx));

        let compact = slash_command::is_compact_command(&text, &self.commands);
        let registered = slash_command::is_registered_command(&text, &self.commands);
        if !compact && !registered && self.is_executing() {
            self.deliver_while_busy(text, cx);
            return;
        }
        // 自己发的这条就是当下最新的一条：即使此前在翻历史，也把视角带回末尾。
        self.transcript.set_follow_mode(FollowMode::Tail);

        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            match api.submit_prompt(&session_id, &text).await {
                // `/compact` 会换掉整篇转录，而服务端只回一句「处理完了」，所以自己重拉一次快照。
                Ok(PromptSubmitResponse::Handled { .. }) if compact => {
                    this.update(cx, |this, cx| this.open_session(session_id, cx))
                        .ok();
                },
                Ok(_) => {},
                Err(error) => tracing::warn!(%error, "提交提示失败"),
            }
        })
        .detach();
        cx.notify();
    }

    /// 发送按钮是否可用。
    ///
    /// 与前端 `canSubmit` 同判据：有会话且不在压缩中。忙的时候照样能发——那正是待发队列的
    /// 入口；压缩期间整篇转录要换掉，这时排的队排不出正确结果。
    fn can_submit(&self) -> bool {
        self.session_id.is_some()
            && self
                .state
                .control()
                .is_none_or(|control| control.phase != PhaseDto::Compacting)
    }

    /// 会话是否正在执行（思考 / 生成 / 调用工具 / 压缩）。
    fn is_executing(&self) -> bool {
        self.state
            .control()
            .is_some_and(|control| composer_queue::is_execution_phase(control.phase))
    }

    /// 忙的时候这一条往哪去：注入当前 turn，或排进待发队列。
    fn deliver_while_busy(&mut self, text: String, cx: &mut Context<Self>) {
        if self.delivery == DeliveryMode::Inject {
            if composer_queue::can_inject(self.state.control()) {
                self.inject(text, cx);
                return;
            }
            // 打不进就说一声，然后按 Queue 走：静默改道比报错更让人困惑。
            self.hint = Some("当前无法 inject，已改为加入队列".to_owned());
        }
        self.queue.push(text);
        cx.notify();
    }

    /// 把一条输入注入当前 turn。
    ///
    /// 服务端拒了（turn 刚结束、注入窗口已关）就把这条退回队列：它是用户敲的字，
    /// 不能因为一次失败的注入丢掉。
    fn inject(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            if let Err(error) = api.inject_message(&session_id, &text).await {
                tracing::warn!(%error, "注入输入失败");
                this.update(cx, |this, cx| {
                    this.hint = Some(format!("无法 inject，已改为加入队列：{error}"));
                    this.queue.push(text);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    /// 待发队列：丢掉一条。
    fn remove_pending(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.queue.remove(id) {
            cx.notify();
        }
    }

    /// 待发队列：「编辑」——把这一条的正文取回输入区。
    fn edit_pending(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.queue.take_text(id) else {
            return;
        };
        let caret = text.len();
        self.apply_input(text, caret, window, cx);
        // `set_value` 不发 `Change`，面板要手工重算一次（与斜杠命令插入同一个原因）。
        self.refresh_panels(cx);
        cx.notify();
    }

    /// 待发队列：「重发」——立刻提交一次；失败放回队列。
    fn resend_pending(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let Some(message) = self
            .queue
            .items()
            .iter()
            .find(|message| message.id == id)
            .cloned()
        else {
            return;
        };
        self.queue.remove(id);
        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            if let Err(error) = api.submit_prompt(&session_id, &message.text).await {
                tracing::warn!(%error, "重发排队消息失败");
                this.update(cx, |this, cx| {
                    this.hint = Some(format!("重发失败：{error}"));
                    this.queue.restore(message);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    /// 待发队列：「Inject」——把这一条立刻注入当前 turn（不改投递模式）。
    fn inject_pending(&mut self, id: &str, cx: &mut Context<Self>) {
        if !composer_queue::can_inject(self.state.control()) {
            self.hint = Some("当前 turn 已结束，无法 inject；消息会保留在 queue 中".to_owned());
            cx.notify();
            return;
        }
        let Some(text) = self.queue.take_text(id) else {
            return;
        };
        self.inject(text, cx);
    }

    /// 空闲下来就把队列里的输入依次提交。
    ///
    /// 失败的一条放回队列并留提示，其余继续——与前端「单条失败不阻塞整批」一致。
    /// 不同的是这里只在会话状态变化时才冲：前端的清单长度一变就会再冲一次，失败时会
    /// 反复重试；排队消息失败通常意味着服务端不认这条输入，重试只是刷日志。
    fn flush_queue(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        // 一趟冲刷在飞时不再起第二趟：`cx.spawn` 的句柄被换掉会取消上一趟，
        // 而上一趟的 `pending` 已经不在队列里了，取消就等于把那几条输入丢掉。
        if self.is_executing() || self.queue.is_empty() || self.flush_task.is_some() {
            return;
        }
        let pending = self.queue.drain();
        let api = self.api.clone();
        self.flush_task = Some(cx.spawn(async move |this, cx| {
            for message in pending {
                if let Err(error) = api.submit_prompt(&session_id, &message.text).await {
                    tracing::warn!(%error, "排队消息提交失败");
                    this.update(cx, |this, cx| {
                        this.hint = Some(format!("排队消息发送失败：{error}"));
                        this.queue.restore(message);
                        cx.notify();
                    })
                    .ok();
                }
            }
            this.update(cx, |this, cx| {
                this.flush_task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn abort(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let api = self.api.clone();
        cx.spawn(async move |_this, _cx| {
            if let Err(error) = api.abort(&session_id).await {
                tracing::warn!(%error, "中止 turn 失败");
            }
        })
        .detach();
        cx.notify();
    }

    fn resolve_approval(
        &mut self,
        call_id: String,
        decision: ApprovalDecisionDto,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let api = self.api.clone();
        cx.spawn(async move |_this, _cx| {
            if let Err(error) = api.resolve_approval(&session_id, &call_id, decision).await {
                tracing::warn!(%error, "提交审批结果失败");
            }
        })
        .detach();
        cx.notify();
    }

    /// 重算会话里命中的转录项，并把列表滚到第一处。
    ///
    /// 会话内容是已经在手的块，直接扫一遍就行，不必像代码搜索那样走服务端。命中的单位是
    /// 「项」而不是「处」：一项里命中五次也只算一项——跳转要落到的是一条消息。
    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        let needle = self.search.needle(cx);
        let case_sensitive = self.search.case_sensitive();
        // 先把各一项的文本取出来：下面要改自身的字段，借不了 `self.state`。
        let texts: Vec<String> = transcript_items(self.state.blocks())
            .iter()
            .map(transcript_item_text)
            .collect();
        self.search_matches.clear();
        if !needle.is_empty() {
            for (index, text) in texts.iter().enumerate() {
                if !find::literal_matches(text, &needle, case_sensitive).is_empty() {
                    self.search_matches.push(index);
                }
            }
        }
        self.search.rewind();
        self.search.set_count(self.search_matches.len());
        self.scroll_to_search_match();
        cx.notify();
    }

    /// 把转录滚到当前命中那一项。
    fn scroll_to_search_match(&self) {
        let Some(index) = self.search_matches.get(self.search.current()) else {
            return;
        };
        self.transcript.scroll_to(ListOffset {
            item_ix: *index,
            offset_in_item: px(0.),
        });
    }

    /// 跳到下一处/上一处命中。
    fn step_search(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.search.advance(forward);
        self.scroll_to_search_match();
        cx.notify();
    }

    fn toggle_search_case(&mut self, cx: &mut Context<Self>) {
        self.search.toggle_case();
        self.refresh_search(cx);
    }

    /// 清空查询串（查找栏右端那枚按钮）。
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.clear(window, cx);
        self.refresh_search(cx);
    }

    fn render_transcript(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.session_id.is_none() {
            return self.render_placeholder("从左侧选择一个会话", cx);
        }

        let items = transcript_items(self.state.blocks());
        if items.is_empty() {
            return self.render_placeholder("还没有消息，发送一条提示开始", cx);
        }

        // 虚拟列表：渲染闭包只对可见项（含 overdraw）调用，离屏块不再构建元素树。
        // `sync_transcript_list` 已在 `render` 入口同步过项数与测量标记。
        let view = cx.entity();
        list(self.transcript.clone(), move |ix, _window, cx| {
            view.update(cx, |this, cx| this.render_transcript_item(ix, cx))
        })
        .flex_1()
        .pt_6()
        .pb_2()
        // 横向 gutter 不能挂在 list 上：列表只把纵向 padding 计入项定位，横向的等于没有。
        .into_any_element()
    }

    /// 渲染第 `ix` 个转录项；计数里为「分叉当前会话」入口预留的位置就是 `items.len()`。
    ///
    /// 项与项之间的间距由这里的 `pb_4` 承担（list 不是 flex 容器，没有 gap），
    /// 尾项多出的 16px 由列表的 `pb_2` 抵回，总底边距与旧的 `py_6` 容器一致。
    ///
    /// 外层必须 `w_full`：项是以「可用宽度 = 列表宽」的根盒量出来的，不撑满就退化成
    /// fit-content，气泡的 `self_end`/`ml_auto` 无处靠右，整条消息会贴左。横向 gutter
    /// 同样只能挂在这一层。
    fn render_transcript_item(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let items = transcript_items(self.state.blocks());
        let item = match items.get(ix) {
            Some(TranscriptItem::Block(block)) => self.render_block(block, cx),
            Some(TranscriptItem::Run(run)) => self.render_run(run, cx),
            None => self.render_fork_row(cx),
        };
        let mut row = v_flex().w_full().px_6().pb_4();
        // 查找的当前命中：整项铺一层强调底色。正文里的具体那一段没有单独标记——正文与工具
        // 结果各由自己的视图渲染，不给外部高亮留位置。
        if self.search_matches.get(self.search.current()) == Some(&ix) {
            row = row.bg(cx.theme().accent).rounded(cx.theme().radius);
        }
        row.child(item).into_any_element()
    }

    /// 跨会话待回答问卷的横幅：当前会话丢了工具块的给恢复卡片，其余会话给一行可点的入口。
    ///
    /// 与前端的 `PendingAskUserBanner` 同处：顶栏之下、转录之上，而且不在转录的滚动区里——
    /// 它说的是「别处有人在等回答」，与当前会话滚到哪里无关。
    fn render_pending_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.pending_ask_user.is_empty() {
            return div().into_any_element();
        }
        let current = self.session_id.as_deref();
        let recovered: Vec<&PendingQuestion> = match current {
            Some(session_id) => self
                .pending_ask_user
                .for_session(session_id)
                .filter(|question| {
                    !pending_ask_user::has_visible_block(self.state.blocks(), question)
                })
                .collect(),
            None => Vec::new(),
        };
        let others: Vec<&PendingQuestion> = self.pending_ask_user.others(current).collect();
        if recovered.is_empty() && others.is_empty() {
            return div().into_any_element();
        }

        let mut banner = v_flex()
            .gap_2()
            .px_6()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted);
        for question in recovered {
            banner = banner.child(self.render_recovered_card(question, cx));
        }
        for question in others {
            banner = banner.child(self.render_pending_row(question, cx));
        }
        banner.into_any_element()
    }

    /// 恢复出来的问卷卡片；实体由 `sync_recovered_cards` 兜底创建，这一支只是渲染顺序的退路。
    fn render_recovered_card(&self, question: &PendingQuestion, cx: &App) -> AnyElement {
        let card = match self.recovered_ask_user.get(&question.call_id) {
            Some(card) => card.clone().into_any_element(),
            None => return div().into_any_element(),
        };
        div()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().primary.alpha(0.3))
            .p_3()
            .child(card)
            .into_any_element()
    }

    /// 别的会话在等回答：整行可点，点了切过去。
    fn render_pending_row(&self, question: &PendingQuestion, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .session_titles
            .get(&question.session_id)
            .cloned()
            // 列表里没有它（比如刚分叉出的子会话）时退回短 id，与前端同口径。
            .unwrap_or_else(|| question.session_id.chars().take(8).collect());
        let text = match question.questions.first() {
            Some(first) => format!("会话「{title}」有问题待回答：{}", first.question),
            None => format!("会话「{title}」有问题待回答"),
        };
        let session_id = question.session_id.clone();
        let hover_background = cx.theme().list_hover;
        let accent = cx.theme().primary;
        h_flex()
            .id(SharedString::from(format!(
                "pending-ask-user-{}:{}",
                question.session_id, question.call_id
            )))
            .items_center()
            .gap_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(accent.alpha(0.3))
            .px_3()
            .py_1()
            .hover(move |this| this.bg(hover_background))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(ChatEvent::OpenSession(session_id.clone()));
            }))
            .child(IconName::Spark.element(Size::Small).text_color(accent))
            .child(div().flex_1().min_w_0().truncate().text_sm().child(text))
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("切换会话 →"),
            )
            .into_any_element()
    }

    /// 空态提示：一段居中说明，不额外套容器。
    fn render_placeholder(&self, message: &'static str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(message)
            .into_any_element()
    }

    fn render_block(&self, block: &ConversationBlockDto, cx: &mut Context<Self>) -> AnyElement {
        match block {
            ConversationBlockDto::User { id, text, .. } => {
                let body: AnyElement = match self.markdown.get(id) {
                    Some(state) => TextView::new(state).into_any_element(),
                    // markdown 状态由 sync_markdown 兜底创建，这一支只是渲染顺序的退路。
                    None => div().child(text.clone()).into_any_element(),
                };
                Bubble::new()
                    .alignment(MessageAlignment::End)
                    .with_variant(BubbleVariant::Secondary)
                    .child(div().max_w(px(560.)).child(body))
                    .into_any_element()
            },
            // 助手块与工具调用在转录里总是成组出现（见 `assistant_run`）；这一支只是把匹配
            // 穷尽掉，单个块按只有一段的回合渲染。
            ConversationBlockDto::Assistant { .. } | ConversationBlockDto::ToolCall { .. } => {
                self.render_run(&Run::new(block_id(block), std::slice::from_ref(block)), cx)
            },

            // 纯文本提示块：命令输出、回合回顾、错误。三者共用一条路径——正文显示什么，
            // 复制按钮就复制什么。正文本身不给选区，理由见 `render_note_actions`。
            ConversationBlockDto::Error { message, .. }
            | ConversationBlockDto::Recap { text: message, .. }
            | ConversationBlockDto::SystemNote { text: message, .. } => {
                let is_error = matches!(block, ConversationBlockDto::Error { .. });
                let container = if is_error {
                    v_flex()
                        .border_l_2()
                        .border_color(cx.theme().danger)
                        .pl_3()
                        .py_1()
                } else {
                    v_flex()
                };
                let body = if is_error {
                    div().text_color(cx.theme().danger)
                } else {
                    div().text_sm().text_color(cx.theme().muted_foreground)
                };
                container
                    .child(body.child(message.clone()))
                    .child(self.render_note_actions(block_id(block), message.clone(), cx))
                    .into_any_element()
            },
            ConversationBlockDto::CompactSummary {
                summary,
                pre_tokens,
                post_tokens,
                ..
            } => v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("上下文已压缩：{pre_tokens} → {post_tokens} tokens")),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(summary.clone()),
                )
                .into_any_element(),
        }
    }

    /// 提示块的复制入口：一键把整块文本写进剪贴板，按钮随即切成「已复制」。
    ///
    /// 这些块是纯文本、不走 markdown，所以没有可选中的正文——选区只在 markdown 格式的
    /// `TextView` 上（见 `sync_markdown`），改用它会把命令输出的对齐与围栏重排一遍，
    /// 展示就变了。因此这里给的是显式入口，而不是选区。
    fn render_note_actions(&self, key: &str, text: String, cx: &mut Context<Self>) -> AnyElement {
        let copied = self.copied.as_deref() == Some(key);
        let copy_key = key.to_owned();
        h_flex()
            .items_center()
            .gap_1()
            .pt_1()
            .child(
                Button::new(SharedString::from(format!("note-copy-{key}")))
                    .ghost()
                    .small()
                    .icon(IconName::Copy.element(Size::Small))
                    .label(if copied { "已复制" } else { "复制" })
                    .tooltip("复制这段文本")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_run(&copy_key, text.clone(), cx);
                    })),
            )
            .into_any_element()
    }

    /// 一次助手回合：过程段收成一行摘要，正文段直接铺开，末尾挂回合级动作。
    fn render_run(&self, run: &Run<'_>, cx: &mut Context<Self>) -> AnyElement {
        let mut column = v_flex().gap_3();
        for segment in &run.segments {
            column = column.child(match segment {
                RunSegment::Content(block) => self.render_assistant_content(block),
                // 待回答的问卷提到折叠区外：收起来等于没问。
                RunSegment::Process(process) => v_flex()
                    .gap_2()
                    .children(
                        process
                            .prompts
                            .iter()
                            .map(|block| self.render_ask_user_card(block_id(block), cx)),
                    )
                    .child(self.render_process_segment(process, cx))
                    .into_any_element(),
            });
        }
        if let Some(actions) = &run.actions {
            column = column.child(self.render_run_actions(&run.key, actions, cx));
        }
        column.into_any_element()
    }

    /// 完成答复下方的一行：复制这一回合，或从这一回合分叉。
    ///
    /// 分叉按钮只在块里带持久化点时才出现——分叉点就是那个 seq，没有它只能复制。
    fn render_run_actions(
        &self,
        key: &str,
        actions: &RunActions,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let copied = self.copied.as_deref() == Some(key);
        let copy_key = key.to_owned();
        let copy_text = actions.copy_text.clone();
        let mut row = h_flex().items_center().gap_1().pt_1().child(
            Button::new(SharedString::from(format!("run-copy-{key}")))
                .ghost()
                .small()
                .icon(IconName::Copy.element(Size::Small))
                .label(if copied { "已复制" } else { "复制" })
                .tooltip("复制此 Turn")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy_run(&copy_key, copy_text.clone(), cx);
                })),
        );
        if let Some(at) = actions.fork_at {
            row = row.child(
                Button::new(SharedString::from(format!("run-fork-{key}")))
                    .ghost()
                    .small()
                    .icon(IconName::Branch.element(Size::Small))
                    .label("分叉")
                    .tooltip("从此 Turn 分叉")
                    .on_click(cx.listener(move |this, _, _, cx| this.fork_session(Some(at), cx))),
            );
        }
        row.into_any_element()
    }

    /// 转录末尾的会话级分叉入口；末项自己给不出分叉按钮时才出现。
    fn render_fork_row(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .items_center()
            .py_1()
            .child(
                Button::new("fork-session")
                    .ghost()
                    .small()
                    .icon(IconName::Branch.element(Size::Small))
                    .label("分叉当前会话")
                    .on_click(cx.listener(|this, _, _, cx| this.fork_session(None, cx))),
            )
            .into_any_element()
    }

    /// 问卷卡片：状态由 `sync_ask_user_cards` 兜底创建，这一支只是渲染顺序的退路。
    fn render_ask_user_card(&self, call_id: &str, _cx: &mut Context<Self>) -> AnyElement {
        match self.ask_user.get(call_id) {
            Some(card) => card.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// 回合里的助手正文：markdown 状态由 `sync_markdown` 兜底创建。
    fn render_assistant_content(&self, block: &ConversationBlockDto) -> AnyElement {
        match self.markdown.get(block_id(block)) {
            Some(state) => TextView::new(state).into_any_element(),
            // 渲染顺序的退路：状态还没建起来时先把可见正文铺出来。
            None => div().child(visible_text(block)).into_any_element(),
        }
    }

    /// 一段处理过程：一行摘要（整行可点）+ 展开后的思考与工具行。
    ///
    /// 段内有待决定的事时强制展开，而且点也不收起——审批按钮藏在折叠区里等于没给。
    fn render_process_segment(
        &self,
        process: &ProcessSegment<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 只剩问卷的过程段没有摘要行可渲染：卡片已经单独提出来了。
        if process.entries.is_empty() {
            return div().into_any_element();
        }
        let open = process.has_attention || self.is_expanded(&process.id, false);
        let chevron = IconName::ChevronRight.element(Size::Small);
        let chevron = if open {
            chevron.rotate(radians(std::f32::consts::FRAC_PI_2))
        } else {
            chevron
        };
        let hover_background = cx.theme().list_hover;
        let title_color = if process.has_error() {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        let force_open = process.has_attention;
        let toggle_id = process.id.clone();

        let mut summary = h_flex()
            .id(SharedString::from(format!("process-{}", process.id)))
            .items_center()
            .gap_2()
            .w_full()
            .py_1()
            .rounded(cx.theme().radius)
            .hover(move |this| this.bg(hover_background))
            .on_click(cx.listener(move |this, _, _, cx| {
                if force_open {
                    this.expanded.insert(toggle_id.clone(), true);
                    cx.notify();
                    return;
                }
                this.toggle_expanded(toggle_id.clone(), false, cx);
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(title_color)
                    .child(if process.has_error() {
                        "处理失败".to_owned()
                    } else {
                        process.title()
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(process.latest_label()),
            );

        if process.entries.len() > 1 {
            summary = summary.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} 项", process.entries.len())),
            );
        }

        v_flex()
            .child(summary.child(chevron.text_color(cx.theme().muted_foreground)))
            .children(open.then(|| self.render_process_body(process, cx)))
            .into_any_element()
    }

    /// 过程段的正文：左侧一道竖线，里面是思考与工具行。
    fn render_process_body(
        &self,
        process: &ProcessSegment<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body = v_flex()
            .mt_2()
            .pb_1()
            .pl_3()
            .gap_3()
            .border_l_2()
            .border_color(cx.theme().border);
        for entry in &process.entries {
            body = body.child(match entry {
                ProcessEntry::Thinking { key, .. } => self.render_thinking(key, cx),
                ProcessEntry::Tool(activity) => self.render_activity_row(activity, cx),
            });
        }
        body.into_any_element()
    }

    /// 一段思考：同样走 markdown，颜色收敛到次级。
    fn render_thinking(&self, key: &str, cx: &mut Context<Self>) -> AnyElement {
        match self.markdown.get(key) {
            Some(state) => div()
                .min_w_0()
                .text_color(cx.theme().muted_foreground)
                .child(TextView::new(state))
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// 工具行：图标 + 活动文案 +（展开后）详情面板。
    ///
    /// 展开开关就是这一行本身，与前端把整行做成 `<summary>` 一致：收起时它要能单独读完
    /// 一次调用，展开后细节才出现。
    fn render_activity_row(
        &self,
        activity: &ToolActivity<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let block = activity.block;
        let id = block_id(block);
        // 等待审批时才默认展开：那是待决定的事。执行中不自动展开——与前端一致，
        // 否则一次 turn 里卡片会随状态反复自开自合，转录列跟着跳。
        let default_open = matches!(
            block,
            ConversationBlockDto::ToolCall {
                approval: Some(_),
                ..
            }
        );
        let open = self.is_expanded(id, default_open);
        let chevron = IconName::ChevronRight.element(Size::Small);
        let chevron = if open {
            chevron.rotate(radians(std::f32::consts::FRAC_PI_2))
        } else {
            chevron
        };
        let hover_background = cx.theme().list_hover;
        let toggle_id = id.to_owned();

        let mut header = h_flex()
            .id(SharedString::from(format!("activity-{id}")))
            .items_center()
            .gap_2()
            .w_full()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .hover(move |this| this.bg(hover_background))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_expanded(toggle_id.clone(), default_open, cx);
            }))
            .child(
                activity_icon(activity.kind)
                    .element(Size::Small)
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(activity_color(activity, cx))
                    .child(activity.label.clone()),
            );

        for (value, color) in [
            (
                activity.insertions.map(|count| format!("+{count}")),
                cx.theme().success,
            ),
            (
                activity.deletions.map(|count| format!("-{count}")),
                cx.theme().danger,
            ),
        ] {
            if let Some(value) = value {
                header = header.child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(color)
                        .child(value),
                );
            }
        }
        // 运行中的子 Agent 用它的当前工具顶掉工具行的状态文字（前端 `streamingStatusText` 同序）。
        let runtime = agent_session::runtime_label(self.state.agent_session_for_tool_call(id))
            .or_else(|| runtime_label(activity));
        if let Some(runtime) = runtime {
            header = header.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(runtime),
            );
        }

        v_flex()
            .child(header.child(chevron.text_color(cx.theme().muted_foreground)))
            .children(open.then(|| self.render_tool_details(block, cx)))
            .into_any_element()
    }

    /// 工具卡的详情面板：ID、参数、摘要，以及结果（结果位可能要换成审批卡）。
    fn render_tool_details(
        &self,
        block: &ConversationBlockDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ConversationBlockDto::ToolCall {
            id,
            arguments,
            status,
            approval,
            arguments_json,
            ..
        } = block
        else {
            return div().into_any_element();
        };

        let arguments = format_arguments(arguments_json.as_ref(), arguments);
        let mut rows = v_flex()
            .px_2()
            .pb_2()
            .gap_3()
            .child(detail_row("ID", mono_text(id.clone(), cx), cx))
            .child(detail_row("参数", mono_text(arguments.clone(), cx), cx));

        let summary = summary_line(block);
        if let Some(summary) = summary.filter(|summary| *summary != arguments) {
            rows = rows.child(detail_row("摘要", mono_text(summary, cx), cx));
        }

        // 等待审批时结果是待决定的事，审批卡顶掉结果位。
        let gate_pending = matches!(status, ToolCallStatusDto::Streaming) && approval.is_some();
        let result = match (gate_pending, approval) {
            (true, Some(approval)) => self.render_approval(id, approval, cx),
            // 问卷的答案是问答对，交回给卡片自己排——结果里的 JSON 不是给人看的。
            _ if ask_user::is_ask_user(block) => self.render_ask_user_card(id, cx),
            // 计划列表也一样：进度项才是给人看的，结果文本只是「更新了几项」。
            _ if todo_list::has_items(block) => self.render_todo_card(block, cx),
            // 子 Agent 卡：只在工具调用还在流式、且确实挂了子会话时出现（与前端同判据）。
            _ if matches!(status, ToolCallStatusDto::Streaming)
                && agent_session::is_agent_tool(block) =>
            {
                match self.state.agent_session_for_tool_call(id) {
                    Some(session) => self.render_agent_card(id, session, cx),
                    None => self.render_tool_result(block, cx),
                }
            },
            _ => self.render_tool_result(block, cx),
        };
        rows.child(detail_row("结果", result, cx))
            .into_any_element()
    }

    /// 结果：元信息行 + 正文。错误与取消各给一层语义色描边。
    fn render_tool_result(
        &self,
        block: &ConversationBlockDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ConversationBlockDto::ToolCall { status, .. } = block else {
            return div().into_any_element();
        };

        let border = match status {
            ToolCallStatusDto::Failed => cx.theme().danger.alpha(0.25),
            ToolCallStatusDto::Cancelled => cx.theme().warning.alpha(0.25),
            _ => cx.theme().border,
        };
        let mut content = v_flex().gap_3();
        let rows = meta_rows(block);
        if !rows.is_empty() {
            let mut grid = v_flex().gap_1();
            for (label, value) in rows {
                grid = grid.child(
                    h_flex()
                        .items_baseline()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(label),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_xs()
                                .child(value),
                        ),
                );
            }
            content = content.child(grid);
        }

        // 补丁卡在元信息与正文之间多一段逐文件的应用情况（与前端 `PatchToolDetails` 同序）。
        if matches!(tool_view(block), ToolView::Patch) {
            content = content.child(self.render_patch_files(block, cx));
        }

        div()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(border)
            .bg(cx.theme().muted)
            .px_3()
            .py_2()
            .child(content.child(self.render_tool_body(block, cx)))
            .into_any_element()
    }

    /// 补丁卡的逐文件应用情况：每条一行标签 + 路径 + 失败原因，超出上限收一行计数。
    fn render_patch_files(
        &self,
        block: &ConversationBlockDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let files = patch_files(block);
        if files.is_empty() {
            return div().into_any_element();
        }

        let mut list = v_flex().gap_1();
        for file in files.iter().take(PATCH_FILES_SHOWN) {
            let path = if file.path.is_empty() {
                "(unknown path)".to_owned()
            } else {
                file.path.clone()
            };
            let mut row = h_flex()
                .items_baseline()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if file.applied {
                            cx.theme().success
                        } else {
                            cx.theme().danger
                        })
                        .child(file.label.to_uppercase()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .child(path),
                );
            if !file.error.is_empty() {
                row = row.child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(file.error.clone()),
                );
            }
            list = list.child(row);
        }
        if files.len() > PATCH_FILES_SHOWN {
            list = list.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("+{} more files", files.len() - PATCH_FILES_SHOWN)),
            );
        }
        list.into_any_element()
    }

    /// 计划卡：整卡一段纯文本，对应前端的 `todoWrite` 渲染器。
    ///
    /// 每项一行、行首是状态词（等宽字体下三个状态词等宽，天然对齐），不带进度条、计数网格和
    /// 逐行元素：卡片是转录里最长、出现次数最多的块，多一个元素就多一份重排成本；计数已经由
    /// 卡片头的摘要行给出，状态由文字表达、不依赖颜色。
    fn render_todo_card(&self, block: &ConversationBlockDto, cx: &mut Context<Self>) -> AnyElement {
        let mut text = String::new();
        for item in todo_list::items(block) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(item.status.label());
            text.push(' ');
            text.push_str(&item.label);
        }

        div()
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .text_color(cx.theme().foreground)
            .child(text)
            .into_any_element()
    }

    /// 子 Agent 卡：身份与状态一行、任务一行，运行中给「查看子会话」，收尾后给摘要或错误。
    ///
    /// 状态与阶段都来自 `agentSessionUpdated` 增量，快照里的链接只有状态没有阶段，所以回放
    /// 旧会话看不到阶段与当前工具。
    fn render_agent_card(
        &self,
        call_id: &str,
        session: &AgentSession,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let running = session.status == AgentSessionStatusDto::Running;
        let tone = match session.status {
            AgentSessionStatusDto::Running => cx.theme().primary,
            AgentSessionStatusDto::Completed => cx.theme().success,
            AgentSessionStatusDto::Failed => cx.theme().danger,
        };

        let mut header = h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().primary)
                    .child("子 AGENT"),
            )
            .child(div().text_xs().child(session.agent_name.clone()))
            .child(
                div()
                    .rounded_full_style(cx)
                    .px_2()
                    .py_0p5()
                    .text_xs()
                    .text_color(tone)
                    .bg(tone.alpha(0.15))
                    .child(agent_session::status_label(session.status)),
            );
        if running {
            if let Some(phase) = session.phase {
                header = header.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(agent_session::phase_label(phase)),
                );
            }
            if let Some(tool) = &session.current_tool {
                header = header.child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("→ {tool}")),
                );
            }
        }

        let mut card = v_flex()
            .gap_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .p_3()
            .child(header)
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(session.task.clone()),
            );

        if running {
            let child_session_id = session.child_session_id.clone();
            card = card.child(
                Button::new(SharedString::from(format!("agent-open-{call_id}")))
                    .outline()
                    .small()
                    .label("查看子会话")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ChatEvent::OpenSession(child_session_id.clone()));
                    })),
            );
        }

        // 摘要与错误各归各自的终态：失败时归并层已经把摘要清掉了，这里也不必再让位。
        if session.status == AgentSessionStatusDto::Completed
            && let Some(summary) = &session.summary
        {
            card = card.child(
                div()
                    .id(SharedString::from(format!("agent-summary-{call_id}")))
                    .max_h(px(192.))
                    .overflow_y_scroll()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().background)
                    .p_2()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary.clone()),
            );
        }
        if session.status == AgentSessionStatusDto::Failed
            && let Some(error) = &session.error
        {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }

        card.into_any_element()
    }

    /// 结果正文：文件变更、读取内容、搜索结果、命令输出或通用文本。
    fn render_tool_body(&self, block: &ConversationBlockDto, cx: &mut Context<Self>) -> AnyElement {
        let ConversationBlockDto::ToolCall { status, .. } = block else {
            return div().into_any_element();
        };
        let streaming = matches!(status, ToolCallStatusDto::Streaming);
        let id = block_id(block);

        match detail_body(block) {
            DetailBody::Empty => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if streaming {
                    "等待输出…"
                } else {
                    "（无输出）"
                })
                .into_any_element(),
            DetailBody::Text(text, kind) => self.render_preview(id, 0, text, kind, cx),
            DetailBody::Replacement { old, new } => v_flex()
                .gap_2()
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("替换前"),
                        )
                        .child(self.render_preview(id, 0, old, PreviewKind::Plain, cx)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("替换后"),
                        )
                        .child(self.render_preview(id, 1, new, PreviewKind::Plain, cx)),
                )
                .into_any_element(),
        }
    }

    /// 一段正文预览：默认只给开头若干行，其余收在「显示完整输出」后面。
    ///
    /// `slot` 区分同一个块里的多段正文，它和块 id 一起构成预览的展开态键。
    fn render_preview(
        &self,
        block_id: &str,
        slot: usize,
        text: &str,
        kind: PreviewKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("{block_id}:{slot}");
        let expanded = self.preview_expanded.get(&key).copied().unwrap_or(false);
        let (preview, omitted) =
            truncate_preview(text, TOOL_PREVIEW_MAX_CHARS, TOOL_PREVIEW_MAX_LINES);
        let body = if expanded { text } else { preview };

        let mut column = v_flex()
            .gap_2()
            .child(self.render_code_lines(body, kind, cx));
        if omitted > 0 {
            let toggle_key = key.clone();
            column = column.child(
                Button::new(SharedString::from(format!("preview-{key}")))
                    .ghost()
                    .small()
                    .label(if expanded {
                        "收起完整输出".to_owned()
                    } else {
                        format!("显示完整输出 · 另有 {omitted} 字符")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let current = this
                            .preview_expanded
                            .get(&toggle_key)
                            .copied()
                            .unwrap_or(false);
                        this.preview_expanded.insert(toggle_key.clone(), !current);
                        cx.notify();
                    })),
            );
        }
        column.into_any_element()
    }

    /// 逐行渲染正文。diff 按行的语义着色，带行号的内容把它拆到左侧行号列。
    fn render_code_lines(
        &self,
        text: &str,
        kind: PreviewKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lines: Vec<&str> = text.lines().take(TOOL_MAX_RENDERED_LINES).collect();
        let numbered =
            kind == PreviewKind::Numbered && lines.iter().any(|line| numbered_line(line).is_some());
        let mut column = v_flex()
            .py_1()
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs();

        for line in &lines {
            let (gutter, code) = match numbered.then(|| numbered_line(line)).flatten() {
                Some((number, code)) => (number, code),
                None => ("", *line),
            };
            let mut row = h_flex().w_full().gap_2().min_w_0();
            if !gutter.is_empty() {
                row = row.child(
                    div()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(gutter.to_owned()),
                );
            }
            if kind == PreviewKind::Diff {
                let (color, background) = diff_colors(diff_line_kind(line), cx);
                row = row.bg(background).text_color(color);
            }
            column = column.child(row.child(div().min_w_0().child(if code.is_empty() {
                " ".to_owned()
            } else {
                code.to_owned()
            })));
        }

        if text.lines().count() > TOOL_MAX_RENDERED_LINES {
            column = column.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("（只渲染前 {TOOL_MAX_RENDERED_LINES} 行）")),
            );
        }
        column.into_any_element()
    }


    /// 一次审批：提示 + 四个决定。
    ///
    /// 允许一次是主操作，总是拒绝是持续性的破坏决定，两者各占视觉一端；中间两个保持安静。
    fn render_approval(
        &self,
        call_id: &str,
        approval: &ToolApprovalDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let decisions = [
            ("allow-once", "允许一次", ApprovalDecisionDto::AllowOnce),
            ("allow-always", "总是允许", ApprovalDecisionDto::AllowAlways),
            ("deny-once", "拒绝一次", ApprovalDecisionDto::DenyOnce),
            ("deny-always", "总是拒绝", ApprovalDecisionDto::DenyAlways),
        ];

        let mut row = h_flex().gap_2();
        for (suffix, label, decision) in decisions {
            let call_id = call_id.to_string();
            let id = SharedString::from(format!("{suffix}-{call_id}"));
            let button = match decision {
                ApprovalDecisionDto::AllowOnce => Button::new(id).primary(),
                ApprovalDecisionDto::AllowAlways | ApprovalDecisionDto::DenyOnce => {
                    Button::new(id).outline()
                },
                ApprovalDecisionDto::DenyAlways => Button::new(id).danger(),
            };
            row = row.child(
                button
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.resolve_approval(call_id.clone(), decision, cx);
                    })),
            );
        }

        v_flex()
            .gap_2()
            .child(div().text_sm().child(approval.prompt.clone()))
            .child(row)
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .title
            .clone()
            .unwrap_or_else(|| SharedString::from("会话"));
        let phase = self
            .state
            .control()
            .map(|control| phase_label(control.phase))
            .unwrap_or("未连接");

        let mut header = page_header(cx);
        // 侧边栏收起时给一条回到它的路（前端同样只在收起时显示这枚按钮）。
        if !self.sidebar_open {
            header = header.child(icon_button(
                "chat-expand-sidebar",
                IconName::Sidebar,
                "展开侧边栏",
                cx,
                |_this: &mut ChatView, cx| cx.emit(ChatEvent::ToggleSidebar),
            ));
        }
        header
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(phase),
            )
            .child(div().flex_1())
            .child(find::render_find_bar(
                &self.search,
                search_actions(),
                "清空查找",
                cx,
            ))
            .into_any_element()
    }

    /// 工具权限模式开关：完全访问与请求批准是同一枚按钮的两态。
    fn render_approval_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(config) = &self.config else {
            return div().into_any_element();
        };
        let mode = config.approval_mode;
        Toggle::new("approval-mode")
            .checked(mode == ApprovalModeDto::Yolo)
            .icon(IconName::Shield.element(Size::Small))
            .label(composer_config::approval_label(mode))
            .tooltip(composer_config::approval_hint(mode))
            .disabled(self.approval_saving)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_approval(cx)))
            .into_any_element()
    }

    /// 模型选择：触发器 + 浮在它上方、左对齐的浮层面板。
    ///
    /// 面板跟着触发器走（`relative` + `bottom_full`），不像命令面板那样撑满输入区——
    /// 前端这里也是 `absolute bottom-[calc(100%+8px)] left-0`，宽 240px。
    fn render_model_selector(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.model_panel_open.then(|| {
            div()
                .id("model-panel")
                .absolute()
                .left_0()
                .bottom_full()
                .mb_2()
                // 收起监听挂在**面板自己**身上，不挂在外层：`on_mouse_down_out` 看的是元素
                // 自己的边界，而面板是绝对定位的——挂在外层会连「点面板里面」也算成外面。
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_model_panel(cx)))
                .child(self.render_model_panel(cx))
        });
        div()
            .id("model-selector")
            .relative()
            .child(self.render_model_trigger(cx))
            .children(panel)
            .into_any_element()
    }

    /// 模型触发器：当前模型名 + 展开箭头。
    fn render_model_trigger(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.model_panel_open;
        let (text_color, background) = if open {
            (cx.theme().foreground, cx.theme().list_hover)
        } else {
            (cx.theme().muted_foreground, cx.theme().transparent)
        };
        let hover_bg = cx.theme().list_hover;
        let hover_text = cx.theme().foreground;
        let label =
            composer_config::current_model_label(self.current_model.as_ref(), self.models_loading);

        let mut trigger = h_flex()
            .id("model-trigger")
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .text_sm()
            .text_color(text_color)
            .bg(background)
            .hover(move |this| this.bg(hover_bg).text_color(hover_text))
            .on_click(cx.listener(move |this, _, window, cx| {
                // 点触发器时 `on_mouse_down_out`（捕获阶段，早于这里）已经把面板收起来了；
                // 只在状态没被动过时才翻面，否则一次点击会又收又开——与 gpui-base `Popover`
                // 里的 `state.is_open() == open` 是同一个防重复手法。
                if this.model_panel_open == open {
                    this.toggle_model_panel(window, cx);
                }
            }))
            .child(
                div()
                    .min_w_0()
                    .max_w(px(MODEL_LABEL_MAX_WIDTH))
                    .truncate()
                    .child(label.to_owned()),
            )
            .child(IconName::ChevronDown.element(Size::Small));
        // 清单还在拉时置灰，与前端 `disabled:opacity-60` 同效果。
        if self.models_loading {
            trigger = trigger.opacity(0.6);
        }
        trigger.into_any_element()
    }

    /// 模型面板：搜索框 + 按 profile 分组的模型行。
    fn render_model_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.model_query.read(cx).value().to_string();
        let groups = composer_config::model_groups(&self.models, &query);

        let mut list = v_flex()
            .id("model-list")
            .p_1()
            .max_h(px(MODEL_PANEL_MAX_HEIGHT))
            .overflow_y_scroll();
        if groups.is_empty() {
            list = list.child(panel_note(
                composer_config::empty_model_note(!self.models.is_empty()).to_owned(),
                cx,
            ));
        } else {
            for (index, group) in groups.iter().enumerate() {
                if index > 0 {
                    // 段与段之间留一口气，与前端 `gi > 0 ? 'mt-1.5'` 同口径。
                    list = list.child(div().h(px(6.)));
                }
                list = list.child(panel_group(
                    format!(
                        "{} · {}",
                        group.profile_name,
                        composer_config::wire_format_label(group.wire_format)
                    ),
                    cx,
                ));
                for model in &group.models {
                    list = list.child(self.render_model_row(model, cx));
                }
            }
        }

        v_flex()
            .w(px(MODEL_PANEL_WIDTH))
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .child(
                div()
                    .p_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Input::new(&self.model_query)),
            )
            .child(list)
            .into_any_element()
    }

    /// 模型面板的一行：模型 id，选中那一项尾巴上打勾。
    fn render_model_row(&self, model: &AvailableModelDto, cx: &mut Context<Self>) -> AnyElement {
        let active = composer_config::is_current_model(self.current_model.as_ref(), model);
        let (text_color, background) = if active {
            (cx.theme().primary, cx.theme().list_hover)
        } else {
            (cx.theme().foreground, cx.theme().transparent)
        };
        let hover = cx.theme().list_hover;
        let profile_name = model.profile_name.clone();
        let model_id = model.model_id.clone();

        let mut row = h_flex()
            .id(SharedString::from(format!(
                "model-row-{profile_name}::{model_id}"
            )))
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .text_sm()
            .text_color(text_color)
            .bg(background)
            .hover(move |this| this.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_model(profile_name.clone(), model_id.clone(), cx);
            }))
            .child(div().min_w_0().truncate().child(model.model_id.clone()));
        if active {
            row = row.child(IconName::Check.element(Size::Small));
        }
        row.into_any_element()
    }


    /// 输入区上方的状态行：项目、本地、分支、重试、插件状态栏项与会话指标。
    ///
    /// 这些都不进滚动区，也不随输入变：它们说的是「这个会话在哪里、跑了多少」。
    fn render_status_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut row = h_flex()
            .w_full()
            .flex_wrap()
            .items_center()
            .gap_x_5()
            .gap_y_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground);

        if let Some(project) = self
            .working_dir
            .as_deref()
            .and_then(session_list::project_name_tail)
        {
            // 文件浏览的根目录就是这个会话的项目目录，入口因此贴着项目名放。
            row = row.child(
                Button::new("chat-files")
                    .ghost()
                    .small()
                    .label("文件")
                    .tooltip("浏览这个项目的文件")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::OpenFiles))),
            );
            row = row.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(IconName::Folder.element(Size::Small))
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(PROJECT_LABEL_MAX_WIDTH))
                            .truncate()
                            .child(project.to_owned()),
                    ),
            );
        }
        row = row.child(
            h_flex()
                .items_center()
                .gap_2()
                .child(IconName::Monitor.element(Size::Small))
                .child("本地"),
        );
        if let Some(branch) = self.branch_label() {
            row = row.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(IconName::Branch.element(Size::Small))
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(BRANCH_LABEL_MAX_WIDTH))
                            .truncate()
                            .child(branch.to_owned()),
                    ),
            );
        }
        if let Some(retry) = self
            .state
            .control()
            .and_then(|control| control.retry_status.as_ref())
        {
            row = row.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .text_color(cx.theme().warning)
                    .child(IconName::Retry.element(Size::Small))
                    .child(retry_text(retry)),
            );
        }
        for item in self
            .status_items
            .iter()
            .filter(|item| !is_branch_item(&item.id) && !item.text.is_empty())
        {
            row = row.child(
                div()
                    .min_w_0()
                    .max_w(px(STATUS_ITEM_MAX_WIDTH))
                    .truncate()
                    .child(item.text.clone()),
            );
        }
        if let Some(metrics) = self.state.metrics() {
            // 指标收成一条簇：标签压暗、数字等宽。原来是「输入 2.0K 缓存 50.0%」这样一串
            // 同色同重的句子，读数得在句子中间找，位数一变还会左右晃。
            let mut cluster = h_flex().flex_shrink_0().gap_3();
            for item in metrics::metrics_row(metrics) {
                cluster = cluster.child(
                    h_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(item.label),
                        )
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(cx.theme().foreground)
                                .child(item.value),
                        ),
                );
            }
            row = row.child(cluster);
        }

        row.into_any_element()
    }

    /// 分支名：插件状态栏项里有几个约定俗成的 id（与前端同样按这三种找）。
    fn branch_label(&self) -> Option<&str> {
        self.status_items
            .iter()
            .find(|item| is_branch_item(&item.id) && !item.text.is_empty())
            .map(|item| item.text.as_str())
    }

    /// 待发队列面板：收起时只有一行「N queued」。
    fn render_queue_panel(&self, can_inject: bool, cx: &mut Context<Self>) -> AnyElement {
        if self.queue.is_empty() {
            return div().into_any_element();
        }
        let chevron = IconName::ChevronDown.element(Size::Small);
        let chevron = if self.queue_expanded {
            chevron
        } else {
            chevron.rotate(radians(-std::f32::consts::FRAC_PI_2))
        };
        let hover_background = cx.theme().list_hover;
        let mut panel = v_flex().w_full().gap_1().child(
            h_flex()
                .id("queue-header")
                .items_center()
                .gap_1()
                .rounded(cx.theme().radius)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .hover(move |this| this.bg(hover_background))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.queue_expanded = !this.queue_expanded;
                    cx.notify();
                }))
                .child(chevron)
                .child(format!("{} queued", self.queue.len())),
        );
        if self.queue_expanded {
            let rows: Vec<AnyElement> = self
                .queue
                .items()
                .iter()
                .map(|message| self.render_queue_row(message, can_inject, cx))
                .collect();
            panel = panel.child(v_flex().w_full().children(rows));
        }
        panel.into_any_element()
    }

    /// 队列里的一条：正文 + 编辑 / 重发 / 注入 / 删除。
    fn render_queue_row(
        &self,
        message: &PendingMessage,
        can_inject: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = message.id.clone();
        let edit_id = id.clone();
        let resend_id = id.clone();
        let inject_id = id.clone();
        let remove_id = id.clone();

        h_flex()
            .items_center()
            .gap_3()
            .py_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(message.text.clone()),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("queue-edit-{id}")))
                            .ghost()
                            .small()
                            .icon(IconName::Edit.element(Size::Small))
                            .tooltip("编辑")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_pending(&edit_id, window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("queue-resend-{id}")))
                            .ghost()
                            .small()
                            .icon(IconName::Retry.element(Size::Small))
                            .tooltip("重发")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.resend_pending(&resend_id, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("queue-inject-{id}")))
                            .ghost()
                            .small()
                            .icon(IconName::Send.element(Size::Small))
                            .tooltip(if can_inject {
                                "Inject 到当前 turn"
                            } else {
                                "Agent 未在运行，无法 inject"
                            })
                            .disabled(!can_inject)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.inject_pending(&inject_id, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("queue-remove-{id}")))
                            .ghost()
                            .small()
                            .icon(IconName::Trash.element(Size::Small))
                            .tooltip("删除")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_pending(&remove_id, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// 投递模式开关：下一发送去注入当前 turn 还是排队。
    fn render_delivery_toggle(&self, can_inject: bool, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.delivery;
        // 强调色用 `primary`：本主题的 `accent` 是一块浅色背景，当文字色用会与底色糊在一起。
        let color = if mode == DeliveryMode::Inject && can_inject {
            cx.theme().primary
        } else {
            cx.theme().muted_foreground
        };

        Button::new("delivery-mode")
            .ghost()
            .small()
            .icon(IconName::Send.element(Size::Small))
            .label(mode.label())
            .tooltip(mode.hint(can_inject))
            .text_color(color)
            .on_click(cx.listener(|this, _, _, cx| {
                this.delivery = this.delivery.toggled();
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_input(&self, cx: &mut Context<Self>) -> AnyElement {
        let enabled = self.can_submit();
        let busy = self.is_executing();
        let can_inject = composer_queue::can_inject(self.state.control());
        let input = self.input.clone();

        // 发送与中止是同一组动作：忙的时候并排出现，不忙时只有发送。投递模式开关也只在这时
        // 有意义——空闲时每一条都是直接提交。
        let mut controls = h_flex()
            .items_center()
            .gap_3()
            .child(self.render_approval_toggle(cx))
            .child(self.render_model_selector(cx));
        if busy {
            controls = controls.child(self.render_delivery_toggle(can_inject, cx));
            controls = controls.child(
                Button::new("abort")
                    .outline()
                    .label("中止")
                    .on_click(cx.listener(|this, _, _, cx| this.abort(cx))),
            );
        }
        controls = controls.child(
            Button::new("send")
                .primary()
                .icon(IconName::Send.element(Size::Small))
                .tooltip("发送")
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.submit(&input, window, cx);
                })),
        );

        // 面板是浮层：浮在输入区上方而不是排进列里，否则每敲一个字转录都会跟着上下位移。
        let panel = div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_full()
            .mb_2()
            .flex()
            .justify_center()
            .children(
                self.command_panel
                    .as_ref()
                    .map(|panel| self.render_command_panel(panel, cx)),
            )
            .children(
                self.argument_panel
                    .as_ref()
                    .map(|panel| self.render_argument_panel(panel, cx)),
            );

        v_flex()
            .relative()
            .py_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                // 输入区与转录列共用同一条左右边界：同宽、同内缩。
                v_flex()
                    .w_full()
                    .gap_2()
                    .px_6()
                    .child(self.render_queue_panel(can_inject, cx))
                    .child(
                        // 会话元信息与控件合成一行：读的一侧在左，动的一侧在右。控件不另起
                        // 一行，输入区上方只留这一条。
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_3()
                            .child(div().flex_1().min_w_0().child(self.render_status_row(cx)))
                            .child(controls),
                    )
                    .child(div().w_full().min_w_0().child(Textarea::new(&self.input)))
                    .children(self.hint.as_ref().map(|hint| {
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(hint.clone())
                    }))
                    .children(self.error.as_ref().map(|error| {
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error.clone())
                    })),
            )
            .child(panel)
            .into_any_element()
    }

    /// 命令面板：一行一条命令，技能与插件各起一个小标题。
    fn render_command_panel(&self, panel: &CommandPanel, cx: &mut Context<Self>) -> AnyElement {
        let visible = slash_command::visible_commands(&self.commands, &panel.trigger.query);
        let mut list = v_flex().p_1();
        if visible.is_empty() {
            list = list.child(if self.commands_loading {
                panel_loading("加载中…", cx)
            } else {
                panel_note(format!("没有找到匹配「{}」的命令", panel.trigger.query), cx)
            });
        } else {
            let mut group: Option<bool> = None;
            for (index, command) in visible.iter().enumerate().take(COMMAND_PANEL_MAX_ROWS) {
                let skill = slash_command::is_skill_command(&command.extension_id);
                if group != Some(skill) {
                    list = list.child(panel_group(
                        if skill { "技能" } else { "插件" }.to_owned(),
                        cx,
                    ));
                    group = Some(skill);
                }
                list = list.child(self.render_command_row(
                    index,
                    command,
                    index == panel.selected,
                    cx,
                ));
            }
        }
        panel_shell(list.into_any_element(), cx)
    }

    /// 命令面板的一行：图标 + `/名字` + 描述。
    fn render_command_row(
        &self,
        index: usize,
        command: &SlashCommandInfoDto,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = cx.theme().primary;
        let hover = cx.theme().list_hover;
        let muted = cx.theme().muted_foreground;
        let (text_color, background) = if selected {
            (accent, hover)
        } else {
            (muted, cx.theme().transparent)
        };
        let icon = if slash_command::is_skill_command(&command.extension_id) {
            IconName::Zap
        } else {
            IconName::Terminal
        };
        h_flex()
            .id(SharedString::from(format!("command-row-{index}")))
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(background)
            .text_color(text_color)
            // 悬停只是提示可点：底色不与选中态相争。
            .hover(move |this| this.bg(hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(panel) = this.command_panel.as_mut() {
                    panel.selected = index;
                }
                this.accept_panel_selection(window, cx);
            }))
            .child(icon.element(Size::Small))
            .child(
                div()
                    .flex_shrink_0()
                    .text_sm()
                    .child(format!("/{}", command.name)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .child(command.description.clone()),
            )
            .into_any_element()
    }

    /// 参数补全面板：候选一行一条，末尾按需报截断。
    fn render_argument_panel(&self, panel: &ArgumentPanel, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex().p_1();
        if panel.items.is_empty() {
            list = list.child(if panel.loading {
                panel_loading("加载中…", cx)
            } else {
                panel_note("无补全建议".to_owned(), cx)
            });
        } else {
            for (index, item) in panel.items.iter().enumerate().take(COMMAND_PANEL_MAX_ROWS) {
                list =
                    list.child(self.render_argument_row(index, item, index == panel.selected, cx));
            }
            if panel.truncated {
                list = list.child(panel_note("结果过多，已截断".to_owned(), cx));
            }
        }
        panel_shell(list.into_any_element(), cx)
    }

    /// 参数补全的一行：候选文本在左，说明在右。
    fn render_argument_row(
        &self,
        index: usize,
        item: &CommandCompletionItemDto,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = cx.theme().primary;
        let hover = cx.theme().list_hover;
        let muted = cx.theme().muted_foreground;
        let (text_color, background) = if selected {
            (accent, hover)
        } else {
            (cx.theme().foreground, cx.theme().transparent)
        };
        h_flex()
            .id(SharedString::from(format!("argument-row-{index}")))
            .items_center()
            .justify_between()
            .gap_3()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(background)
            .text_color(text_color)
            .hover(move |this| this.bg(hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(panel) = this.argument_panel.as_mut() {
                    panel.selected = index;
                }
                this.accept_panel_selection(window, cx);
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(item.label.clone()),
            )
            .children(item.detail.as_ref().map(|detail| {
                div()
                    .flex_shrink_0()
                    .max_w(px(COMMAND_PANEL_MAX_WIDTH / 2.0))
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(detail.clone())
            }))
            .into_any_element()
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_transcript_list();
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(self.render_pending_banner(cx))
            // 必须是 flex 列容器：块容器会忽略子项的 `flex_1`，虚拟列表就只能拿到
            // 自身 padding 的高度（32px），贴底对齐后可见区里什么都画不出来。
            .child(v_flex().flex_1().min_h_0().child(self.render_transcript(cx)))
            .child(self.render_input(cx))
    }
}

/// 持续订阅会话事件流，直到实体被释放。
///
/// 服务端可能在 turn 结束后关闭流，所以正常结束也重订阅；重订阅前等待一个固定间隔，
/// 避免在服务端持续拒绝时打满 CPU。
async fn follow(
    api: Api,
    session_id: String,
    mut cursor: Option<String>,
    this: WeakEntity<ChatView>,
    cx: &mut AsyncApp,
) {
    loop {
        match api.subscribe(&session_id, cursor.as_deref()).await {
            Ok(mut stream) => {
                // 连着的时候全局问卷事件会自己送到，跨会话轮询让路。
                if this
                    .update(cx, |this, _| this.stream_connected = true)
                    .is_err()
                {
                    return;
                }
                loop {
                    match stream.next().await {
                        Ok(Some(envelope)) => {
                            cursor = Some(envelope.cursor.value.clone());
                            let mut batch = vec![envelope];
                            // 同一次网络读可能带来多帧，先把已解出的抽干再提交，
                            // 避免逐帧重渲染。
                            loop {
                                match stream.try_next() {
                                    Ok(Some(envelope)) => {
                                        cursor = Some(envelope.cursor.value.clone());
                                        batch.push(envelope);
                                    },
                                    Ok(None) => break,
                                    Err(error) => {
                                        this.update(cx, |this, cx| {
                                            this.fail(error.to_string(), cx);
                                        })
                                        .ok();
                                        break;
                                    },
                                }
                            }
                            if this
                                .update(cx, |this, cx| this.apply_envelopes(batch, cx))
                                .is_err()
                            {
                                return;
                            }
                        },
                        Ok(None) => break,
                        Err(error) => {
                            this.update(cx, |this, cx| this.fail(error.to_string(), cx))
                                .ok();
                            break;
                        },
                    }
                }
                if this
                    .update(cx, |this, _| this.stream_connected = false)
                    .is_err()
                {
                    return;
                }
            },
            Err(error) => {
                this.update(cx, |this, cx| this.fail(error.to_string(), cx))
                    .ok();
            },
        }
        cx.background_executor().timer(RECONNECT_DELAY).await;
    }
}

/// 状态栏项里哪几个 id 是分支名；这是插件与前端之间的约定。
/// 会话查找栏的几个动作，指到 [`ChatView`] 上的方法。
fn search_actions() -> find::FindActions<ChatView> {
    find::FindActions {
        toggle_case: |this, _, cx| this.toggle_search_case(cx),
        prev: |this, _, cx| this.step_search(false, cx),
        next: |this, _, cx| this.step_search(true, cx),
        close: |this, window, cx| this.clear_search(window, cx),
    }
}

fn is_branch_item(id: &str) -> bool {
    matches!(id, "git-branch" | "branch" | "gitBranch")
}

/// 重试状态那一项：远端状态码（连接中断时没有码），加上次数与退避时长。
///
/// 它是瞬态状态：重试成功或 turn 结束后服务端就不再上报，这一项随之消失。
fn retry_text(retry: &LlmRetryStatusDto) -> String {
    let head = match retry.status {
        Some(status) => format!("远端 {status}"),
        None => "连接中断".to_owned(),
    };
    format!(
        "{head} · 重试 {}/{} · 退避 {:.1} 秒",
        retry.attempt,
        retry.max_retries,
        retry.delay_ms as f64 / 1000.0
    )
}

/// 活动行的行首图标，与前端 `activityIconName` 同一张表。
fn activity_icon(kind: ActivityKind) -> IconName {
    match kind {
        ActivityKind::Command => IconName::Monitor,
        ActivityKind::Tool => IconName::Plug,
        ActivityKind::Created
        | ActivityKind::Edited
        | ActivityKind::Read
        | ActivityKind::Searched => IconName::Edit,
    }
}

/// 活动文案的颜色：失败的调用标红，其余用主题的强调前景色。
///
/// 不用 `accent`：在这个主题里 `accent` 是浅色底（深色模式下 neutral-800），当文字色等于
/// 看不见；`primary` 在深色下是 near-white，正是前端 `text-accent` 想表达的「这一行是重点」。
fn activity_color(activity: &ToolActivity<'_>, cx: &App) -> Hsla {
    if activity_failed(activity) {
        cx.theme().danger
    } else {
        cx.theme().primary
    }
}

/// 折叠与预览键归属的块 id：过程段去掉 `process:` 前缀，思考键与预览键看冒号前的那一段。
fn key_owner(key: &str) -> &str {
    let key = key.strip_prefix("process:").unwrap_or(key);
    key.split(':').next().unwrap_or(key)
}


/// 详情面板的一行：小号标签在上，内容在下。
fn detail_row(label: &'static str, value: AnyElement, cx: &App) -> AnyElement {
    v_flex()
        .gap_1()
        .min_w_0()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(value)
        .into_any_element()
}

/// 详情里的等宽文本：路径、参数、摘要都按字面显示。
fn mono_text(text: String, cx: &App) -> AnyElement {
    div()
        .min_w_0()
        .font_family(cx.theme().mono_font_family.clone())
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// 参数正文：优先结构化 JSON，读不出来才退回参数原文。
fn format_arguments(arguments_json: Option<&Value>, arguments: &str) -> String {
    if let Some(value) =
        arguments_json.filter(|value| value.as_object().is_some_and(|args| !args.is_empty()))
    {
        return serde_json::to_string_pretty(value).unwrap_or_else(|_| arguments.to_owned());
    }
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return "{}".to_owned();
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| trimmed.to_owned()),
        Err(_) => trimmed.to_owned(),
    }
}

/// 增删行底色的混色比例：语义色与主题底色预混出实底。
///
/// 取 0.18 而不是更低的透明度：底色要在深色主题上「一眼可辨」才算把增删说清楚了。
const TINT_ALPHA: f32 = 0.18;

/// diff 行的（文字色, 底色）。
///
/// 底色是语义色与主题底色预混出来的**不透明**色，而不是降透明度的语义色：半透明底色要求
/// 底下那一层真的参与合成，一旦画到别的东西、或那条合成路径没生效，整片底色就看不出来——
/// 而增删行恰恰必须一眼可辨。
///
/// 与 [`crate::views::code::diff`] 共用一份配色：两处画的是同一种东西（那里是双栏的格子底色）。
pub(crate) fn diff_colors(kind: DiffLineKind, cx: &App) -> (Hsla, Hsla) {
    let theme = cx.theme();
    match kind {
        DiffLineKind::Addition => (theme.success, tinted(theme.background, theme.success)),
        DiffLineKind::Deletion => (theme.danger, tinted(theme.background, theme.danger)),
        DiffLineKind::FileHeader => (theme.muted_foreground, theme.transparent),
        DiffLineKind::Hunk => (theme.foreground, theme.muted),
        DiffLineKind::Context => (theme.foreground, theme.transparent),
    }
}

/// 把语义色按 [`TINT_ALPHA`] 叠到主题底色上，得到不透明的行底色。
fn tinted(background: Hsla, color: Hsla) -> Hsla {
    background.blend(color.alpha(TINT_ALPHA))
}


/// 浮层外壳：宽度上限 + 边框 + 底色；面板浮在输入区上方，底色必须不透明。
fn panel_shell(list: AnyElement, cx: &App) -> AnyElement {
    div()
        .w_full()
        .max_w(px(COMMAND_PANEL_MAX_WIDTH))
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().popover)
        .child(list)
        .into_any_element()
}

/// 浮层里的一句提示（没有匹配、已截断）。
fn panel_note(text: String, cx: &App) -> AnyElement {
    div()
        .px_3()
        .py_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// 浮层里的加载态：转圈加一句话。
///
/// 转圈是这里唯一能表示「还在等」的东西：一句静态的「加载中…」与「没有结果」长得一模一样。
fn panel_loading(text: &str, cx: &App) -> AnyElement {
    h_flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(Spinner::new().small())
        .child(text.to_owned())
        .into_any_element()
}

/// 浮层里的小标题：命令面板的技能/插件，模型面板的 profile 段。
fn panel_group(label: String, cx: &App) -> AnyElement {
    div()
        .px_3()
        .pt_2()
        .pb_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

fn phase_label(phase: PhaseDto) -> &'static str {
    match phase {
        PhaseDto::Idle => "空闲",
        PhaseDto::Thinking => "思考中…",
        PhaseDto::Streaming => "生成中…",
        PhaseDto::CallingTool => "调用工具…",
        PhaseDto::Compacting => "压缩上下文中…",
        PhaseDto::Error => "出错",
    }
}

/// 虚拟列表的一次同步动作。
#[derive(Debug, PartialEq, Eq)]
enum TranscriptSync {
    /// 无需调整。
    None,
    /// 用 `count` 项替换 `[at, at + replaced)` 区间。
    Splice {
        at: usize,
        replaced: usize,
        count: usize,
    },
    /// 数量不变但内容变了：全部重测高度。
    Remeasure,
}

/// 根据修订号与项数变化决定虚拟列表的同步方式。
///
/// 修订号单调推进时的数量增长是流式落库的常规路径：尾部追加，保留已有项的
/// 测量高度。其余数量变化（清 transient、压缩重 hydrate、切会话——修订号可能
/// 回退）说明项的身份不可信，只能整体重建。数量不变而修订号变了是流式文本
/// 长高，重测但不重建：`remeasure_items` 保留旧高度作 size_hint，滚动不跳。
///
/// 注意 `ListState::splice(old_range, count)` 的总项数是「保留项 + count」，
/// 尾部追加必须传 `count - old_count` 个新项，而不是目标总数。
fn transcript_sync_plan(
    old_count: usize,
    count: usize,
    revision: u64,
    synced_revision: u64,
) -> TranscriptSync {
    if count == old_count {
        return if revision == synced_revision {
            TranscriptSync::None
        } else {
            TranscriptSync::Remeasure
        };
    }
    if count > old_count && revision >= synced_revision {
        TranscriptSync::Splice {
            at: old_count,
            replaced: 0,
            count: count - old_count,
        }
    } else {
        TranscriptSync::Splice {
            at: 0,
            replaced: old_count,
            count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TranscriptSync, transcript_sync_plan};

    #[test]
    fn streaming_append_adds_only_the_new_items() {
        // 流式落库：旧 3 项、新 4 项、修订号前进，只应追加 1 项。
        assert_eq!(
            transcript_sync_plan(3, 4, 6, 5),
            TranscriptSync::Splice {
                at: 3,
                replaced: 0,
                count: 1
            }
        );
    }

    #[test]
    fn first_load_of_a_session_builds_the_whole_list() {
        assert_eq!(
            transcript_sync_plan(0, 12, 1, 0),
            TranscriptSync::Splice {
                at: 0,
                replaced: 0,
                count: 12
            }
        );
    }

    #[test]
    fn switching_sessions_rebuilds_instead_of_appending() {
        // 切到更大的会话：修订号回退，不得走追加路径——否则项数变成 5 + 12。
        assert_eq!(
            transcript_sync_plan(5, 12, 1, 30),
            TranscriptSync::Splice {
                at: 0,
                replaced: 5,
                count: 12
            }
        );
        // 切到更小的会话：数量缩减，同样整体重建。
        assert_eq!(
            transcript_sync_plan(12, 5, 2, 1),
            TranscriptSync::Splice {
                at: 0,
                replaced: 12,
                count: 5
            }
        );
    }

    #[test]
    fn content_only_changes_remeasure_without_rebuilding() {
        assert_eq!(transcript_sync_plan(7, 7, 9, 8), TranscriptSync::Remeasure);
        assert_eq!(transcript_sync_plan(7, 7, 9, 9), TranscriptSync::None);
    }
}
