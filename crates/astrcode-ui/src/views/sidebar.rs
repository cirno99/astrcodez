//! 侧边栏：项目分组、会话列表与页面导航。
//!
//! 分组与排序都是纯推导，在 [`crate::session_list`] 里；这里只把它的结果摆出来，并把用户的
//! 操作发成事件（选中、折叠、Fork、删除）交给外壳。折叠状态与宽度的持久化在外壳手里：
//! 偏好文件是整份替换，只能有一个写者。

use astrcode_protocol::http::SessionListItemDto;
use gpui_kit::{
    AnyElement, Context, EventEmitter, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        ActiveTheme as _, Disableable as _, Size,
        button::{Button, ButtonVariants as _},
        h_flex, v_flex,
    },
    div, px, radians,
};

use super::{MainView, icon_button, page_header};
use crate::{icons::IconName, preferences, session_list, theme};

/// 右键菜单的宽度，与前端 `min-w-[176px]` 同值。
const MENU_WIDTH: f32 = 176.0;
/// 菜单与侧边栏右边缘之间的留白；位置按它夹取，免得菜单越过侧边栏压到主区域上。
const MENU_EDGE_GAP: f32 = 8.0;
/// 纵向夹取时按它估算菜单高度（两项加确认态的余量）。
const MENU_ESTIMATED_HEIGHT: f32 = 96.0;

/// 侧边栏对外的事件。
#[derive(Debug, Clone)]
pub enum SidebarEvent {
    /// 用户选中了一个会话。
    Select(String),
    /// 用户要求新建一个会话（「新对话」）。
    NewSession,
    /// 用户要求打开「新建项目」弹窗；一个项目都没有时「新对话」也走这里。
    NewProject,
    /// 用户要求切到某一页。
    OpenView(MainView),
    /// 用户要求收起侧边栏。
    ToggleSidebar,
    /// 用户要求重取会话列表。
    RefreshSessions,
    /// 用户要求从某个会话分叉。
    ForkSession(String),
    /// 用户要求删除某个会话。
    DeleteSession(String),
    /// 用户要求删除某个项目（工作目录下的全部会话）。
    DeleteProject(String),
    /// 用户要求删除一批会话（选择态里确认过的那些）。
    DeleteSessions(Vec<String>),
    /// 折叠集合变了；落盘由外壳做。
    CollapsedChanged(Vec<String>),
}

/// 右键菜单指向的对象。
#[derive(Debug, Clone, PartialEq, Eq)]
enum MenuTarget {
    Session(String),
    Project(String),
}

/// 已经打开的右键菜单。
struct ContextMenu {
    target: MenuTarget,
    /// 窗口坐标；打开时已按窗口与侧边栏边界夹过。
    x: f32,
    y: f32,
    /// 是否已经点了删除、正在等确认。
    confirming: bool,
}

pub struct Sidebar {
    sessions: Vec<SessionListItemDto>,
    /// 项目的固定顺序；只在第一次见到列表时按时间定序，之后不再重排。
    project_order: Vec<String>,
    /// 折叠起来的项目目录（集合语义，顺序无关）。
    collapsed: Vec<String>,
    active: Option<String>,
    error: Option<String>,
    /// 当前主区域显示的那一页；导航项靠它高亮。
    view: MainView,
    /// 看板扩展是否可用；不可用时不给入口，否则点进去是一个打不通的页面。
    kanban_available: bool,
    /// 会话列表是否在重取中。
    refreshing: bool,
    /// 侧边栏宽度；右键菜单按它横向夹取，取自外壳。
    width: f32,
    /// 选择态：整份列表切成批量勾选，行不再切会话。
    select_mode: bool,
    /// 选择态里勾选的会话 id；列表换了一批即剪枝。
    selected_ids: Vec<String>,
    /// 批量删除是否已经点了删除、正在等确认。
    confirm_batch_delete: bool,
    menu: Option<ContextMenu>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.notify();
        Self {
            sessions: Vec::new(),
            project_order: Vec::new(),
            collapsed: Vec::new(),
            active: None,
            error: None,
            view: MainView::Chat,
            kanban_available: false,
            refreshing: false,
            width: preferences::SIDEBAR_WIDTH_DEFAULT as f32,
            select_mode: false,
            selected_ids: Vec::new(),
            confirm_batch_delete: false,
            menu: None,
        }
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionListItemDto>, cx: &mut Context<Self>) {
        // 项目顺序：第一次见到列表时按各组最早会话定序，之后只做「删掉的移除、新的追加」。
        self.project_order = if self.project_order.is_empty() {
            session_list::initial_project_order(&sessions)
        } else {
            session_list::sync_project_order(&self.project_order, &sessions)
        };
        // 列表换了，勾选里可能已经有查不到的 id（前端 `effectiveSelectedSessionIds` 同判据）。
        self.selected_ids = session_list::prune_selected(&self.selected_ids, &sessions);
        self.sessions = sessions;
        self.error = None;
        self.refreshing = false;
        self.prune_collapsed(cx);
        // 列表换了，「这条会话还在不在」的答案就变了；右键菜单指向的对象可能已经不在。
        self.close_menu();
        cx.notify();
    }

    pub fn set_active(&mut self, session_id: &str, cx: &mut Context<Self>) {
        self.active = Some(session_id.to_string());
        cx.notify();
    }

    pub fn set_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message);
        // 报错即结束这一趟重取，否则刷新按钮会一直是「刷新中」。
        self.refreshing = false;
        cx.notify();
    }

    /// 由外壳告知当前在哪一页：从卡片点进对话时来路也是外壳定的。
    pub fn set_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        self.view = view;
        cx.notify();
    }

    pub fn set_kanban_available(&mut self, available: bool, cx: &mut Context<Self>) {
        if self.kanban_available == available {
            return;
        }
        self.kanban_available = available;
        cx.notify();
    }

    pub fn set_refreshing(&mut self, refreshing: bool, cx: &mut Context<Self>) {
        if self.refreshing == refreshing {
            return;
        }
        self.refreshing = refreshing;
        cx.notify();
    }

    /// 宽度变了：菜单的横向夹取跟着变。
    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if self.width == width {
            return;
        }
        self.width = width;
        cx.notify();
    }

    /// 装上服务端存着的折叠集合。
    ///
    /// 只在偏好到位时调用一次：之后折叠状态由本视图自己维护，外壳只负责把它落盘。
    pub fn set_collapsed_dirs(&mut self, collapsed: Vec<String>, cx: &mut Context<Self>) {
        if self.collapsed == collapsed {
            return;
        }
        self.collapsed = collapsed;
        self.prune_collapsed(cx);
        cx.notify();
    }

    /// 当前折叠着的项目目录。
    pub fn collapsed_dirs(&self) -> &[String] {
        &self.collapsed
    }

    /// 某个会话在 UI 里的显示名；列表里没有这个 id 时返回 `None`。
    pub fn session_title(&self, session_id: &str) -> Option<String> {
        self.sessions
            .iter()
            .find(|item| item.session_id == session_id)
            .map(super::display_title)
    }

    /// 某个会话的工作目录；列表里没有这个 id 时返回 `None`。
    ///
    /// 会话面板的状态行靠它写项目名——工作目录是会话的属性，外壳手里只有列表。
    pub fn working_dir_of(&self, session_id: &str) -> Option<String> {
        self.sessions
            .iter()
            .find(|item| item.session_id == session_id)
            .map(|item| item.working_dir.clone())
    }

    /// 当前选中的会话 id。
    pub fn active_id(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// 「新对话」该建在哪个目录：当前会话所在的项目，还没有选中项时用列表里的第一个。
    pub fn active_working_dir(&self) -> Option<String> {
        if let Some(active) = self.active.as_deref()
            && let Some(item) = self.sessions.iter().find(|item| item.session_id == active)
        {
            return Some(item.working_dir.clone());
        }
        self.sessions.first().map(|item| item.working_dir.clone())
    }

    /// 会话列表里的工作目录，按列表顺序。
    ///
    /// 新建项目弹窗拿它当历史之外的候选来源。与前端
    /// `sessions.map((session) => session.workingDir)` 同口径：不去重也不排序——去重发生在
    /// 合并候选的那一步（`kanban::merge_project_path_candidates`）。
    pub fn project_working_dirs(&self) -> Vec<String> {
        self.sessions
            .iter()
            .map(|item| item.working_dir.clone())
            .collect()
    }

    /// 折叠集合里去掉已经不在列表里的目录，变了就告诉外壳落盘。
    fn prune_collapsed(&mut self, cx: &mut Context<Self>) {
        let groups = session_list::group_sessions(&self.sessions, &self.project_order);
        let pruned = session_list::prune_collapsed(&self.collapsed, &groups);
        if pruned == self.collapsed {
            return;
        }
        self.collapsed = pruned.clone();
        cx.emit(SidebarEvent::CollapsedChanged(pruned));
    }

    fn toggle_collapsed(&mut self, working_dir: &str, cx: &mut Context<Self>) {
        self.collapsed = session_list::toggle_collapsed(&self.collapsed, working_dir);
        cx.emit(SidebarEvent::CollapsedChanged(self.collapsed.clone()));
        cx.notify();
    }

    /// 进入选择态：右键菜单与上一次的确认都作废，勾选从空开始。
    fn enter_select_mode(&mut self, cx: &mut Context<Self>) {
        self.close_menu();
        self.confirm_batch_delete = false;
        self.selected_ids.clear();
        self.select_mode = true;
        cx.notify();
    }

    /// 退出选择态；勾选与确认一并清掉。
    fn exit_select_mode(&mut self, cx: &mut Context<Self>) {
        self.select_mode = false;
        self.selected_ids.clear();
        self.confirm_batch_delete = false;
        cx.notify();
    }

    fn close_menu(&mut self) {
        self.menu = None;
    }

    /// 收起菜单并重画。
    fn dismiss_menu(&mut self, cx: &mut Context<Self>) {
        self.close_menu();
        cx.notify();
    }

    /// 打开右键菜单并夹进可点区域。
    ///
    /// 夹取照前端 `Sidebar.tsx`：右边界留出菜单宽度，下边界留出菜单高度。侧边栏永远贴着
    /// 窗口左上角，所以窗口坐标就是本视图坐标。
    fn open_menu(
        &mut self,
        target: MenuTarget,
        event: &MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = window.viewport_size();
        let x = f32::from(event.position.x);
        let y = f32::from(event.position.y);
        let max_x = (self.width - MENU_WIDTH - MENU_EDGE_GAP).max(MENU_EDGE_GAP);
        let max_y = (f32::from(viewport.height) - MENU_ESTIMATED_HEIGHT).max(0.0);
        self.menu = Some(ContextMenu {
            target,
            x: x.clamp(0.0, max_x),
            y: y.clamp(0.0, max_y),
            confirming: false,
        });
        cx.notify();
    }

    /// 一项页面导航；当前页高亮。
    fn render_nav_item(
        &self,
        view: MainView,
        icon: IconName,
        label: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.view == view;
        let hover_background = cx.theme().list_hover;
        let mut item = h_flex()
            .id(SharedString::from(format!("nav-{view:?}")))
            .items_center()
            .gap_3()
            .w_full()
            .min_h(px(40.0))
            .px_3()
            .rounded(cx.theme().radius)
            .text_sm()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.view = view;
                this.close_menu();
                cx.emit(SidebarEvent::OpenView(view));
                cx.notify();
            }))
            .child(icon.element(Size::Small));
        // 当前页保持选中底色，其余项用悬停底色：两者都画会在悬停时把选中态盖掉。
        if active {
            item = item.bg(cx.theme().list_active);
        } else {
            item = item.hover(move |this| this.bg(hover_background));
        }
        item.child(label.to_string()).into_any_element()
    }

    /// 「新对话」：它不是页面切换，而是「在某个项目里再开一条会话」。
    fn render_new_conversation_item(&self, cx: &mut Context<Self>) -> AnyElement {
        let hover_background = cx.theme().list_hover;
        h_flex()
            .id("nav-new-conversation")
            .items_center()
            .gap_3()
            .w_full()
            .min_h(px(40.0))
            .px_3()
            .rounded(cx.theme().radius)
            .text_sm()
            .hover(move |this| this.bg(hover_background))
            .on_click(cx.listener(|this, _, _, cx| {
                this.close_menu();
                cx.emit(SidebarEvent::NewSession);
            }))
            .child(IconName::Edit.element(Size::Small))
            .child("新对话".to_string())
            .into_any_element()
    }

    fn render_list_header(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.select_mode {
            return self.render_selection_card(cx);
        }
        let mut actions = h_flex().gap_1();
        if !self.sessions.is_empty() {
            actions = actions
                .child(
                    Button::new("refresh-sessions")
                        .ghost()
                        .label(if self.refreshing {
                            "刷新中"
                        } else {
                            "刷新"
                        })
                        .disabled(self.refreshing)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refreshing = true;
                            cx.emit(SidebarEvent::RefreshSessions);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("select-sessions")
                        .ghost()
                        .label("选择")
                        .on_click(cx.listener(|this, _, _, cx| this.enter_select_mode(cx))),
                );
        }
        h_flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("会话"),
            )
            .child(actions)
            .into_any_element()
    }

    /// 选择态顶部那块：计数 + 全选 + 删除，删除要先确认一次。
    ///
    /// 它顶替的是「会话 / 刷新 / 选择」那一行而不是浮层——前端也是把整块头换掉。
    fn render_selection_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.selected_ids.len();
        let mut card = v_flex()
            .mb_2()
            .gap_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .px_3()
            .py_2()
            .text_xs();

        if self.confirm_batch_delete {
            card = card
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("确认删除选中的 {count} 个会话？")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(Button::new("batch-cancel-confirm").label("取消").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.confirm_batch_delete = false;
                                cx.notify();
                            }),
                        ))
                        .child(
                            Button::new("batch-confirm")
                                .danger()
                                .label("删除")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let ids = this.selected_ids.clone();
                                    this.exit_select_mode(cx);
                                    cx.emit(SidebarEvent::DeleteSessions(ids));
                                })),
                        ),
                );
        } else {
            let all = session_list::all_selected(&self.selected_ids, &self.sessions);
            card = card
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("已选 {count} 项")),
                        )
                        .child(
                            Button::new("batch-exit")
                                .ghost()
                                .label("取消")
                                .on_click(cx.listener(|this, _, _, cx| this.exit_select_mode(cx))),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("batch-select-all")
                                .label(if all { "取消全选" } else { "全选" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let next = session_list::toggle_select_all(
                                        &this.selected_ids,
                                        &this.sessions,
                                    );
                                    this.selected_ids = next;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("batch-delete")
                                .danger()
                                .label("删除")
                                .disabled(count == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_batch_delete = true;
                                    cx.notify();
                                })),
                        ),
                );
        }
        card.into_any_element()
    }

    /// 一个项目分组：组头（名字 + 折叠箭头）与组内会话。
    fn render_project_group(
        &self,
        group: &session_list::ProjectGroup,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = self.collapsed.iter().any(|dir| dir == &group.working_dir);
        let is_active_project = {
            let active_dir = self
                .active
                .as_deref()
                .and_then(|active| self.working_dir_of(active));
            active_dir.as_deref() == Some(group.working_dir.as_str())
        };
        let header_background = if is_active_project && self.view == MainView::Chat {
            cx.theme().list_active
        } else {
            cx.theme().transparent
        };
        let header_hover = cx.theme().list_hover;
        let name = session_list::project_name(&group.working_dir).to_string();
        // 组头点击选中组内最近用过的会话；没有会话的组（理论上不会有）点了只切折叠。
        let latest = group
            .session_indices
            .first()
            .map(|index| self.sessions[*index].session_id.clone());
        let working_dir = group.working_dir.clone();
        let toggle_dir = working_dir.clone();
        let menu_dir = working_dir.clone();

        let header = h_flex()
            .id(SharedString::from(format!("project-{working_dir}")))
            .items_center()
            .w_full()
            .min_h(px(36.0))
            .rounded(cx.theme().radius)
            .bg(header_background)
            .hover(move |this| this.bg(header_hover))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // 选择态里组头不出菜单：那一刻整份列表在讲勾选，右键没有别的意思。
                    if this.select_mode {
                        return;
                    }
                    this.open_menu(MenuTarget::Project(menu_dir.clone()), event, window, cx);
                }),
            )
            .child(
                h_flex()
                    .id(SharedString::from(format!("project-open-{working_dir}")))
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .text_sm()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_menu();
                        // 选择态里组头只切折叠：点一下不该把当前会话切到别的项目去。
                        if this.select_mode {
                            this.toggle_collapsed(&toggle_dir, cx);
                            return;
                        }
                        if let Some(session_id) = &latest {
                            this.active = Some(session_id.clone());
                            cx.emit(SidebarEvent::Select(session_id.clone()));
                        } else {
                            this.toggle_collapsed(&toggle_dir, cx);
                        }
                        cx.notify();
                    }))
                    .child(IconName::Folder.element(Size::Small))
                    .child(div().min_w_0().truncate().child(name)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("project-toggle-{working_dir}")))
                    .mr_1()
                    .px_1()
                    .py_1()
                    .rounded(cx.theme().radius)
                    .text_color(cx.theme().muted_foreground)
                    .child(if collapsed {
                        IconName::ChevronRight.element(Size::Small).into_any_element()
                    } else {
                        IconName::ChevronRight
                            .element(Size::Small)
                            .rotate(radians(std::f32::consts::FRAC_PI_2))
                            .into_any_element()
                    })
                    // 箭头与组头都有点击处理，不拦就一次点击干两件事。
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_collapsed(&working_dir, cx);
                    })),
            );

        let mut column = v_flex().w_full().mb_2().child(header);
        if !collapsed {
            let rows: Vec<AnyElement> = group
                .session_indices
                .iter()
                .map(|index| self.render_session_row(&self.sessions[*index], cx))
                .collect();
            column = column.child(v_flex().w_full().pl_6().children(rows));
        }
        column.into_any_element()
    }

    fn render_session_row(&self, item: &SessionListItemDto, cx: &mut Context<Self>) -> AnyElement {
        let session_id = item.session_id.clone();
        let label = session_list::session_label(item).to_string();
        let is_selected = self.selected_ids.iter().any(|id| id == &session_id);
        // 选择态里不画「当前会话」的高亮：那一刻整份列表在讲另一件事（前端同口径）。
        let is_active = !self.select_mode
            && self.active.as_deref() == Some(session_id.as_str())
            && self.view == MainView::Chat;
        let background = if is_active {
            cx.theme().list_active
        } else {
            cx.theme().transparent
        };
        let hover_background = cx.theme().list_hover;

        let mut row = h_flex()
            .id(SharedString::from(format!("session-{session_id}")))
            .items_center()
            .gap_2()
            .w_full()
            .min_h(px(32.0))
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .text_sm()
            .bg(background)
            .hover(move |this| this.bg(hover_background));

        if self.select_mode {
            let toggle_id = session_id.clone();
            // 整行都是勾选框的一部分：选择态里点一行不该切走当前会话。
            row = row
                .child(render_selection_checkbox(is_selected, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let next = session_list::toggle_selected(&this.selected_ids, &toggle_id);
                    this.selected_ids = next;
                    cx.notify();
                }));
        } else {
            let menu_session = session_id.clone();
            row = row
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_menu();
                    this.active = Some(session_id.clone());
                    cx.emit(SidebarEvent::Select(session_id.clone()));
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.open_menu(
                            MenuTarget::Session(menu_session.clone()),
                            event,
                            window,
                            cx,
                        );
                    }),
                );
        }

        row.child(div().flex_1().min_w_0().truncate().child(label))
            .into_any_element()
    }

    /// 右键菜单的内容：未确认时给动作，确认后给一串删除确认。
    fn render_menu_body(&self, menu: &ContextMenu, cx: &mut Context<Self>) -> AnyElement {
        let label = match &menu.target {
            MenuTarget::Session(_) => "此会话",
            MenuTarget::Project(_) => "此项目及其所有会话",
        };
        let mut body = v_flex().w_full().py_1();
        if menu.confirming {
            let target = menu.target.clone();
            body = body
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("确认删除{label}？")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .child(
                            Button::new("menu-cancel")
                                .label("取消")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_menu();
                                    cx.notify();
                                })),
                        )
                        .child(Button::new("menu-confirm").danger().label("删除").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.close_menu();
                                match &target {
                                    MenuTarget::Session(id) => {
                                        cx.emit(SidebarEvent::DeleteSession(id.clone()))
                                    },
                                    MenuTarget::Project(dir) => {
                                        cx.emit(SidebarEvent::DeleteProject(dir.clone()))
                                    },
                                }
                                cx.notify();
                            }),
                        )),
                );
        } else {
            if let MenuTarget::Session(id) = &menu.target {
                let id = id.clone();
                body = body.child(
                    Button::new("menu-fork")
                        .ghost()
                        .label("Fork 会话")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu();
                            cx.emit(SidebarEvent::ForkSession(id.clone()));
                            cx.notify();
                        })),
                );
            }
            body = body.child(
                Button::new("menu-delete")
                    .ghost()
                    .label(match &menu.target {
                        MenuTarget::Session(_) => "删除会话",
                        MenuTarget::Project(_) => "删除项目",
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // 先落成确认态：删除是破坏性的，一次点击不该直接生效。
                        if let Some(menu) = this.menu.as_mut() {
                            menu.confirming = true;
                        }
                        cx.notify();
                    })),
            );
        }

        body.into_any_element()
    }

    /// 菜单层：铺满侧边栏的一层，菜单本体是它的流内子元素。
    ///
    /// 不用「绝对定位 + 高度由内容决定」的浮层：实测那种浮层的命中框不跟着内容走，于是菜单
    /// 画得对、点哪儿都不中（收起的点击反而被别的元素接住）。这里两层都简单——层有确定的
    /// 满尺寸，菜单是流内元素、只靠外边距挪到点击处。
    fn render_menu_layer(&self, menu: &ContextMenu, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("sidebar-menu-layer")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            // 点左右键都收起：右键落在别的行上时不该什么都不发生。
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.dismiss_menu(cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| this.dismiss_menu(cx)),
            )
            .child(
                div()
                    .id("sidebar-menu")
                    .mt(px(menu.y))
                    .ml(px(menu.x))
                    .w(px(MENU_WIDTH))
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius_lg)
                    // 点在菜单自己身上不算「外面」：拦下这一下，不让它冒泡到层上。
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                    )
                    .child(self.render_menu_body(menu, cx)),
            )
            .into_any_element()
    }
}

/// 选择态里每行左边的勾选框。
///
/// 未勾选时勾仍占位、只是画成透明：与前端一样只换底色，行高不因此变。描边与填充都走
/// `primary`——本主题的 `accent` 是一块浅色底，当描边用会与侧边栏底色糊在一起。
fn render_selection_checkbox(checked: bool, cx: &Context<Sidebar>) -> AnyElement {
    let (border, glyph) = if checked {
        (cx.theme().primary, cx.theme().primary_foreground)
    } else {
        (cx.theme().border, cx.theme().transparent)
    };
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(16.0))
        // 主题没有比 `radius` 更小的档，而 8px 在 16px 的方框上已经是半高、会读成圆形；
        // 取最小的那个档（框架自己给小控件也是这么钳的，见 gpui-component 的 `Checkbox`）。
        .rounded(cx.theme().radius.min(px(4.0)))
        .border_1()
        .border_color(border)
        .bg(if checked {
            cx.theme().primary
        } else {
            cx.theme().transparent
        })
        .child(
            IconName::Check
                .element(Size::Size(px(11.0)))
                .text_color(glyph),
        )
        .into_any_element()
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = session_list::group_sessions(&self.sessions, &self.project_order);
        let rows: Vec<AnyElement> = groups
            .iter()
            .map(|group| self.render_project_group(group, cx))
            .collect();

        let mut nav = v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .child(self.render_new_conversation_item(cx));
        nav = nav.child(self.render_nav_item(MainView::Code, IconName::Terminal, "代码", cx));
        if self.kanban_available {
            nav = nav.child(self.render_nav_item(MainView::Kanban, IconName::Board, "看板", cx));
        }

        // 横向脊线：整列留 `px_3` 边距，行与页头再各自加 `px_2` 内缩，于是「AstrCode」、导航项、
        // 项目名与会话名的文字都落在同一条线上，行的选中底色也都从同一条边起。
        let mut column = v_flex()
            .id("sidebar")
            .relative()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                page_header(cx)
                    .px_3()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("AstrCode"),
                    )
                    .child(icon_button(
                        "sidebar-new-project",
                        IconName::Plus,
                        "新建项目",
                        cx,
                        |this: &mut Sidebar, cx| {
                            this.close_menu();
                            cx.emit(SidebarEvent::NewProject);
                        },
                    ))
                    .child(icon_button(
                        "sidebar-collapse",
                        IconName::Sidebar,
                        "收起侧边栏",
                        cx,
                        |this: &mut Sidebar, cx| {
                            this.close_menu();
                            cx.emit(SidebarEvent::ToggleSidebar);
                        },
                    )),
            )
            .child(nav)
            .child(
                div()
                    .id("session-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .px_3()
                            .py_2()
                            .child(self.render_list_header(cx))
                            .children(rows),
                    ),
            );

        if let Some(error) = &self.error {
            column = column.child(
                div()
                    .p_3()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }

        // 页脚：身份标识贴底，右侧是设置入口（前端 `Sidebar.tsx` 页脚那枚按钮）。
        column = column.child(
            h_flex()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .min_w_0()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(cx.theme().sidebar_border)
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .size(px(24.0))
                        .rounded(cx.theme().radius_full())
                        .bg(theme::brand_avatar_background())
                        .text_size(px(9.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::brand_avatar_foreground())
                        .child("AS"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().muted_foreground)
                        .child("AstrCode"),
                )
                .child(icon_button(
                    "sidebar-settings",
                    IconName::Settings,
                    "设置",
                    cx,
                    |this: &mut Sidebar, cx| {
                        this.close_menu();
                        cx.emit(SidebarEvent::OpenView(MainView::Settings));
                    },
                )),
        );

        if let Some(menu) = &self.menu {
            column = column.child(self.render_menu_layer(menu, cx));
        }

        column
    }
}
