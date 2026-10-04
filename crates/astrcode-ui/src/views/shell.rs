//! 应用外壳：左侧会话列表 + 右侧会话面板。
//!
//! 外壳是几件跨页状态的唯一持有者：会话列表、界面偏好（侧边栏宽度与折叠集合）、当前页。
//! 侧边栏只发事件，落盘与重取都回到这里做。

use std::collections::HashMap;

use astrcode_protocol::http::{SessionListItemDto, UiPreferencesResponseDto};
use gpui_kit::{
    AppContext as _, AsyncApp, Context, Entity, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Render, Styled as _, Subscription, Task,
    Window, component::h_flex, div, px,
};

use super::{
    MainView,
    chat::{ChatEvent, ChatView},
    display_title,
    kanban::{KanbanEvent, KanbanView},
    new_project::{NewProjectEvent, NewProjectModal},
    settings::{SettingsEvent, SettingsView},
    sidebar::{Sidebar, SidebarEvent},
};
use crate::{
    api::{Api, ApiError},
    kanban,
    preferences::{self, PendingPreferences, UiPreferences},
    session_list,
};

/// 侧边栏右边缘那条宽度拖拽把手的宽度。
const SIDEBAR_HANDLE_WIDTH: f32 = 4.0;

/// 打开着的「新建项目」弹窗。
///
/// 订阅与弹窗同生共死：弹窗每次打开都重建（输入框要 `Window`，候选也要一份新的快照），
/// 订阅跟着它一起换掉，不必单独退订。
struct NewProjectDialog {
    modal: Entity<NewProjectModal>,
    _subscription: Subscription,
}

pub struct Shell {
    api: Api,
    /// 新建会话时提交给 server 的工作目录；由宿主注入（Web 宿主没有进程当前目录）。
    working_dir: String,
    sidebar: Entity<Sidebar>,
    chat: Entity<ChatView>,
    kanban: Entity<KanbanView>,
    settings: Entity<SettingsView>,
    /// 主区域当前显示的那一页。
    main_view: MainView,
    /// 侧边栏是否显示；收起后由主区域的页头给展开入口。
    sidebar_open: bool,
    /// 打开着的「新建项目」弹窗。
    new_project: Option<NewProjectDialog>,
    /// 看板扩展是否可用；它决定侧边栏的入口与看板页是否允许停留。
    kanban_available: bool,
    /// 侧边栏宽度（px）。拖拽期间只有它变，松手才落盘。
    sidebar_width: f32,
    /// 界面偏好在 UI 侧的当前值；写回是整份替换，所以必须留着一份。
    preferences: UiPreferences,
    /// 偏好是否已经从服务端取回。
    preferences_loaded: bool,
    /// 取回之前发生的本地改动；基准（服务端那份）到了之后按字段重放。
    preferences_pending: PendingPreferences,
    /// 拖拽起点：按下时的指针 x 与当时的宽度。
    drag_origin: Option<(f32, f32)>,
    /// 会话列表的加载/刷新任务；换一次它的句柄即取消上一次。
    session_task: Option<Task<()>>,
    /// 分叉请求任务；与列表刷新分成两个句柄，后者换掉前者时不会把分叉中途掐掉。
    fork_task: Option<Task<()>>,
    /// 扩展清单的拉取任务。
    extensions_task: Option<Task<()>>,
    /// 偏好读写的任务。
    preferences_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(api: Api, working_dir: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(Sidebar::new);
        let chat = cx.new(|cx| ChatView::new(api.clone(), window, cx));
        let kanban = cx.new(|_| KanbanView::new(api.clone()));
        let settings = cx.new(|cx| SettingsView::new(api.clone(), window, cx));
        // 侧边栏一开始是显示的，三个主区域的页头因此都不挂展开入口。
        chat.update(cx, |chat, cx| chat.set_sidebar_open(true, cx));
        kanban.update(cx, |kanban, cx| kanban.set_sidebar_open(true, cx));
        settings.update(cx, |settings, cx| settings.set_sidebar_open(true, cx));
        let subscriptions = vec![
            // 用 `subscribe_in` 而不是 `subscribe`：打开新建项目弹窗要建 `InputState`，
            // 那需要 `Window`，而事件在订阅里处理。
            cx.subscribe_in(
                &sidebar,
                window,
                |this, _, event: &SidebarEvent, window, cx| match event {
                    SidebarEvent::Select(session_id) => {
                        // 侧边栏里的会话属于对话页：从看板点回来时也要换页，与卡片跳转同一路。
                        this.set_main_view(MainView::Chat, cx);
                        this.open_session(session_id.clone(), cx);
                    },
                    SidebarEvent::NewSession => this.create_conversation(window, cx),
                    SidebarEvent::NewProject => this.open_new_project(window, cx),
                    SidebarEvent::OpenView(view) => this.set_main_view(*view, cx),
                    SidebarEvent::ToggleSidebar => this.toggle_sidebar(cx),
                    SidebarEvent::RefreshSessions => this.sync_sessions(None, cx),
                    SidebarEvent::ForkSession(session_id) => {
                        this.fork_session(session_id.clone(), cx)
                    },
                    SidebarEvent::DeleteSession(session_id) => {
                        this.delete_session(session_id.clone(), cx)
                    },
                    SidebarEvent::DeleteProject(working_dir) => {
                        this.delete_project(working_dir.clone(), cx)
                    },
                    SidebarEvent::DeleteSessions(session_ids) => {
                        this.delete_sessions(session_ids.clone(), cx)
                    },
                    SidebarEvent::CollapsedChanged(collapsed) => {
                        this.preferences.collapsed_project_dirs = collapsed.clone();
                        this.note_preference_edit(|pending| {
                            pending.set_collapsed_project_dirs(collapsed.clone())
                        });
                        this.persist_preferences(cx);
                    },
                },
            ),
            cx.subscribe(&chat, |this, _, event: &ChatEvent, cx| match event {
                ChatEvent::SessionForked(session_id) => {
                    this.refresh_and_select(Some(session_id.clone()), cx);
                },
                // 子 Agent 卡的「查看子会话」：切到子会话这一侧与点列表项同路，
                // 子会话不在当前列表里时拿不到标题，顶栏留空。
                ChatEvent::OpenSession(session_id) => this.open_session(session_id.clone(), cx),
                ChatEvent::ToggleSidebar => this.toggle_sidebar(cx),
            }),
            // 卡片上的点击落在看板页里，跳对话意味着从看板回到对话页。
            cx.subscribe(&kanban, |this, _, event: &KanbanEvent, cx| match event {
                KanbanEvent::OpenSession(session_id) => {
                    this.set_main_view(MainView::Chat, cx);
                    this.open_session(session_id.clone(), cx);
                },
                KanbanEvent::ToggleSidebar => this.toggle_sidebar(cx),
                // 卡片用过与删过的目录：与新建项目走的是同一条偏好写回。
                KanbanEvent::RememberProjectPath(path) => this.remember_project_path(path, cx),
                KanbanEvent::ForgetProjectPath(path) => this.forget_project_path(path, cx),
            }),
            // 设置页改过主模型或权限后，会话面板的工具条按钮要跟着变（前端
            // `bumpModelRefreshKey`）；改过扩展启停后，看板入口跟着出现或消失。
            cx.subscribe(
                &settings,
                |this, _, event: &SettingsEvent, cx| match event {
                    SettingsEvent::ToggleSidebar => this.toggle_sidebar(cx),
                    SettingsEvent::ModelChanged => {
                        this.chat
                            .update(cx, |chat, cx| chat.refresh_model_config(cx));
                    },
                    SettingsEvent::ExtensionsChanged => this.refresh_extensions(cx),
                },
            ),
        ];

        let mut shell = Self {
            api,
            working_dir,
            sidebar,
            chat,
            kanban,
            settings,
            main_view: MainView::Chat,
            sidebar_open: true,
            new_project: None,
            kanban_available: false,
            sidebar_width: preferences::SIDEBAR_WIDTH_DEFAULT as f32,
            preferences: UiPreferences::defaults(),
            preferences_loaded: false,
            preferences_pending: PendingPreferences::default(),
            drag_origin: None,
            session_task: None,
            fork_task: None,
            extensions_task: None,
            preferences_task: None,
            _subscriptions: subscriptions,
        };
        shell.sync_sessions(None, cx);
        shell.refresh_extensions(cx);
        shell.load_preferences(cx);
        shell
    }

    /// 切主区域；看板扩展不可用时不允许停在看板页。
    fn set_main_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        let view = match view {
            MainView::Kanban if !self.kanban_available => MainView::Chat,
            view => view,
        };
        self.main_view = view;
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_view(view, cx));
        // 看板只在显示时轮询，因此每次切换都要告诉它当前是否在屏幕上；设置页只在显示时
        // 重取配置，同理。
        let visible = view == MainView::Kanban;
        self.kanban
            .update(cx, |kanban, cx| kanban.set_visible(visible, cx));
        let visible = view == MainView::Settings;
        self.settings
            .update(cx, |settings, cx| settings.set_visible(visible, cx));
        cx.notify();
    }

    /// 拉扩展清单：看板入口是否出现全看它。
    ///
    /// 拉不到就不给入口（与 Web 前端同判据）：装了但禁用、或加载失败时，
    /// 页面没有可用路由可打，入口指向的只会是一个打不通的页面。
    fn refresh_extensions(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let sidebar = self.sidebar.clone();
        self.extensions_task = Some(cx.spawn(async move |this, cx| {
            let Ok(extensions) = api.list_extensions().await else {
                return;
            };
            let available = kanban::extension_available(&extensions);
            sidebar.update(cx, |sidebar, cx| {
                sidebar.set_kanban_available(available, cx)
            });
            this.update(cx, |this, cx| {
                this.kanban_available = available;
                if !available && this.main_view == MainView::Kanban {
                    this.set_main_view(MainView::Chat, cx);
                }
            })
            .ok();
        }));
    }

    /// 取一次界面偏好。取不到就用默认值跑：界面照常可用，落盘等下一次改动。
    fn load_preferences(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.preferences_task = Some(cx.spawn(async move |this, cx| {
            let Ok(response) = api.ui_preferences().await else {
                return;
            };
            this.update(cx, |this, cx| this.adopt_preferences(&response, cx))
                .ok();
        }));
    }

    /// 偏好到位：把攒下的改动重放到服务端那份上，并把结果装到界面上。
    ///
    /// 取回之前本地那份只是 delta 而不是基准（见 [`Self::note_preference_edit`]），所以这里
    /// 必须重放、不能在「本地那份」与「服务端那份」之间二选一：本地动过的字段取改动，没动过
    /// 的字段取服务端存着的值。
    fn adopt_preferences(&mut self, response: &UiPreferencesResponseDto, cx: &mut Context<Self>) {
        let pending = std::mem::take(&mut self.preferences_pending);
        let has_edits = !pending.is_empty();
        self.preferences = pending.merge(UiPreferences::from_response(response));
        // 标记要在 `apply_preferences` 之前落上：它会走一遍 `set_sidebar_width`，
        // 那一刻起就已经不在「取回之前」了。
        self.preferences_loaded = true;
        self.apply_preferences(cx);
        if has_edits {
            self.persist_preferences(cx);
        }
    }

    /// 把偏好里的宽度与折叠集合装到界面上。
    fn apply_preferences(&mut self, cx: &mut Context<Self>) {
        let width = self.preferences.sidebar_width as f32;
        let collapsed = self.preferences.collapsed_project_dirs.clone();
        self.set_sidebar_width(width, cx);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_collapsed_dirs(collapsed, cx));
        self.sync_kanban_paths(cx);
    }

    /// 把看板「新建卡片」弹窗要用的路径来源推过去。
    ///
    /// 与新建项目弹窗取的是同一批偏好字段；默认目录取当前选中会话的工作目录，会话一换
    /// 也要重推（见 [`Self::open_session`]）。
    fn sync_kanban_paths(&mut self, cx: &mut Context<Self>) {
        let default_working_dir = self
            .sidebar
            .read(cx)
            .active_working_dir()
            .unwrap_or_default();
        let project_paths = self.preferences.kanban_project_paths.clone();
        let ignored_paths = self.preferences.kanban_ignored_project_paths.clone();
        self.kanban.update(cx, |kanban, cx| {
            kanban.set_project_paths(default_working_dir, project_paths, ignored_paths, cx)
        });
    }

    /// 记下一次本地改动。
    ///
    /// 偏好取回之前本地那份还不是「值」而是 delta：基准还没到，照着它做整份替换会把没动过的
    /// 字段打回初值。那段时间的改动因此先攒起来，等基准到了再重放；取回之后改动直接生效，
    /// 这里就是空转。
    fn note_preference_edit(&mut self, edit: impl FnOnce(&mut PendingPreferences)) {
        if !self.preferences_loaded {
            edit(&mut self.preferences_pending);
        }
    }

    /// 把界面此刻的宽度与折叠集合写回偏好文件。
    ///
    /// 写回是整份替换，请求体由本地 [`UiPreferences`] 拼出，看板那两项一并带着。
    fn persist_preferences(&mut self, cx: &mut Context<Self>) {
        if !self.preferences_loaded {
            // 基准还没到：此刻写回是拿着本地那堆初值做整份替换，会把服务端存着的宽度、折叠集合
            // 与看板路径一并抹掉。改动已经记在 [`Self::preferences_pending`] 里，取回那一步会
            // 连同这次写回一起做（见 [`Self::adopt_preferences`]）。
            return;
        }
        // 折叠集合取本地这一份，不回读侧边栏：`CollapsedChanged` 已经把它同步住了，而侧边栏
        // 那份在会话列表还没到的时候会被剪成空——回读会把「还没加载」写成「用户全展开了」。
        self.preferences.sidebar_width = f64::from(self.sidebar_width);

        let api = self.api.clone();
        let request = self.preferences.update_request();
        self.preferences_task = Some(cx.spawn(async move |this, cx| {
            if let Err(error) = api.save_ui_preferences(&request).await {
                this.update(cx, |this, cx| this.show_error(error.to_string(), cx))
                    .ok();
            }
        }));
    }

    /// 拖侧边栏右边缘：宽度实时跟手，松手才落盘（与前端 `useSidebarResize` 同节奏）。
    fn drag_sidebar(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((origin_x, origin_width)) = self.drag_origin else {
            return;
        };
        let next = origin_width + (f32::from(event.position.x) - origin_x);
        let width = self.preferences.set_sidebar_width(f64::from(next)) as f32;
        self.set_sidebar_width(width, cx);
    }

    /// 改宽度：只改界面，不写盘——写盘在松手时一次做完。
    fn set_sidebar_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if self.sidebar_width == width {
            return;
        }
        self.note_preference_edit(|pending| pending.set_sidebar_width(f64::from(width)));
        self.sidebar_width = width;
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_width(width, cx));
        cx.notify();
    }

    /// 松手：结束拖拽并落盘。
    fn commit_sidebar_width(&mut self, cx: &mut Context<Self>) {
        if self.drag_origin.take().is_none() {
            return;
        }
        self.persist_preferences(cx);
    }

    /// 把一条消息放到侧边栏底部的错误位；这一格是外壳与列表共用的错误出口。
    fn show_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_error(message, cx));
    }

    fn open_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let title = self.sidebar.read(cx).session_title(&session_id);
        let working_dir = self.sidebar.read(cx).working_dir_of(&session_id);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_active(&session_id, cx));
        self.chat.update(cx, |chat, cx| {
            chat.open_session(session_id, cx);
            // 状态行上的项目名与标题一样，都是会话的属性，只有外壳手里有。
            chat.set_working_dir(working_dir, cx);
            if let Some(title) = title {
                chat.set_title(title, cx);
            }
        });
        // 看板的新建卡片弹窗默认目录跟着当前会话走。
        self.sync_kanban_paths(cx);
    }

    /// 拉取会话列表；`create` 有值时先在该目录下建一个新会话。
    ///
    /// 列表为空时也会建一个（在宿主给的工作目录下），否则首次启动没有任何可选中项。
    fn sync_sessions(&mut self, create: Option<String>, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let working_dir = self.working_dir.clone();
        let sidebar = self.sidebar.clone();
        let chat = self.chat.clone();

        self.session_task = Some(cx.spawn(async move |_this, cx| {
            match fetch_sessions(&api, create.as_deref(), &working_dir).await {
                Ok((sessions, created)) => {
                    let target =
                        created.or_else(|| sessions.first().map(|item| item.session_id.clone()));
                    apply_session_list(&sidebar, &chat, sessions, target, cx);
                },
                Err(error) => {
                    sidebar.update(cx, |sidebar, cx| sidebar.set_error(error.to_string(), cx));
                },
            }
        }));
    }

    /// 「新对话」：在当前项目里开一条新会话，并切回对话页。
    ///
    /// 一个项目都没有时改开「新建项目」弹窗（前端 `handleCreateConversation` 同判据）：
    /// 没有项目就没有「当前项目」，退回宿主工作目录会建出一条用户没挑过目录的会话。
    fn create_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(working_dir) = self.sidebar.read(cx).active_working_dir() else {
            self.open_new_project(window, cx);
            return;
        };
        self.set_main_view(MainView::Chat, cx);
        self.sync_sessions(Some(working_dir), cx);
    }

    /// 打开「新建项目」弹窗：默认目录、候选与历史都取此刻的快照。
    ///
    /// 候选里的历史与忽略集来自偏好（前端读 localStorage，这里读偏好文件）；弹窗自己那份只在
    /// 弹窗生命周期内有效，删候选由 [`Self::forget_project_path`] 落盘。
    fn open_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (default_working_dir, extra_candidates) = {
            let sidebar = self.sidebar.read(cx);
            (
                sidebar
                    .active_working_dir()
                    .unwrap_or_else(|| self.working_dir.clone()),
                sidebar.project_working_dirs(),
            )
        };
        let history = kanban::normalize_path_list(
            &self.preferences.kanban_project_paths,
            kanban::PROJECT_PATH_HISTORY_LIMIT,
        );
        let ignored = kanban::normalize_path_list(
            &self.preferences.kanban_ignored_project_paths,
            kanban::IGNORED_PROJECT_PATH_LIMIT,
        );
        let api = self.api.clone();
        let modal = cx.new(|cx| {
            NewProjectModal::new(
                api,
                default_working_dir,
                extra_candidates,
                history,
                ignored,
                window,
                cx,
            )
        });
        let subscription = cx.subscribe_in(
            &modal,
            window,
            |this, _, event: &NewProjectEvent, _, cx| match event {
                NewProjectEvent::Create(working_dir) => {
                    this.create_project(working_dir.clone(), cx)
                },
                NewProjectEvent::ForgetPath(path) => this.forget_project_path(path, cx),
                NewProjectEvent::Close => this.close_new_project(cx),
            },
        );
        self.new_project = Some(NewProjectDialog {
            modal,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// 关掉「新建项目」弹窗；丢掉实体即丢掉它里面的订阅。
    fn close_new_project(&mut self, cx: &mut Context<Self>) {
        if self.new_project.take().is_some() {
            cx.notify();
        }
    }

    /// 在给定目录下建一条会话：成功就记住这个路径、关掉弹窗并切到对话页。
    ///
    /// 失败把消息送回弹窗（弹窗还开着，用户能改路径重试），不像别处那样落到侧边栏底部的错误
    /// 位——那时弹窗正挡着错误位，消息会被埋掉。
    fn create_project(&mut self, working_dir: String, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let sidebar = self.sidebar.clone();
        let chat = self.chat.clone();
        let Some(dialog) = &self.new_project else {
            return;
        };
        let modal = dialog.modal.clone();

        self.session_task = Some(cx.spawn(async move |this, cx| {
            let created = match api.create_session(&working_dir).await {
                Ok(session_id) => session_id,
                Err(error) => {
                    let message = error.to_string();
                    modal.update(cx, |modal, cx| modal.show_error(message, cx));
                    return;
                },
            };
            let sessions = match api.list_sessions().await {
                Ok(sessions) => sessions,
                Err(error) => {
                    let message = error.to_string();
                    modal.update(cx, |modal, cx| modal.show_error(message, cx));
                    return;
                },
            };
            apply_session_list(&sidebar, &chat, sessions, Some(created), cx);
            this.update(cx, |this, cx| {
                this.remember_project_path(&working_dir, cx);
                this.close_new_project(cx);
                this.set_main_view(MainView::Chat, cx);
            })
            .ok();
        }));
    }

    /// 记住一条项目路径：清掉它的忽略记录并写回偏好。
    fn remember_project_path(&mut self, working_dir: &str, cx: &mut Context<Self>) {
        let (paths, ignored) = kanban::remember_project_path(
            &self.preferences.kanban_project_paths,
            &self.preferences.kanban_ignored_project_paths,
            working_dir,
        );
        self.preferences.kanban_project_paths = paths;
        self.preferences.kanban_ignored_project_paths = ignored;
        self.note_preference_edit(|pending| pending.remember_project_path(working_dir));
        self.persist_preferences(cx);
    }

    /// 从候选里删掉一条路径：清历史，并记进忽略集。
    fn forget_project_path(&mut self, working_dir: &str, cx: &mut Context<Self>) {
        let (paths, ignored) = kanban::forget_project_path(
            &self.preferences.kanban_project_paths,
            &self.preferences.kanban_ignored_project_paths,
            working_dir,
        );
        self.preferences.kanban_project_paths = paths;
        self.preferences.kanban_ignored_project_paths = ignored;
        self.note_preference_edit(|pending| pending.forget_project_path(working_dir));
        self.persist_preferences(cx);
    }

    /// 收起或展开侧边栏；收起后两个主区域的页头出现展开入口。
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        let open = self.sidebar_open;
        self.chat
            .update(cx, |chat, cx| chat.set_sidebar_open(open, cx));
        self.kanban
            .update(cx, |kanban, cx| kanban.set_sidebar_open(open, cx));
        self.settings
            .update(cx, |settings, cx| settings.set_sidebar_open(open, cx));
        cx.notify();
    }

    /// 刷新列表并选中 `target`。
    fn refresh_and_select(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let sidebar = self.sidebar.clone();
        let chat = self.chat.clone();

        self.session_task = Some(cx.spawn(
            async move |_this, cx| match api.list_sessions().await {
                Ok(sessions) => apply_session_list(&sidebar, &chat, sessions, target, cx),
                Err(error) => {
                    sidebar.update(cx, |sidebar, cx| sidebar.set_error(error.to_string(), cx));
                },
            },
        ));
    }

    /// 从某个会话分叉：先建新会话，再刷新列表并选中它。
    fn fork_session(&mut self, source_id: String, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let sidebar = self.sidebar.clone();

        self.fork_task = Some(cx.spawn(async move |this, cx| {
            let new_session = match api.fork_session(&source_id, None).await {
                Ok(session_id) => session_id,
                Err(error) => {
                    sidebar.update(cx, |sidebar, cx| sidebar.set_error(error.to_string(), cx));
                    return;
                },
            };
            this.update(cx, |this, cx| {
                this.refresh_and_select(Some(new_session), cx)
            })
            .ok();
        }));
    }

    /// 删除一个会话：删掉之后重新决定选中谁。
    fn delete_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let sidebar = self.sidebar.clone();
        let chat = self.chat.clone();
        let active = self.sidebar.read(cx).active_id().map(str::to_owned);

        self.session_task = Some(cx.spawn(async move |_this, cx| {
            if let Err(error) = api.delete_session(&session_id).await {
                sidebar.update(cx, |sidebar, cx| sidebar.set_error(error.to_string(), cx));
                return;
            }
            match api.list_sessions().await {
                Ok(sessions) => {
                    let target =
                        session_list::pick_active_after_delete(&sessions, active.as_deref(), None);
                    apply_session_list(&sidebar, &chat, sessions, target, cx);
                },
                Err(error) => {
                    sidebar.update(cx, |sidebar, cx| sidebar.set_error(error.to_string(), cx));
                },
            }
        }));
    }

    /// 删除一个项目及其全部会话。
    ///
    /// 被删的可能正是当前会话所在的项目，所以之后要重新决定选中谁。
    fn delete_project(&mut self, working_dir: String, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let sidebar = self.sidebar.clone();
        let chat = self.chat.clone();
        let active = self.sidebar.read(cx).active_id().map(str::to_owned);

        self.session_task = Some(cx.spawn(async move |_this, cx| {
            if let Err(error) = api.delete_project(&working_dir).await {
                sidebar.update(cx, |sidebar, cx| sidebar.set_error(error.to_string(), cx));
                return;
            }
            match api.list_sessions().await {
                Ok(sessions) => {
                    let target = session_list::pick_active_after_delete(
                        &sessions,
                        active.as_deref(),
                        Some(&working_dir),
                    );
                    apply_session_list(&sidebar, &chat, sessions, target, cx);
                },
                Err(error) => {
                    sidebar.update(cx, |sidebar, cx| sidebar.set_error(error.to_string(), cx));
                },
            }
        }));
    }

    /// 批量删除会话（选择态里确认过的那一批）。
    ///
    /// 一条失败不挡住其余：这里是 N 次单删（前端 `deleteSessions` 同样逐条发），失败的那些
    /// 报一句出来，成功的那批照常消失。
    fn delete_sessions(&mut self, session_ids: Vec<String>, cx: &mut Context<Self>) {
        let api = self.api.clone();
        let sidebar = self.sidebar.clone();
        let chat = self.chat.clone();
        let active = self.sidebar.read(cx).active_id().map(str::to_owned);

        self.session_task = Some(cx.spawn(async move |_this, cx| {
            let mut failure: Option<String> = None;
            for session_id in &session_ids {
                if let Err(error) = api.delete_session(session_id).await {
                    tracing::warn!(session_id, %error, "删除会话失败");
                    failure.get_or_insert_with(|| error.to_string());
                }
            }
            match api.list_sessions().await {
                Ok(sessions) => {
                    let target =
                        session_list::pick_active_after_delete(&sessions, active.as_deref(), None);
                    apply_session_list(&sidebar, &chat, sessions, target, cx);
                    // 报错放在列表落地之后：`set_sessions` 会把上一次的错误清掉。
                    if let Some(message) = failure {
                        sidebar.update(cx, |sidebar, cx| sidebar.set_error(message, cx));
                    }
                },
                Err(error) => {
                    sidebar.update(cx, |sidebar, cx| sidebar.set_error(error.to_string(), cx));
                },
            }
        }));
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match self.main_view {
            MainView::Chat => self.chat.clone().into_any_element(),
            MainView::Kanban => self.kanban.clone().into_any_element(),
            MainView::Settings => self.settings.clone().into_any_element(),
        };
        let width = self.sidebar_width;

        let mut root = h_flex()
            .relative()
            .size_full()
            // 拖拽期间的指针移动与松手都落在根上：把手只有 4px 宽，拖快了指针就跑出去了。
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| this.drag_sidebar(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.commit_sidebar_width(cx)),
            );
        // 收起时不画侧边栏，也不画那条把手：两者是一件事的两个部分。
        if self.sidebar_open {
            root = root
                .child(
                    div()
                        .w(px(width))
                        .h_full()
                        .flex_shrink_0()
                        .child(self.sidebar.clone()),
                )
                .child(
                    div()
                        .id("sidebar-resize")
                        .w(px(SIDEBAR_HANDLE_WIDTH))
                        .h_full()
                        .flex_shrink_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.drag_origin =
                                    Some((f32::from(event.position.x), this.sidebar_width));
                                cx.notify();
                            }),
                        ),
                );
        }
        root = root.child(div().flex_1().min_w_0().h_full().child(main));
        // 弹窗铺满整层，画在最后因此盖住其余部分。
        if let Some(dialog) = &self.new_project {
            root = root.child(dialog.modal.clone());
        }
        root
    }
}

/// 把会话列表推给侧边栏，并把 `target` 选上。
///
/// `target` 为空表示列表里已经没有可选的会话（会话或项目被删光），此时把会话面板清空。
fn apply_session_list(
    sidebar: &Entity<Sidebar>,
    chat: &Entity<ChatView>,
    sessions: Vec<SessionListItemDto>,
    target: Option<String>,
    cx: &mut AsyncApp,
) {
    // 会话列表马上要移进侧边栏，标题与工作目录在这一侧取好：当前会话的给顶栏与状态行，
    // 全量的标题给跨会话问卷横幅。
    let titles: HashMap<String, String> = sessions
        .iter()
        .map(|item| (item.session_id.clone(), display_title(item)))
        .collect();
    let title = target
        .as_deref()
        .and_then(|session_id| titles.get(session_id).cloned());
    let working_dir = target
        .as_deref()
        .and_then(|session_id| sessions.iter().find(|item| item.session_id == session_id))
        .map(|item| item.working_dir.clone());
    sidebar.update(cx, |sidebar, cx| sidebar.set_sessions(sessions, cx));
    chat.update(cx, |chat, cx| chat.set_session_titles(titles, cx));
    match target {
        Some(session_id) => {
            sidebar.update(cx, |sidebar, cx| sidebar.set_active(&session_id, cx));
            chat.update(cx, |chat, cx| {
                chat.open_session(session_id, cx);
                chat.set_working_dir(working_dir, cx);
                if let Some(title) = title {
                    chat.set_title(title, cx);
                }
            });
        },
        None => chat.update(cx, |chat, cx| chat.clear_session(cx)),
    }
}

/// 取会话列表；`create` 有值时先在该目录下建一个，返回新建的会话 id。
async fn fetch_sessions(
    api: &Api,
    create: Option<&str>,
    working_dir: &str,
) -> Result<(Vec<SessionListItemDto>, Option<String>), ApiError> {
    if let Some(working_dir) = create {
        let created = api.create_session(working_dir).await?;
        let sessions = api.list_sessions().await?;
        return Ok((sessions, Some(created)));
    }

    let sessions = api.list_sessions().await?;
    if sessions.is_empty() {
        // 首次启动：没有会话可选中时先建一个，避免落在空白界面。
        let created = api.create_session(working_dir).await?;
        let sessions = api.list_sessions().await?;
        return Ok((sessions, Some(created)));
    }
    Ok((sessions, None))
}
