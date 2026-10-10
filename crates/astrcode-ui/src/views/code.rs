//! 代码页：左侧文件树 + 右侧「正文 / 变更」两栏。
//!
//! 数据全部走 server 的 `/api/files/*`：`astrcode-ui` 是宿主无关层，自己读不了磁盘，两个宿主
//! 因此共用这一页（ADR 0001）。浏览根目录由外壳注入（当前会话的工作目录）。
//!
//! 取用是按需的：目录随展开取一层，正文与变更只在真正要显示时才取，切栏不会重复取同一份。

use std::sync::Arc;

use astrcode_protocol::http::{
    FileContentResponseDto, GitStatusEntryDto, GitStatusEntryStateDto, GitStatusResponseDto,
};
use gpui_kit::{
    AnyElement, App, Context, EventEmitter, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    Window,
    component::{ActiveTheme as _, Size, h_flex, highlighter::HighlightTheme, v_flex},
    div, px,
};

use super::{icon_button, page_header};
use crate::{api::Api, icons::IconName};

mod changes;
mod diff;
mod editor;
mod tree;

use changes::{availability_note, entry_label, section_label};
use diff::{change_summary, render_diff, state_label};
use editor::{language_for_path, render_code};
use tree::{FileTree, TreeRow};

/// 文件树列的宽度。
const TREE_WIDTH: f32 = 240.0;
/// 每一层缩进的像素数。
const INDENT: f32 = 12.0;
/// 左列里变更清单的高度上限；超出的部分自己滚，剩下的高度留给文件树。
const CHANGES_MAX_HEIGHT: f32 = 240.0;

/// 代码页对外的事件。
#[derive(Debug, Clone)]
pub enum CodeViewEvent {
    /// 用户要求展开侧边栏。
    ToggleSidebar,
}

/// 右栏显示哪一面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    /// 文件正文。
    Content,
    /// 文件相对 HEAD 的改动。
    Diff,
}

/// 一份取回来的数据，或者取失败的原因。
enum Loaded<T> {
    Ready(T),
    Failed(String),
}

pub struct CodeView {
    api: Api,
    /// 浏览根目录（当前会话的工作目录）；空串表示还没有打开任何项目。
    root: String,
    tree: FileTree,
    /// 选中的文件，相对根目录的路径。
    selected: Option<String>,
    pane: Pane,
    content: Option<Loaded<FileContentResponseDto>>,
    diff: Option<Loaded<astrcode_protocol::http::FileDiffResponseDto>>,
    /// 工作区相对 HEAD 的改动清单；`None` 表示这个根目录还没取过。
    changes: Option<Loaded<GitStatusResponseDto>>,
    /// 侧边栏是否显示；收起后由页头给展开入口。
    sidebar_open: bool,
    /// 语法高亮主题；随产品主题固定，构造一次。
    highlight_theme: Arc<HighlightTheme>,
    /// 目录列举的任务；换一次句柄即取消上一次。
    tree_task: Option<Task<()>>,
    /// 正文与变更的任务。
    file_task: Option<Task<()>>,
    /// 变更清单的任务；换一次句柄即取消上一次。
    changes_task: Option<Task<()>>,
}

impl EventEmitter<CodeViewEvent> for CodeView {}

impl CodeView {
    pub fn new(api: Api, cx: &mut Context<Self>) -> Self {
        Self {
            api,
            root: String::new(),
            tree: FileTree::default(),
            selected: None,
            pane: Pane::Content,
            content: None,
            diff: None,
            changes: None,
            // 侧边栏一开始是显示的，页头因此不挂展开入口。
            sidebar_open: true,
            // 高亮调色板跟产品主题走（`theme::code_highlight_style` 装进去的那份），
            // 不用框架自带的深色主题：它与产品底色对不上。
            highlight_theme: cx.theme().highlight_theme.clone(),
            tree_task: None,
            file_task: None,
            changes_task: None,
        }
    }

    /// 换浏览根目录；根变了就丢掉旧树与旧选中的文件。
    pub fn set_root(&mut self, root: String, cx: &mut Context<Self>) {
        if self.root == root {
            return;
        }
        self.root = root;
        self.tree.clear();
        self.selected = None;
        self.content = None;
        self.diff = None;
        self.changes = None;
        self.fetch_dir(String::new(), cx);
        self.fetch_changes(cx);
        cx.notify();
    }

    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.sidebar_open = open;
        cx.notify();
    }

    /// 丢掉全部目录缓存再取一遍根目录与改动清单：agent 刚建出、刚改过的文件因此能出现。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.tree.clear();
        self.fetch_dir(String::new(), cx);
        self.fetch_changes(cx);
        self.reload_file(cx);
    }

    /// 取根目录下某个目录的一层条目。
    fn fetch_dir(&mut self, path: String, cx: &mut Context<Self>) {
        if self.root.is_empty() {
            return;
        }
        let api = self.api.clone();
        let root = self.root.clone();
        self.tree.begin_loading(&path);
        cx.notify();
        self.tree_task = Some(cx.spawn(async move |this, cx| {
            let result = api.file_tree(&root, &path).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(response) => this.tree.set_dir(&path, response.entries),
                    Err(error) => this.tree.set_error(&path, error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 展开/折叠一个目录；展开时按需取它这一层。
    fn toggle_dir(&mut self, path: String, cx: &mut Context<Self>) {
        let expanded = self.tree.toggle(&path);
        if expanded && !self.tree.is_loaded(&path) {
            self.fetch_dir(path, cx);
        }
        cx.notify();
    }

    /// 选中一个文件：清掉上一份内容，只取当前要显示的那一面。
    fn select(&mut self, path: String, cx: &mut Context<Self>) {
        self.selected = Some(path);
        self.content = None;
        self.diff = None;
        self.reload_file(cx);
        cx.notify();
    }

    fn set_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.pane == pane {
            return;
        }
        self.pane = pane;
        self.reload_file(cx);
        cx.notify();
    }

    /// 取当前要显示的那一面；已经取过的就不重复取。
    fn reload_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let needs_fetch = match self.pane {
            Pane::Content => self.content.is_none(),
            Pane::Diff => self.diff.is_none(),
        };
        if !needs_fetch {
            return;
        }

        let api = self.api.clone();
        let root = self.root.clone();
        let pane = self.pane;
        self.file_task = Some(cx.spawn(async move |this, cx| {
            let (content, diff) = match pane {
                Pane::Content => (Some(api.file_content(&root, &path).await), None),
                Pane::Diff => (None, Some(api.file_diff(&root, &path).await)),
            };
            this.update(cx, |this, cx| {
                if let Some(result) = content {
                    this.content = Some(to_loaded(result));
                }
                if let Some(result) = diff {
                    this.diff = Some(to_loaded(result));
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 取整个工作区相对 HEAD 的改动清单。
    ///
    /// 取回来之前不清旧清单：手动刷新时列表不该先闪成空的。换了根目录才会清空（见
    /// [`Self::set_root`]）。
    fn fetch_changes(&mut self, cx: &mut Context<Self>) {
        if self.root.is_empty() {
            return;
        }
        let api = self.api.clone();
        let root = self.root.clone();
        self.changes_task = Some(cx.spawn(async move |this, cx| {
            let result = api.worktree_status(&root).await;
            this.update(cx, |this, cx| {
                this.changes = Some(to_loaded(result));
                cx.notify();
            })
            .ok();
        }));
    }

    /// 点清单里的一项：切到「变更」栏看它改了什么。
    fn open_change(&mut self, path: String, cx: &mut Context<Self>) {
        self.pane = Pane::Diff;
        self.select(path, cx);
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = page_header(cx);
        if !self.sidebar_open {
            header = header.child(icon_button(
                "code-sidebar",
                IconName::Sidebar,
                "展开侧边栏",
                cx,
                |_, cx| cx.emit(CodeViewEvent::ToggleSidebar),
            ));
        }
        header = header
            .child(div().text_sm().child("代码"))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.root.clone()),
            )
            .child(div().flex_1())
            .child(self.render_pane_tab(Pane::Content, "正文", cx))
            .child(self.render_pane_tab(Pane::Diff, "变更", cx))
            .child(icon_button(
                "code-refresh",
                IconName::Refresh,
                "刷新",
                cx,
                |this, cx| this.refresh(cx),
            ));
        header.into_any_element()
    }

    fn render_pane_tab(
        &self,
        pane: Pane,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.pane == pane;
        let hover = cx.theme().list_hover;
        let mut tab = div()
            .id(SharedString::from(format!("code-pane-{label}")))
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .text_xs()
            .on_click(cx.listener(move |this, _, _, cx| this.set_pane(pane, cx)))
            .child(label.to_string());
        // 选中态与悬停态只画一个：两个都画会在悬停时把选中态盖掉。
        tab = if active {
            tab.bg(cx.theme().list_active)
                .text_color(cx.theme().foreground)
        } else {
            tab.text_color(cx.theme().muted_foreground)
                .hover(move |this| this.bg(hover))
        };
        tab.into_any_element()
    }

    /// 左列：变更清单在上、文件树在下，两侧同时可见。
    ///
    /// 清单自己限高，剩下的高度都给文件树；两处都能滚，谁高谁矮由内容决定。
    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut column = v_flex().w(px(TREE_WIDTH)).h_full().flex_shrink_0();
        // 还没打开项目时不摆这个区：左列这时只有「没有内容」本身。
        if !self.root.is_empty() {
            column = column.child(self.render_changes(cx));
        }
        column
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_tree(cx)),
            )
            .into_any_element()
    }

    /// 变更清单区：标题 + 条目，或者一句说明。
    fn render_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let ready = match &self.changes {
            Some(Loaded::Ready(status)) => Some(status),
            _ => None,
        };
        let section = v_flex()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(section_label(ready)),
            );
        let body = match &self.changes {
            None => change_note("正在取…", cx),
            Some(Loaded::Failed(message)) => change_note(message.clone(), cx),
            Some(Loaded::Ready(status)) => match availability_note(status.availability) {
                Some(note) => change_note(note, cx),
                None if status.entries.is_empty() => change_note("没有未提交的改动。", cx),
                None => self.render_change_list(status, cx),
            },
        };
        section.child(body).into_any_element()
    }

    /// 清单里的全部条目。
    ///
    /// 这一列只有 240px 宽，因此不分组也不排两列：状态标签在前、路径在后，超出的部分截断。
    fn render_change_list(
        &self,
        status: &GitStatusResponseDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = v_flex()
            // 滚动容器必须带 id：`overflow_y_scroll` 挂在有状态元素上。
            .id("code-changes-scroll")
            .w_full()
            .px_1()
            .pb_1()
            .max_h(px(CHANGES_MAX_HEIGHT))
            .overflow_y_scroll();
        for entry in &status.entries {
            list = list.child(self.render_change_row(entry, cx));
        }
        if status.truncated {
            list = list.child(change_note(
                format!("（只列出前 {} 条）", status.entries.len()),
                cx,
            ));
        }
        list.into_any_element()
    }

    fn render_change_row(&self, entry: &GitStatusEntryDto, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected.as_deref() == Some(entry.path.as_str());
        let hover = cx.theme().list_hover;
        let path = entry.path.clone();
        let mut row = h_flex()
            .id(SharedString::from(format!("code-change-{}", entry.path)))
            .items_center()
            .gap_1()
            .w_full()
            .min_h(px(24.0))
            .px_1()
            .rounded(cx.theme().radius)
            .text_xs()
            .on_click(cx.listener(move |this, _, _, cx| this.open_change(path.clone(), cx)))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(status_color(entry.state, cx))
                    .child(entry_label(entry.state)),
            )
            .child(div().min_w_0().truncate().child(entry.path.clone()));

        row = if selected {
            row.bg(cx.theme().list_active)
                .text_color(cx.theme().foreground)
        } else {
            row.text_color(cx.theme().foreground)
                .hover(move |this| this.bg(hover))
        };
        row.into_any_element()
    }

    /// 文件树本体；列宽与占用的高度由左列给，树自己纵向滚动。
    fn render_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut column = v_flex().w_full().p_2();
        for row in self.tree.visible_rows() {
            column = column.child(self.render_tree_row(row, cx));
        }
        div()
            // 滚动容器必须带 id：`overflow_y_scroll` 挂在有状态元素上。
            .id("code-tree-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(column)
            .into_any_element()
    }

    fn render_tree_row(&self, row: TreeRow, cx: &mut Context<Self>) -> AnyElement {
        // 取失败的那一行只是说明，没有可点的动作。
        if let Some(message) = row.error.clone() {
            return div()
                .w_full()
                .pl(px(INDENT * row.depth as f32 + INDENT))
                .pr_2()
                .py_1()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(message)
                .into_any_element();
        }

        let selected = self.selected.as_deref() == Some(row.path.as_str());
        let hover = cx.theme().list_hover;
        let path = row.path.clone();
        let mut item = h_flex()
            .id(SharedString::from(format!("code-tree-{}", row.path)))
            .items_center()
            .gap_1()
            .w_full()
            .min_h(px(24.0))
            .pl(px(INDENT * row.depth as f32 + 4.0))
            .pr_1()
            .rounded(cx.theme().radius)
            .text_xs()
            .on_click(cx.listener(move |this, _, _, cx| {
                if row.is_dir {
                    this.toggle_dir(path.clone(), cx);
                } else {
                    this.select(path.clone(), cx);
                }
            }));

        item = if selected {
            item.bg(cx.theme().list_active)
                .text_color(cx.theme().foreground)
        } else {
            item.text_color(cx.theme().foreground)
                .hover(move |this| this.bg(hover))
        };

        // 目录的箭头：展开时朝下，折叠时朝右。
        if row.is_dir {
            item = item.child(
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(IconName::ChevronRight.element(Size::Small)),
            );
        }
        item.child(if row.is_dir {
            IconName::Folder.element(Size::Small)
        } else {
            IconName::Edit.element(Size::Small)
        })
        .child(div().min_w_0().truncate().child(row.name))
        .into_any_element()
    }

    fn render_pane(&self, window: &Window, cx: &App) -> AnyElement {
        if self.root.is_empty() {
            return placeholder("还没有打开项目：先新建或选一个会话。", cx);
        }
        let Some(path) = self.selected.as_deref() else {
            return placeholder("从左侧选一个文件。", cx);
        };

        let (meta, body) = match self.pane {
            Pane::Content => match &self.content {
                None => return placeholder("正在读取…", cx),
                Some(Loaded::Failed(message)) => return placeholder(message, cx),
                Some(Loaded::Ready(content)) => (
                    content_meta(content),
                    render_content(content, &self.highlight_theme, window, cx),
                ),
            },
            Pane::Diff => match &self.diff {
                None => return placeholder("正在读取…", cx),
                Some(Loaded::Failed(message)) => return placeholder(message, cx),
                Some(Loaded::Ready(diff)) => (
                    format!("{} · {}", state_label(diff.state), change_summary(diff)),
                    render_diff(diff, cx),
                ),
            },
        };

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().min_w_0().truncate().text_xs().child(path.to_owned()))
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(meta),
                    ),
            )
            // 正文与 diff 都不换行，横向也放不下：两个方向都在这里滚。
            .child(
                div()
                    .id("code-pane-scroll")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_scroll()
                    .child(body),
            )
            .into_any_element()
    }
}

impl Render for CodeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let sidebar = self.render_sidebar(cx);
        let pane = self.render_pane(window, cx);
        v_flex().size_full().child(header).child(
            h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(sidebar)
                    // 分隔线自绘：只有 1px，用边框会把相邻两列画成两条线。
                    .child(div().w(px(1.0)).h_full().flex_shrink_0().bg(cx.theme().border))
                    .child(div().flex_1().min_w_0().h_full().child(pane)),
        )
    }
}

/// 正文栏的元信息：大小、行数，以及是否被截断。
fn content_meta(content: &FileContentResponseDto) -> String {
    let size = if content.size_bytes < 1024 {
        format!("{} B", content.size_bytes)
    } else {
        format!("{:.1} KB", content.size_bytes as f64 / 1024.0)
    };
    if content.binary {
        return size;
    }
    let truncated = if content.truncated {
        " · 已截断"
    } else {
        ""
    };
    format!("{size} · {} 行{truncated}", content.total_lines)
}

fn render_content(
    content: &FileContentResponseDto,
    theme: &HighlightTheme,
    window: &Window,
    cx: &App,
) -> AnyElement {
    if content.binary {
        return placeholder("二进制文件，无法按代码展示。", cx);
    }
    let language = language_for_path(&content.path);
    render_code(&content.text, language, theme, window, cx)
}

fn placeholder(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .p_4()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
        .into_any_element()
}

/// 变更区里的一句说明（取不到、没改动、被截断）。
fn change_note(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .px_2()
        .pb_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
        .into_any_element()
}

/// 变更条目的状态色：新增/未跟踪算「加」，删除/冲突算「减」，其余算「改」。
fn status_color(state: GitStatusEntryStateDto, cx: &App) -> Hsla {
    match state {
        GitStatusEntryStateDto::Added | GitStatusEntryStateDto::Untracked => cx.theme().success,
        GitStatusEntryStateDto::Deleted | GitStatusEntryStateDto::Conflicted => cx.theme().danger,
        GitStatusEntryStateDto::Renamed => cx.theme().primary,
        GitStatusEntryStateDto::Modified => cx.theme().warning,
    }
}

fn to_loaded<T>(result: Result<T, crate::api::ApiError>) -> Loaded<T> {
    match result {
        Ok(value) => Loaded::Ready(value),
        Err(error) => Loaded::Failed(error.to_string()),
    }
}
