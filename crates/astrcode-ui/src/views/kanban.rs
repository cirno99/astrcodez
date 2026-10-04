//! 看板页：公共卡片区（四格）与日历区（每个时间桶两个手风琴项）。
//!
//! 两个区域合起来覆盖全部六列，不新增也不隐藏任何一列；列、槽位与落点的词汇都在
//! [`crate::kanban`]，这里只负责把它们摆上屏幕。拖拽沿用 gpui-kit 的框架拖拽
//! （`on_drag` / `on_drop`）：跨容器投递与幽灵都由框架负责，本页只在落点上判断
//! 「落在哪一列、哪一天」；落点高亮用 `drag_over` 的样式，不另存一份悬停状态。
//!
//! 与 Web 前端的差异（没做的部分按批次补齐，不在这里含糊过去）：
//! - 卡片上还没有列下拉框与「编辑」入口，改列目前只走拖拽，改标题要等编辑弹窗；
//! - 空日历列的两个槽位标签常显（前端悬停才显形）；
//! - 卡片没有绑定会话时点击无反馈，前端会弹一条提示；
//! - 多选（Ctrl/Shift）、空白处框选、按项目分组与整批删除都还没做。

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use chrono::Datelike as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, CursorStyle, DefiniteLength, Entity, EventEmitter,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    Window,
    component::{
        ActiveTheme as _, Disableable as _, Size, Theme,
        button::{Button, ButtonVariants as _},
        h_flex, v_flex,
    },
    div, px, relative,
};

use crate::{
    api::Api,
    icons::IconName,
    kanban::{
        CalendarBucket, CalendarScale, CalendarSlot, Card, CardColumn, CreateCardRequest,
        DropTarget, PUBLIC_AREA_COLUMNS, PUBLIC_AREA_DROP_COLUMNS, SlotCounts,
        UNSCHEDULED_BUCKET_KEY, UpdateCardRequest, anchor_label, bucket_key_of, buckets_for,
        card_day_key, day_key_from_iso, day_key_to_date, is_user_writable, opens_conversation,
        resolve_expanded_slot, shift_anchor_day_key, today_key,
    },
    views::{
        create_card::{CreateCardEvent, CreateCardModal},
        icon_button, page_header,
    },
};

/// 后台自动化会推进卡片，因此看板页需要周期性拉取而不是只加载一次。
const BOARD_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// 公共卡片区占页面宽度的比例；日历区吃掉剩下的部分。
const PUBLIC_AREA_FRACTION: f32 = 0.2;

/// 有内容的日历列：日历视口的四分之一，四列正好铺满。
const EXPANDED_COLUMN_FRACTION: f32 = 0.25;

/// 空日历列收缩到基准宽：既保留表格的连续性，又能一眼看出这一天没有卡片。
const COLLAPSED_COLUMN_WIDTH: f32 = 144.0;

/// 日历列头的固定高度；空列与非空列必须同高，横线才连得起来。
const CALENDAR_HEADER_HEIGHT: f32 = 40.0;

/// 卡片正文与备注折叠后的高度；`54px` 与前端 `max-h-[54px]` 一致。
const TEXT_COLLAPSED_HEIGHT: f32 = 54.0;

/// 日历里的星期；只给日刻度的列头用，不进 [`crate::kanban`] 的日期逻辑。
const WEEKDAY_LABELS: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

/// 看板页对外的事件。
#[derive(Debug, Clone)]
pub enum KanbanEvent {
    /// 用户点击卡片，要求跳到对应对话。
    OpenSession(String),
    /// 用户要求展开侧边栏（收起时页头上的那枚按钮）。
    ToggleSidebar,
    /// 新建卡片用过的目录：记进偏好（前端 `rememberProjectPath` 同口径），落盘在外壳。
    RememberProjectPath(String),
    /// 用户从候选里删掉一条目录：清历史并记进忽略集，落盘在外壳。
    ForgetProjectPath(String),
}

/// 拖拽载荷：被拖的那一张卡片。
///
/// 只带 id 与标题：5 秒轮询会换掉卡片对象，载荷里存快照就会拿旧数据回写。
#[derive(Clone)]
struct CardDrag {
    card_id: String,
    title: String,
}

/// 拖拽时跟着指针的幽灵。
struct CardGhost {
    title: String,
}

impl Render for CardGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .text_sm()
            .truncate()
            .child(self.title.clone())
    }
}

/// 日历一个时间桶里两个手风琴项的卡片。
#[derive(Debug, Default, PartialEq, Eq)]
struct BucketCards<'a> {
    backlog: Vec<&'a Card>,
    done: Vec<&'a Card>,
}

impl<'a> BucketCards<'a> {
    fn counts(&self) -> SlotCounts {
        SlotCounts {
            backlog: self.backlog.len(),
            done: self.done.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.backlog.is_empty() && self.done.is_empty()
    }

    fn slot(&self, slot: CalendarSlot) -> &[&'a Card] {
        match slot {
            CalendarSlot::Backlog => &self.backlog,
            CalendarSlot::Done => &self.done,
        }
    }
}

pub struct KanbanView {
    api: Api,
    cards: Vec<Card>,
    error: Option<String>,
    scale: CalendarScale,
    anchor_day_key: String,
    /// 日历横向滚动的位置；「回到今天」靠它把今天那一列带回视口。
    calendar_scroll: ScrollHandle,
    /// 每个日历列里被点开的手风琴项；表里没有这一列表示两项对半展开。
    preferred_slot: HashMap<String, CalendarSlot>,
    /// 被手动展开的卡片；默认全部折叠，因此空表就是「全部收起」。
    expanded: HashSet<String>,
    /// 被手动展开的长文本，键是 `<卡片 id>:body` / `<卡片 id>:note`。
    text_expanded: HashSet<String>,
    /// 长文本在折叠态下量出来的高度（px）；展开态不重量，因此记着上一次的结果。
    text_height: HashMap<String, f32>,
    /// 当前指针悬停的卡片；操作行默认隐身，只有它显形。
    hovered_card: Option<String>,
    /// 一次拉取或写操作的句柄；换一次即取消上一次，避免慢响应盖掉新结果。
    request_task: Option<Task<()>>,
    /// 建卡片的请求句柄；与刷新分开，否则 5 秒轮询会把中途的建卡掐掉。
    create_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    /// 页面是否正在显示；隐藏时不轮询。
    visible: bool,
    /// 侧边栏是否显示；由外壳建视图后告知，收起时页头给展开入口。
    sidebar_open: bool,
    /// 打开着的「新建卡片」弹窗。
    create_card: Option<CreateCardDialog>,
    /// 新建卡片弹窗要用的路径来源；由外壳推过来。
    path_sources: KanbanPathSources,
}

/// 打开着的「新建卡片」弹窗。
///
/// 订阅与弹窗同生共死：弹窗每次打开都重建（输入框要 `Window`），订阅跟着它一起换掉，
/// 不必单独退订。
struct CreateCardDialog {
    modal: Entity<CreateCardModal>,
    _subscription: Subscription,
}

/// 新建卡片弹窗的路径来源。
///
/// 与新建项目弹窗取的是同一批偏好字段（都由外壳持有），只是那边在开弹窗时现取，
/// 这边要留着待用。会话目录那一路不在这里：看板自己手里的卡片就带着它们的 `workingDir`。
#[derive(Debug, Default)]
struct KanbanPathSources {
    /// 默认目录：当前选中会话的工作目录。
    default_working_dir: String,
    project_paths: Vec<String>,
    ignored_paths: Vec<String>,
}

impl EventEmitter<KanbanEvent> for KanbanView {}

impl KanbanView {
    /// 建视图；此时还没有显示，因此不拉数据也不起轮询。
    ///
    /// 扩展缺席时那个路由打不通，隐藏期间每 5 秒发一次只会刷出一串同样的错误。
    pub fn new(api: Api) -> Self {
        Self {
            api,
            cards: Vec::new(),
            error: None,
            scale: CalendarScale::Day,
            anchor_day_key: today_key(),
            calendar_scroll: ScrollHandle::new(),
            preferred_slot: HashMap::new(),
            expanded: HashSet::new(),
            text_expanded: HashSet::new(),
            text_height: HashMap::new(),
            hovered_card: None,
            request_task: None,
            create_task: None,
            poll_task: None,
            visible: false,
            sidebar_open: true,
            create_card: None,
            path_sources: KanbanPathSources::default(),
        }
    }

    /// 侧边栏是否显示；外壳切换时告知。
    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open == open {
            return;
        }
        self.sidebar_open = open;
        cx.notify();
    }

    /// 新建卡片弹窗要用的路径来源；外壳在偏好或会话选择变化时推过来。
    pub fn set_project_paths(
        &mut self,
        default_working_dir: String,
        project_paths: Vec<String>,
        ignored_paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.path_sources = KanbanPathSources {
            default_working_dir,
            project_paths,
            ignored_paths,
        };
        cx.notify();
    }

    /// 打开「新建卡片」弹窗：路径来源与候选都取此刻的快照。
    fn open_create_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let default_working_dir = self.path_sources.default_working_dir.clone();
        let project_paths = self.path_sources.project_paths.clone();
        let ignored_paths = self.path_sources.ignored_paths.clone();
        // 候选里的会话目录取卡片自己的 `workingDir`：看板不需要为此再问一次外壳。
        let extra_candidates = self
            .cards
            .iter()
            .map(|card| card.working_dir.clone())
            .collect();
        let modal = cx.new(|cx| {
            CreateCardModal::new(
                api,
                default_working_dir,
                extra_candidates,
                project_paths,
                ignored_paths,
                window,
                cx,
            )
        });
        let subscription = cx.subscribe_in(
            &modal,
            window,
            |this, _, event: &CreateCardEvent, _, cx| match event {
                CreateCardEvent::Create {
                    title,
                    body,
                    working_dir,
                    date,
                } => this.create_card(
                    title.clone(),
                    body.clone(),
                    working_dir.clone(),
                    date.clone(),
                    cx,
                ),
                CreateCardEvent::ForgetPath(path) => {
                    cx.emit(KanbanEvent::ForgetProjectPath(path.clone()));
                },
                CreateCardEvent::Close => this.close_create_card(cx),
            },
        );
        self.create_card = Some(CreateCardDialog {
            modal,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// 关掉「新建卡片」弹窗；丢掉实体即丢掉它里面的订阅。
    fn close_create_card(&mut self, cx: &mut Context<Self>) {
        if self.create_card.take().is_some() {
            cx.notify();
        }
    }

    /// 建一张卡片：成功就关掉弹窗、刷新看板，并让外壳记住这个目录。
    ///
    /// 失败把消息送回弹窗（弹窗还开着，用户能改完重试），不像别处那样落到页面顶部的错误条
    /// ——那时弹窗正挡着它。`column` 不发：扩展自己把它补成待办（见 `kanban/wire.rs`）。
    fn create_card(
        &mut self,
        title: String,
        body: String,
        working_dir: String,
        date: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let api = self.api.clone();
        let Some(dialog) = &self.create_card else {
            return;
        };
        let modal = dialog.modal.clone();
        self.create_task = Some(cx.spawn(async move |this, cx| {
            let request = CreateCardRequest {
                title,
                body,
                column: None,
                working_dir: Some(working_dir.clone()),
                date,
            };
            match api.kanban_create_card(&request).await {
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        this.close_create_card(cx);
                        this.refresh(cx);
                        cx.emit(KanbanEvent::RememberProjectPath(working_dir));
                    })
                    .ok();
                },
                Err(error) => {
                    modal.update(cx, |modal, cx| modal.show_error(error.to_string(), cx));
                },
            }
        }));
    }

    /// 显示或隐藏本页；由外壳在切换主区域时告知。
    ///
    /// 显示时立刻拉一次并起轮询，隐藏时丢掉轮询句柄（丢弃即取消）。
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.refresh(cx);
            self.poll_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(BOARD_POLL_INTERVAL).await;
                    let Ok(()) = this.update(cx, |this, cx| this.refresh(cx)) else {
                        break;
                    };
                }
            }));
        } else {
            self.poll_task = None;
        }
        cx.notify();
    }


    /// 拉取整块看板。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.kanban_cards().await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(cards) => {
                        // 轮询会删掉卡片：长文本的溢出判定按 id 记着，得跟着卡片一起丢。
                        this.text_height.retain(|key, _| {
                            key.split(':')
                                .next()
                                .is_some_and(|id| cards.iter().any(|card| card.id == id))
                        });
                        this.cards = cards;
                        this.error = None;
                    },
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 把一张卡片移到某个落点：公共区的某一格，或日刻度里某一天的某个槽位。
    ///
    /// 运行中的两列由扩展独占，服务端会以 400 拒绝，因此落点词汇里根本没有它们
    /// （见 [`crate::kanban::PUBLIC_AREA_DROP_COLUMNS`]）。落回原处是空操作。
    fn move_card(&mut self, card_id: &str, target: DropTarget, cx: &mut Context<Self>) {
        let column = target.column();
        // 只有日刻度的桶键才是真正的归属日，因此只有日刻度的落点带日期。
        let day = match &target {
            DropTarget::Bucket { bucket_key, .. } if self.scale == CalendarScale::Day => {
                Some(bucket_key.clone())
            },
            _ => None,
        };

        let Some(card) = self.cards.iter().find(|card| card.id == card_id) else {
            return;
        };
        let same_day = match &day {
            Some(day) => &card.date == day,
            None => true,
        };
        if card.column == column && same_day {
            cx.notify();
            return;
        }

        let request = UpdateCardRequest {
            column: Some(column),
            date: day,
            ..UpdateCardRequest::default()
        };
        let api = self.api.clone();
        let card_id = card_id.to_string();
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.kanban_update_card(&card_id, &request).await;
            this.update(cx, |this, cx| this.apply(outcome.map(|_| ()), cx))
                .ok();
        }));
    }

    /// 删除一张卡片。
    fn delete_card(&mut self, card_id: &str, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let card_id = card_id.to_string();
        self.request_task = Some(cx.spawn(async move |this, cx| {
            let outcome = api.kanban_delete_card(&card_id).await;
            this.update(cx, |this, cx| this.apply(outcome, cx)).ok();
        }));
    }

    /// 一次写操作的收尾：成功即重拉整块看板，失败只报错。
    ///
    /// 改一张卡片后重拉而不是就地改：自动化可能在同一瞬间把卡片推进到别的列，
    /// 就地改会把那次推进盖掉。
    fn apply(&mut self, outcome: Result<(), crate::api::ApiError>, cx: &mut Context<Self>) {
        match outcome {
            Ok(()) => self.refresh(cx),
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            },
        }
    }

    /// 点手风琴项：点的是已经展开的那一项就收回对半态，否则切到它。
    fn toggle_slot(
        &mut self,
        bucket_key: &str,
        slot: CalendarSlot,
        expanded: Option<CalendarSlot>,
        cx: &mut Context<Self>,
    ) {
        if expanded == Some(slot) {
            self.preferred_slot.remove(bucket_key);
        } else {
            self.preferred_slot.insert(bucket_key.to_string(), slot);
        }
        cx.notify();
    }

    /// 点展开开关：一张卡片单独展开或收起。
    fn toggle_expanded(&mut self, card_id: &str) {
        if !self.expanded.remove(card_id) {
            self.expanded.insert(card_id.to_string());
        }
    }

    /// 是否所有卡片都收起了；空看板按「已收起」算，按钮文案才不会反过来。
    fn all_collapsed(&self) -> bool {
        self.cards.is_empty()
            || self
                .cards
                .iter()
                .all(|card| !self.expanded.contains(&card.id))
    }

    /// 全部展开 / 全部收起：一个按钮两种文案，按当前状态取反。
    fn toggle_all_collapsed(&mut self) {
        if self.all_collapsed() {
            self.expanded = self.cards.iter().map(|card| card.id.clone()).collect();
        } else {
            self.expanded.clear();
        }
    }

    /// 可折叠的长文本；卡片正文与执行说明共用。
    ///
    /// 折叠态固定高度、溢出的部分直接裁掉（与前端同此，不画省略号），只有**实测溢出**才给
    /// 「展开/收起」入口——短文本不该挂一个点了没反应的按钮。测量在布局期做：折叠态里内层
    /// 文本 `flex_shrink_0`，量到的才是完整高度，而不是被裁之后的高度。
    fn collapsible_text(
        &self,
        key: &str,
        text: String,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.text_expanded.contains(key);
        let measured = self.text_height.get(key).copied();
        let overflowing = measured.is_some_and(|height| height > TEXT_COLLAPSED_HEIGHT);

        let mut frame = div().w_full();
        if !expanded {
            let handle = cx.entity();
            let measured_key = key.to_string();
            frame = frame
                .max_h(px(TEXT_COLLAPSED_HEIGHT))
                .overflow_hidden()
                .on_children_prepainted(move |bounds, _, cx| {
                    let Some(bounds) = bounds.first() else {
                        return;
                    };
                    let height = f32::from(bounds.size.height);
                    handle.update(cx, |this, cx| {
                        if this.text_height.insert(measured_key.clone(), height) != Some(height) {
                            cx.notify();
                        }
                    });
                });
        }

        let mut block = v_flex().w_full().child(
            frame.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(color)
                    .child(text),
            ),
        );

        if !overflowing {
            return block.into_any_element();
        }
        let key = key.to_string();
        let hover_color = cx.theme().foreground;
        block = block.child(
            h_flex()
                .id(SharedString::from(format!("kanban-text-toggle-{key}")))
                .mt_1()
                .items_center()
                .gap_1()
                .cursor(CursorStyle::PointingHand)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .hover(move |style| style.text_color(hover_color))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if !this.text_expanded.remove(&key) {
                        this.text_expanded.insert(key.clone());
                    }
                    cx.notify();
                }))
                .child(
                    if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    }
                    .element(Size::Small),
                )
                .child(if expanded { "收起" } else { "展开" }),
        );
        block.into_any_element()
    }

    /// 一张卡片。
    ///
    /// 默认折叠：正面只有标题、归属日与展开开关；展开后才出工作目录、创建日、尝试次数、
    /// 备注与操作行。打开对话挂在整张卡片上（与前端同此），卡片上的控件各自
    /// `stop_propagation`，否则点一次展开箭头会顺带跳进对话。
    fn render_card(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let expanded = self.expanded.contains(&card.id);
        let clickable = opens_conversation(card.column);
        let draggable = is_user_writable(card.column);
        let blocked = card.column == CardColumn::Blocked;
        let day_key = card_day_key(card);
        let created_key = day_key_from_iso(&card.created_at);
        let hover_background = cx.theme().secondary_hover;

        let mut shell = v_flex()
            .id(SharedString::from(format!("kanban-card-{}", card.id)))
            .gap_1()
            .w_full()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if blocked {
                cx.theme().danger.opacity(0.3)
            } else {
                cx.theme().border
            })
            .bg(if blocked {
                cx.theme().danger.opacity(0.15)
            } else {
                cx.theme().secondary
            })
            .shadow_sm()
            .cursor(if clickable {
                CursorStyle::PointingHand
            } else if draggable {
                CursorStyle::OpenHand
            } else {
                CursorStyle::Arrow
            });

        // 悬停的高亮不盖掉 blocked 的危险底色。
        if !blocked {
            shell = shell.hover(move |style| style.bg(hover_background));
        }

        if clickable && let Some(session_id) = card.session_id.clone() {
            shell = shell.on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(KanbanEvent::OpenSession(session_id.clone()));
            }));
        }

        // 运行中的卡片由扩展持有，服务端会以 400 拒绝写入，因此不给它拖拽把手。
        if draggable {
            shell = shell.on_drag(
                CardDrag {
                    card_id: card.id.clone(),
                    title: card.title.clone(),
                },
                |drag, _, _, cx| {
                    cx.new(|_| CardGhost {
                        title: drag.title.clone(),
                    })
                },
            );
        }

        // 悬停只记一张卡片：操作行默认隐身，只有指针下这一张显形。
        let hover_id = card.id.clone();
        shell = shell.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            // 离开事件可能晚于下一张卡片的进入事件，还指向别处时不要抢着清空。
            if !*hovered && this.hovered_card.as_deref() != Some(hover_id.as_str()) {
                return;
            }
            let next = hovered.then(|| hover_id.clone());
            if this.hovered_card != next {
                this.hovered_card = next;
                cx.notify();
            }
        }));

        // 标题行：标题单行超出省略，归属日与展开开关各自占固定宽度。
        let mut title_row = h_flex().items_start().gap_1().child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(card.title.clone()),
        );
        if !day_key.is_empty() {
            title_row = title_row.child(
                div()
                    .flex_shrink_0()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().background)
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(short_day_label(&day_key)),
            );
        }
        let toggle_id = card.id.clone();
        title_row = title_row.child(
            div()
                .id(SharedString::from(format!("kanban-expand-{}", card.id)))
                .flex_shrink_0()
                .cursor(CursorStyle::PointingHand)
                .text_color(cx.theme().muted_foreground)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_expanded(&toggle_id);
                    cx.notify();
                }))
                .child(
                    if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    }
                    .element(Size::Small),
                ),
        );
        shell = shell.child(title_row);

        if !card.body.trim().is_empty() {
            shell = shell.child(self.collapsible_text(
                &format!("{}:body", card.id),
                card.body.clone(),
                cx.theme().muted_foreground,
                cx,
            ));
        }

        if !expanded {
            return shell.into_any_element();
        }

        let mut meta = h_flex().items_center().gap_1().flex_wrap();
        if !card.working_dir.trim().is_empty() {
            meta = meta.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(card.working_dir.clone()),
            );
        }
        // 归属日被拖拽改写后，卡片正面显示的就是归属日，这里把创建日补回来。
        if !created_key.is_empty() && created_key != day_key {
            meta = meta.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("· 创建于 {}", short_day_label(&created_key))),
            );
        }
        if card.attempt > 0 {
            meta = meta.child(
                div()
                    .flex_shrink_0()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().warning.opacity(0.15))
                    .px_1()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().warning)
                    .child(format!("第 {} 次尝试", card.attempt)),
            );
        }
        shell = shell.child(meta);

        if let Some(note) = &card.note {
            let (color, background) = if blocked {
                (cx.theme().danger, cx.theme().danger.opacity(0.15))
            } else {
                (cx.theme().muted_foreground, cx.theme().background)
            };
            shell = shell.child(
                div()
                    .rounded(cx.theme().radius)
                    .px_2()
                    .py_1()
                    .bg(background)
                    .child(self.collapsible_text(
                        &format!("{}:note", card.id),
                        note.clone(),
                        color,
                        cx,
                    )),
            );
        }

        // 操作行：默认隐身，悬停到这张卡片才显形——一屏同时挂十几个删除按钮太吵。
        let delete_id = card.id.clone();
        let mut actions = h_flex()
            .items_center()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .opacity(if self.hovered_card.as_deref() == Some(card.id.as_str()) {
                1.0
            } else {
                0.0
            });
        actions = actions.child(div().flex_1()).child(
            Button::new(SharedString::from(format!("kanban-delete-{}", card.id)))
                .outline()
                .label("删除")
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.delete_card(&delete_id, cx);
                })),
        );
        shell.child(actions).into_any_element()
    }

    /// 公共区的一格。
    fn render_public_column(
        &self,
        column: CardColumn,
        cards: &[&Card],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 只有待领取与已阻塞接受落点：其余四列要么是自动化的，要么在日历里。
        let accepts_drop = PUBLIC_AREA_DROP_COLUMNS.contains(&column);
        let target = DropTarget::Column(column);

        let mut shell = v_flex()
            .id(SharedString::from(format!("kanban-column-{column:?}")))
            .flex_1()
            .min_h_0()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background);

        if accepts_drop {
            let drop_color = cx.theme().drop_target;
            shell = shell
                .drag_over::<CardDrag>(move |style, _: &CardDrag, _, _| {
                    style.border_color(drop_color)
                })
                .on_drop(cx.listener(move |this, drag: &CardDrag, _, cx| {
                    this.move_card(&drag.card_id, target.clone(), cx);
                }));
        }

        let header = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .w(px(8.))
                    .h(px(8.))
                    .rounded(cx.theme().radius_full())
                    .bg(column_tone(column, cx.theme())),
            )
            .child(div().text_sm().child(column_label(column)))
            .child(div().flex_1())
            .child(badge(cards.len().to_string(), cx.theme()));

        let rows: Vec<AnyElement> = cards
            .iter()
            .map(|card| self.render_card(card, cx))
            .collect();

        shell
            .child(header)
            .child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .pb_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(column_hint(column)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("kanban-column-list-{column:?}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().p_2().child(v_flex().gap_2().children(rows))),
            )
            .into_any_element()
    }

    /// 日历里的一个时间桶：一个列头 + 两个手风琴项。
    fn render_bucket(
        &self,
        bucket: &CalendarBucket,
        cards: &BucketCards<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let counts = cards.counts();
        let expanded = resolve_expanded_slot(self.preferred_slot.get(&bucket.key).copied(), counts);
        let is_today = bucket.key == bucket_key_of(self.scale, &today_key());

        // 空列收缩到基准宽：日期还在，卡片位置全留白，一眼看出这天没有卡片。
        let width: DefiniteLength = if cards.is_empty() {
            px(COLLAPSED_COLUMN_WIDTH).into()
        } else {
            relative(EXPANDED_COLUMN_FRACTION)
        };

        let mut header = h_flex()
            .h(px(CALENDAR_HEADER_HEIGHT))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(bucket_header(bucket, self.scale, cx.theme()));
        if is_today {
            header = header.text_color(cx.theme().accent);
        }

        let slots: Vec<AnyElement> = CalendarSlot::ALL
            .into_iter()
            .map(|slot| self.render_slot(bucket, slot, cards, expanded, cx))
            .collect();

        v_flex()
            .id(SharedString::from(format!("kanban-bucket-{}", bucket.key)))
            .h_full()
            .flex_shrink_0()
            .w(width)
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(if is_today {
                cx.theme().list_active
            } else {
                cx.theme().background
            })
            .child(header)
            .children(slots)
            .into_any_element()
    }

    /// 日历列里的一个手风琴项：展开时占满剩余高度，收起时只留标题条。
    fn render_slot(
        &self,
        bucket: &CalendarBucket,
        slot: CalendarSlot,
        cards: &BucketCards<'_>,
        expanded: Option<CalendarSlot>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 对半态下两项都展开；手风琴态下只有被选中的那一项展开。
        let is_expanded = expanded.is_none() || expanded == Some(slot);
        // 只有日刻度的桶键才是真正的归属日；归属日未知的「未排期」桶同样不接受落点。
        let accepts_drop = self.scale == CalendarScale::Day && bucket.key != UNSCHEDULED_BUCKET_KEY;
        let target = DropTarget::Bucket {
            bucket_key: bucket.key.clone(),
            slot,
        };

        let bucket_key = bucket.key.clone();
        let mut section = v_flex()
            .id(SharedString::from(format!(
                "kanban-slot-{}-{slot:?}",
                bucket.key
            )))
            .min_h_0();
        if is_expanded {
            section = section.flex_1();
        }

        // 切换展开态只挂在标题条上：卡片自己在下面，点卡片不该顺带收起这一项。
        let mut head = h_flex()
            .id(SharedString::from(format!(
                "kanban-slot-head-{}-{slot:?}",
                bucket.key
            )))
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_2()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .hover({
                let hover = cx.theme().list_hover;
                move |this| this.bg(hover)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_slot(&bucket_key, slot, expanded, cx);
            }))
            .child(div().text_xs().child(slot_label(slot)));
        // 年刻度只给数量：一列要塞进整年，列出卡片既读不动也滚不完。
        if self.scale != CalendarScale::Year {
            head = head
                .child(div().flex_1())
                .child(badge(cards.counts().get(slot).to_string(), cx.theme()));
        }
        section = section.child(head);

        if accepts_drop {
            let drop_color = cx.theme().drop_target;
            section = section
                .drag_over::<CardDrag>(move |style, _: &CardDrag, _, _| style.bg(drop_color))
                .on_drop(cx.listener(move |this, drag: &CardDrag, _, cx| {
                    this.move_card(&drag.card_id, target.clone(), cx);
                }));
        }

        if !is_expanded {
            return section.into_any_element();
        }

        if self.scale == CalendarScale::Year {
            let count = cards.counts().get(slot);
            return section
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .child(div().text_lg().child(count.to_string()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("张卡片"),
                        ),
                )
                .into_any_element();
        }

        let rows: Vec<AnyElement> = cards
            .slot(slot)
            .iter()
            .map(|card| self.render_card(card, cx))
            .collect();
        let rows = if rows.is_empty() {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("暂无卡片")
                .into_any_element()
        } else {
            v_flex().gap_2().children(rows).into_any_element()
        };
        section
            .child(
                div()
                    .id(SharedString::from(format!(
                        "kanban-slot-list-{}-{slot:?}",
                        bucket.key
                    )))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().p_2().child(rows)),
            )
            .into_any_element()
    }

    /// 日历当前渲染的桶，含「未排期」列（有这类卡片时才出现）。
    ///
    /// 渲染与 [`Self::recenter_on_today`] 的居中下标都以它为准：`scroll_to_item` 的下标
    /// 必须与真正画出来的子元素顺序一致，两处各算一遍迟會会错位。
    fn calendar_buckets(&self, grouped: &HashMap<String, BucketCards<'_>>) -> Vec<CalendarBucket> {
        let mut buckets = buckets_for(self.scale, &self.anchor_day_key);
        if grouped.contains_key(UNSCHEDULED_BUCKET_KEY) {
            buckets.insert(0, unscheduled_bucket());
        }
        buckets
    }

    /// 「回到今天」：锚点回到今天，并把今天那一列带回视口。
    ///
    /// 横向滚动不改锚点，所以「已经停在今天、只是滚到别处」时只重置锚点什么都不会变，
    /// 按钮看起来就是坏的（前端为此专门留了一个居中自增号，见 `KanbanPage.tsx:141`）。
    fn recenter_on_today(&mut self, cx: &mut Context<Self>) {
        self.anchor_day_key = today_key();
        let today_bucket = bucket_key_of(self.scale, &self.anchor_day_key);
        let buckets = self.calendar_buckets(&cards_by_bucket(&self.cards, self.scale));
        if let Some(index) = buckets.iter().position(|bucket| bucket.key == today_bucket) {
            self.calendar_scroll.scroll_to_item(index);
        }
        cx.notify();
    }

    /// 日历区：翻页与刻度条 + 一排时间桶。
    fn render_calendar(&self, cx: &mut Context<Self>) -> AnyElement {
        let grouped = cards_by_bucket(&self.cards, self.scale);
        let buckets = self.calendar_buckets(&grouped);

        let empty = BucketCards::default();
        let columns: Vec<AnyElement> = buckets
            .iter()
            .map(|bucket| {
                let cards = grouped.get(&bucket.key).unwrap_or(&empty);
                self.render_bucket(bucket, cards, cx)
            })
            .collect();

        v_flex()
            .flex_1()
            // 日历区的内容是一条横向滚动条（下面那排时间桶），它的 min-content 会被
            // 当成自动最小宽度顶上来，把这一格撑得比它该占的份额宽——桶上的
            // `relative(0.25)` 于是按错的基准算，一屏只放得下一个桶。显式允许收缩。
            .min_w_0()
            .min_h_0()
            .gap_2()
            .child(self.render_calendar_bar(cx))
            .child(
                h_flex()
                    .id("kanban-calendar")
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_x_scroll()
                    .track_scroll(&self.calendar_scroll)
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .children(columns),
            )
            .into_any_element()
    }

    fn render_calendar_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let scales: Vec<AnyElement> = CalendarScale::ALL
            .into_iter()
            .map(|scale| {
                let selected = scale == self.scale;
                div()
                    .id(SharedString::from(format!("kanban-scale-{scale:?}")))
                    .px_2()
                    .py_1()
                    .rounded(cx.theme().radius)
                    .text_xs()
                    .bg(if selected {
                        cx.theme().list_active
                    } else {
                        cx.theme().transparent
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.scale = scale;
                        cx.notify();
                    }))
                    .child(scale.label())
                    .into_any_element()
            })
            .collect();

        h_flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .child(
                Button::new("kanban-anchor-previous")
                    .outline()
                    .label("上一页")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.anchor_day_key =
                            shift_anchor_day_key(this.scale, &this.anchor_day_key, -1);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .min_w(px(120.))
                    .text_sm()
                    .child(anchor_label(self.scale, &self.anchor_day_key)),
            )
            .child(
                Button::new("kanban-anchor-next")
                    .outline()
                    .label("下一页")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.anchor_day_key =
                            shift_anchor_day_key(this.scale, &this.anchor_day_key, 1);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("kanban-anchor-today")
                    .outline()
                    .label("回到今天")
                    .on_click(cx.listener(|this, _, _, cx| this.recenter_on_today(cx))),
            )
            .child(div().flex_1())
            .child(h_flex().gap_1().children(scales))
            .into_any_element()
    }
}

impl Render for KanbanView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let public_cards = cards_by_public_column(&self.cards);
        let running = self
            .cards
            .iter()
            .filter(|card| card.column.is_running())
            .count();

        let columns: Vec<AnyElement> = PUBLIC_AREA_COLUMNS
            .into_iter()
            .map(|column| {
                let cards = public_cards.get(&column).map(Vec::as_slice).unwrap_or(&[]);
                self.render_public_column(column, cards, cx)
            })
            .collect();

        let mut header = page_header(cx);
        // 侧边栏收起时给一条回到它的路（前端同样只在收起时显示这枚按钮）。
        if !self.sidebar_open {
            header = header.child(icon_button(
                "kanban-expand-sidebar",
                IconName::Sidebar,
                "展开侧边栏",
                cx,
                |_this: &mut KanbanView, cx| cx.emit(KanbanEvent::ToggleSidebar),
            ));
        }
        header = header
            .child(
                div()
                    .flex_shrink_0()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("看板"),
            )
            .child(badge(format!("{} 张卡片", self.cards.len()), cx.theme()));
        if running > 0 {
            header = header.child(badge(format!("{running} 张执行中"), cx.theme()));
        }

        let mut page = v_flex().size_full().bg(cx.theme().background).child(
            header
                .child(div().flex_1())
                .child(
                    Button::new("kanban-toggle-all")
                        .outline()
                        .label(if self.all_collapsed() {
                            "全部展开"
                        } else {
                            "全部收起"
                        })
                        .disabled(self.cards.is_empty())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_all_collapsed();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("kanban-refresh")
                        .outline()
                        .label("刷新")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
                .child(
                    Button::new("kanban-create-card")
                        .primary()
                        .label("新建卡片")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_create_card(window, cx)),
                        ),
                ),
        );

        if let Some(error) = &self.error {
            page = page.child(
                div()
                    .flex_shrink_0()
                    .mx_6()
                    .my_2()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().danger.opacity(0.3))
                    .bg(cx.theme().danger.opacity(0.15))
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        } else if self.cards.is_empty() {
            page = page.child(
                div()
                    .flex_shrink_0()
                    .mx_6()
                    .my_2()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().group_box)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("还没有卡片。点右上角「新建卡片」写下第一条需求。"),
            );
        }

        let mut page = page.child(
            // `h_flex` 在交叉轴上居中子元素：不显式拉伸的话，公共区与日历区都只占自己的
            // 内容高度并被居中，窗口越高上下留白越多（见 gpui-base `styled.rs` 的说明）。
            h_flex()
                .flex_1()
                .min_h_0()
                .items_stretch()
                .gap_4()
                .px_6()
                .py_2()
                .child(
                    v_flex()
                        .w(relative(PUBLIC_AREA_FRACTION))
                        .min_h_0()
                        .flex_shrink_0()
                        .gap_3()
                        .children(columns),
                )
                .child(self.render_calendar(cx)),
        );

        // 弹窗铺满整页，画在最后因此盖住其余部分。
        if let Some(dialog) = &self.create_card {
            page = page.child(dialog.modal.clone());
        }
        page
    }
}

/// 公共区四格各自的卡片；日历里的待办与已完成不在这里重复出现。
fn cards_by_public_column(cards: &[Card]) -> HashMap<CardColumn, Vec<&Card>> {
    let mut grouped: HashMap<CardColumn, Vec<&Card>> = HashMap::new();
    for card in cards {
        if PUBLIC_AREA_COLUMNS.contains(&card.column) {
            grouped.entry(card.column).or_default().push(card);
        }
    }
    grouped
}

/// 按归属日把卡片归进各时间桶；归属日无法确定的进 [`UNSCHEDULED_BUCKET_KEY`]。
fn cards_by_bucket(cards: &[Card], scale: CalendarScale) -> HashMap<String, BucketCards<'_>> {
    let mut grouped: HashMap<String, BucketCards<'_>> = HashMap::new();
    for card in cards {
        if !matches!(card.column, CardColumn::Backlog | CardColumn::Done) {
            continue;
        }
        let day = card_day_key(card);
        let key = match bucket_key_of(scale, &day) {
            key if key.is_empty() => UNSCHEDULED_BUCKET_KEY.to_string(),
            key => key,
        };
        let entry = grouped.entry(key).or_default();
        match card.column {
            CardColumn::Backlog => entry.backlog.push(card),
            CardColumn::Done => entry.done.push(card),
            // 上一步已经滤掉其余四列。
            _ => {},
        }
    }
    grouped
}

/// 「未排期」列：归属日未知的旧卡片必须有个可见的落点，否则就会从看板上消失。
fn unscheduled_bucket() -> CalendarBucket {
    CalendarBucket {
        key: UNSCHEDULED_BUCKET_KEY.to_string(),
        label: "未排期".to_string(),
        short_label: "未排期".to_string(),
        start_day: String::new(),
        end_day: String::new(),
    }
}

/// 日历列头：日刻度给「日期数字 + 星期」，其余刻度给周期标签。
fn bucket_header(bucket: &CalendarBucket, scale: CalendarScale, theme: &Theme) -> AnyElement {
    if scale != CalendarScale::Day {
        return div()
            .text_xs()
            .truncate()
            .child(bucket.label.clone())
            .into_any_element();
    }
    let weekday = day_key_to_date(&bucket.start_day)
        .map(|date| WEEKDAY_LABELS[date.weekday().num_days_from_monday() as usize]);
    let mut header = h_flex()
        .items_center()
        .gap_1()
        .child(div().text_sm().child(bucket.short_label.clone()));
    if let Some(weekday) = weekday {
        header = header.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(weekday),
        );
    }
    header.into_any_element()
}

/// 列的状态色：公共区的圆点与日历的强调共用一套，否则同一页会长出两套语言。
fn column_tone(column: CardColumn, theme: &Theme) -> Hsla {
    match column {
        CardColumn::Backlog => theme.muted_foreground,
        CardColumn::Ready => theme.accent,
        CardColumn::Analyzing => theme.info,
        CardColumn::Implementing => theme.primary,
        CardColumn::Done => theme.success,
        CardColumn::Blocked => theme.danger,
    }
}

fn column_label(column: CardColumn) -> &'static str {
    match column {
        CardColumn::Backlog => "待办",
        CardColumn::Ready => "待领取",
        CardColumn::Analyzing => "分析中",
        CardColumn::Implementing => "实施中",
        CardColumn::Done => "已完成",
        CardColumn::Blocked => "已阻塞",
    }
}

fn column_hint(column: CardColumn) -> &'static str {
    match column {
        CardColumn::Backlog => "只记录，不自动执行",
        CardColumn::Ready => "等待扩展领取",
        CardColumn::Analyzing => "扩展正在分析需求",
        CardColumn::Implementing => "扩展正在实施",
        CardColumn::Done => "终态",
        CardColumn::Blocked => "需要人工介入",
    }
}

/// 日历槽位的名字与同名公共列一致，只写一处。
fn slot_label(slot: CalendarSlot) -> &'static str {
    column_label(slot.column())
}

/// 卡片上的日期标记用 `M/D`：完整 ISO 串在窄列里会把标题挤没。
///
/// 日键解析不出来时原样返回，不编造一个日期出来。
fn short_day_label(day_key: &str) -> String {
    match day_key_to_date(day_key) {
        Some(date) => format!("{}/{}", date.month(), date.day()),
        None => day_key.to_string(),
    }
}

/// 数量角标。
fn badge(text: impl Into<SharedString>, theme: &Theme) -> AnyElement {
    div()
        .flex_shrink_0()
        .px_2()
        .rounded(theme.radius)
        .bg(theme.muted)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(text.into())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, column: CardColumn, date: &str) -> Card {
        Card {
            id: id.to_string(),
            title: format!("卡片 {id}"),
            body: String::new(),
            column,
            working_dir: "/tmp/project".to_string(),
            session_id: None,
            attempt: 0,
            note: None,
            date: date.to_string(),
            created_at: "2026-03-01T00:00:00Z".to_string(),
            updated_at: "2026-03-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn public_area_columns_split_from_the_calendar_slots() {
        let cards = vec![
            card("a", CardColumn::Ready, "2026-03-02"),
            card("b", CardColumn::Backlog, "2026-03-02"),
            card("c", CardColumn::Done, "2026-03-02"),
            card("d", CardColumn::Blocked, "2026-03-02"),
        ];
        let grouped = cards_by_public_column(&cards);
        assert_eq!(grouped.get(&CardColumn::Ready).map(Vec::len), Some(1));
        assert_eq!(grouped.get(&CardColumn::Blocked).map(Vec::len), Some(1));
        // 待办与已完成在日历里，公共区不再列一遍。
        assert!(!grouped.contains_key(&CardColumn::Backlog));
        assert!(!grouped.contains_key(&CardColumn::Done));
    }

    #[test]
    fn calendar_buckets_hold_backlog_and_done_only() {
        let cards = vec![
            card("a", CardColumn::Backlog, "2026-03-02"),
            card("b", CardColumn::Done, "2026-03-02"),
            card("c", CardColumn::Ready, "2026-03-02"),
        ];
        let grouped = cards_by_bucket(&cards, CalendarScale::Day);
        let bucket = grouped.get("2026-03-02").expect("这一天有一个桶");
        assert_eq!(bucket.backlog.len(), 1);
        assert_eq!(bucket.done.len(), 1);
        assert_eq!(bucket.counts().get(CalendarSlot::Backlog), 1);
        // 待领取在公共区，不占日历的位置。
        assert_eq!(grouped.len(), 1);
    }

    #[test]
    fn cards_without_a_known_day_land_in_the_unscheduled_bucket() {
        // 没有归属日时先回落到创建时间，回落也不成立才是「未排期」。
        let mut undated = card("old", CardColumn::Backlog, "");
        undated.created_at = String::new();
        let cards = vec![undated, card("odd", CardColumn::Done, "2026-3-2")];

        let grouped = cards_by_bucket(&cards, CalendarScale::Day);
        assert_eq!(grouped.len(), 1, "两张都该进未排期");
        let bucket = grouped.get(UNSCHEDULED_BUCKET_KEY).expect("未排期桶存在");
        assert_eq!(bucket.backlog.len(), 1);
        assert_eq!(bucket.done.len(), 1);
    }

    #[test]
    fn a_card_without_a_date_falls_back_to_its_creation_day() {
        let cards = vec![card("old", CardColumn::Backlog, "")];
        let grouped = cards_by_bucket(&cards, CalendarScale::Day);
        // 创建时间是 UTC，本地时区可能落在前后一天，因此只断言它没有掉进未排期。
        assert!(!grouped.contains_key(UNSCHEDULED_BUCKET_KEY));
        assert_eq!(
            grouped
                .values()
                .map(|bucket| bucket.backlog.len())
                .sum::<usize>(),
            1
        );
    }


    #[test]
    fn coarser_scales_merge_whole_periods_into_one_bucket() {
        let cards = vec![
            card("a", CardColumn::Backlog, "2026-03-02"),
            card("b", CardColumn::Backlog, "2026-03-20"),
        ];
        let weekly = cards_by_bucket(&cards, CalendarScale::Week);
        assert_eq!(weekly.get("2026-03-02").map(|b| b.backlog.len()), Some(1));
        assert_eq!(weekly.get("2026-03-16").map(|b| b.backlog.len()), Some(1));
        let monthly = cards_by_bucket(&cards, CalendarScale::Month);
        assert_eq!(monthly.get("2026-03").map(|b| b.backlog.len()), Some(2));
    }

    #[test]
    fn a_day_key_without_a_date_stays_as_it_is() {
        assert_eq!(short_day_label("2026-10-03"), "10/3");
        assert_eq!(short_day_label("not-a-timestamp"), "not-a-timestamp");
    }

    #[test]
    fn every_column_and_slot_has_a_chinese_label() {
        for column in CardColumn::ALL {
            assert!(!column_label(column).is_empty(), "{column:?} 缺名字");
            assert!(!column_hint(column).is_empty(), "{column:?} 缺说明");
        }
        // 槽位的名字就是它所属列的名字，两处不会各写一版。
        for slot in CalendarSlot::ALL {
            assert_eq!(slot_label(slot), column_label(slot.column()));
        }
    }
}
