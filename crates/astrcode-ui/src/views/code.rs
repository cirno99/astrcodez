//! 代码页：左侧文件树 + 右侧「正文 / 变更」两栏。
//!
//! 数据全部走 server 的 `/api/files/*`：`astrcode-ui` 是宿主无关层，自己读不了磁盘，两个宿主
//! 因此共用这一页（ADR 0001）。浏览根目录由外壳注入（当前会话的工作目录）。
//!
//! 取用是按需的：目录随展开取一层，选中的文件则把正文与 diff 一起取——正文栏要按 diff 画
//! 变更标记，两面因此不再各取各的。

use std::{ops::Range, sync::Arc, time::Duration};

use astrcode_protocol::http::{
    FileContentResponseDto, FileSearchMatchDto, FileSearchResponseDto, GitStatusEntryDto,
    GitStatusEntryStateDto, GitStatusResponseDto,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, HighlightStyle,
    Hsla, InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    Window,
    component::{
        ActiveTheme as _, Size, h_flex,
        highlighter::HighlightTheme,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    div, point, px,
};

use super::{icon_button, page_header};
use crate::{
    api::Api,
    find::{self, Find, Match},
    icons::IconName,
};

mod annotate;
mod changes;
mod diff;
mod editor;
mod selectable;
mod tree;

use annotate::{LineChange, line_changes};
use changes::{availability_note, entry_label, section_label};
use diff::{
    AlignedDiff, aligned_diff, change_summary, render_aligned, render_diff, state_label, state_note,
};
use editor::{LineLayers, language_for_path, pane_scroll, render_code, visible_rows};
use tree::{FileTree, TreeRow};

/// 文件树列的宽度。
const TREE_WIDTH: f32 = 240.0;
/// 每一层缩进的像素数。
const INDENT: f32 = 12.0;
/// 左列里变更清单的高度上限；超出的部分自己滚，剩下的高度留给文件树。
const CHANGES_MAX_HEIGHT: f32 = 240.0;
/// 全局搜索的防抖时长。
///
/// 每敲一个字都走一趟服务端（那是一次全树遍历），因此等于停下来再发。
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
/// 跳转时往目标行上方留出的行数：紧贴顶边的话，上下文的半行都看不到。
const JUMP_LEAD_LINES: f32 = 2.0;

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

/// 一份取回来的正文，连同只为它算一次的两份派生数据。
///
/// 行区间与整份高亮都是全量的（大文件上跑一遍 tree-sitter 是几十毫秒的量级），放进渲染路径就会
/// 每帧重算；跟着正文一起存，正文换了才重算，也不会出现「正文换了、高亮还是上一份」的中间状态。
struct FileBody {
    /// 服务端返回的正文。
    dto: FileContentResponseDto,
    /// 每一行的字节区间（见 [`editor::line_ranges`]）。
    lines: Vec<Range<usize>>,
    /// 整份正文的高亮样式（见 [`editor::highlight`]）。
    highlight: Vec<(Range<usize>, HighlightStyle)>,
}

impl FileBody {
    /// 正文一到手就算好行区间与整份高亮；二进制不按代码展示，也就不必跑语法分析。
    fn new(dto: FileContentResponseDto, theme: &HighlightTheme) -> Self {
        let (lines, highlight) = if dto.binary {
            (Vec::new(), Vec::new())
        } else {
            let lines = editor::line_ranges(&dto.text);
            let highlight = editor::highlight(&dto.text, language_for_path(&dto.path), theme);
            (lines, highlight)
        };
        Self {
            dto,
            lines,
            highlight,
        }
    }
}

pub struct CodeView {
    api: Api,
    /// 浏览根目录（当前会话的工作目录）；空串表示还没有打开任何项目。
    root: String,
    tree: FileTree,
    /// 选中的文件，相对根目录的路径。
    selected: Option<String>,
    pane: Pane,
    content: Option<Loaded<FileBody>>,
    /// 正文栏的滚动位置：虚拟化要知道当前滚到哪一段。两栏共用这一个滚动容器。
    content_scroll: ScrollHandle,
    /// 正文栏的焦点：Ctrl+C 要有焦点路径才走得到窗口根节点的复制处理（见
    /// [`editor::pane_scroll`]）。
    content_focus: FocusHandle,
    diff: Option<Loaded<astrcode_protocol::http::FileDiffResponseDto>>,
    /// 工作区相对 HEAD 的改动清单；`None` 表示这个根目录还没取过。
    changes: Option<Loaded<GitStatusResponseDto>>,
    /// 侧边栏是否显示；收起后由页头给展开入口。
    sidebar_open: bool,
    /// 正文每行的变更标记，下标与正文行序一致；正文或 diff 换一份就重算。
    line_marks: Vec<Option<LineChange>>,
    /// 「变更」栏的双栏对齐行；`None` 表示回落统一 diff（没算过、有截断，或不在这一栏）。
    aligned: Option<AlignedDiff>,
    /// 语法高亮主题；随产品主题固定，构造一次。
    highlight_theme: Arc<HighlightTheme>,
    /// 目录列举的任务；换一次句柄即取消上一次。
    tree_task: Option<Task<()>>,
    /// 正文与变更的任务。
    file_task: Option<Task<()>>,
    /// 变更清单的任务；换一次句柄即取消上一次。
    changes_task: Option<Task<()>>,
    /// 全局搜索的查询框。
    search_query: Entity<InputState>,
    /// 全局搜索是否区分大小写。
    search_case_sensitive: bool,
    /// 最近一次搜索的结果；`None` 表示还没有结果可摆。
    search: Option<Loaded<FileSearchResponseDto>>,
    /// 搜索任务：换一次输入即换掉它，上一次的请求随之取消——防抖由此而来，不必另写定时器。
    search_task: Option<Task<()>>,
    /// 当前文件内的查找（Ctrl/Cmd+F 那条查找栏）。
    find: Find,
    /// 查找栏是否显示。
    find_open: bool,
    /// 当前文件里的命中区间，下标与 [`Find::current`] 对应。
    find_matches: Vec<Match>,
    /// 命中处的高亮（当前那一处用强调色）；与语法高亮一起交给正文渲染。
    find_highlights: Vec<(Range<usize>, HighlightStyle)>,
    /// 搜索结果点过来的那一行：正文到手后当前命中落到它上面。
    pending_find_line: Option<usize>,
    /// 待滚动到的行：行高只有在窗口里才知道，因此记行号、把滚动放到渲染入口。
    pending_scroll: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CodeViewEvent> for CodeView {}

impl CodeView {
    pub fn new(api: Api, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 左列顶上的搜索框：单行；搜索按输入防抖进行，回车不做别的事。
        let search_query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索代码…"));
        // 正文栏的查找框：与全局搜索分开——一个搜整个工作区，一个只搜眼前这份文件。
        let find_query = cx.new(|cx| InputState::new(window, cx).placeholder("在当前文件里查找…"));
        let view = cx.weak_entity();
        let subscriptions = vec![
            // 查询串一变就重搜；空串在 [`Self::start_search`] 里处理，它连请求都不发。
            cx.subscribe_in(
                &search_query,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.start_search(cx);
                    }
                },
            ),
            cx.subscribe_in(&find_query, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refresh_find(cx);
                }
            }),
            // Ctrl/Cmd+F 开查找栏、Esc 收起它。走应用级拦截而不是挂在正文栏上：焦点在查询
            // 框里时那一次按键也要认得出来。两个动作都要求正文栏持有焦点，因此这一页没显示
            // 时不会被误触——那时焦点不在它身上。
            cx.intercept_keystrokes(move |event, window, cx| {
                let handled = view
                    .update(cx, |this, cx| {
                        this.handle_shortcut(&event.keystroke, window, cx)
                    })
                    .unwrap_or(false);
                if handled {
                    cx.stop_propagation();
                }
            }),
        ];
        Self {
            api,
            root: String::new(),
            tree: FileTree::default(),
            selected: None,
            pane: Pane::Content,
            content: None,
            content_scroll: ScrollHandle::new(),
            content_focus: cx.focus_handle(),
            diff: None,
            changes: None,
            // 侧边栏一开始是显示的，页头因此不挂展开入口。
            sidebar_open: true,
            line_marks: Vec::new(),
            aligned: None,
            // 高亮调色板跟产品主题走（`theme::code_highlight_style` 装进去的那份），
            // 不用框架自带的深色主题：它与产品底色对不上。
            highlight_theme: cx.theme().highlight_theme.clone(),
            tree_task: None,
            file_task: None,
            changes_task: None,
            search_query,
            search_case_sensitive: false,
            search: None,
            search_task: None,
            find: Find::new(find_query),
            find_open: false,
            find_matches: Vec::new(),
            find_highlights: Vec::new(),
            pending_find_line: None,
            pending_scroll: None,
            _subscriptions: subscriptions,
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
        // 上一个项目里的行号与对齐行都不属于新根目录。
        self.line_marks.clear();
        self.aligned = None;
        self.changes = None;
        self.fetch_dir(String::new(), cx);
        self.fetch_changes(cx);
        // 搜索结果属于旧的根目录、命中的行属于旧的正文，两者一起作废；查询串留着，
        // 换一个项目就用它重搜一次，否则左列会停在「正在搜索…」上。
        self.search = None;
        self.search_task = None;
        self.find_open = false;
        self.clear_find_matches();
        self.start_search(cx);
        cx.notify();
    }

    pub fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.sidebar_open = open;
        cx.notify();
    }

    /// 丢掉全部目录缓存再取一遍根目录与改动清单：agent 刚建出、刚改过的文件因此能出现。
    ///
    /// 当前文件的正文与 diff 也一起丢掉重取：不丢的话 [`Self::reload_file`] 会因为「已经取过」
    /// 直接返回，正文与变更标记会停在刷新前那一份。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.tree.clear();
        self.fetch_dir(String::new(), cx);
        self.fetch_changes(cx);
        self.content = None;
        self.diff = None;
        self.line_marks.clear();
        self.aligned = None;
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
        self.line_marks.clear();
        self.aligned = None;
        self.reload_file(cx);
        cx.notify();
    }

    fn set_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.pane == pane {
            return;
        }
        self.pane = pane;
        self.reload_file(cx);
        // 切到「变更」栏才值得算双栏对齐（见 [`Self::refresh_aligned`]）。
        self.refresh_aligned();
        cx.notify();
    }

    /// 取当前选中的文件；正文与 diff 一起取，两份都到手才重算标记。
    ///
    /// 已经取过的那份不重复取：切栏与刷新都不会白跑一次请求。
    fn reload_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        if self.content.is_some() && self.diff.is_some() {
            return;
        }

        let api = self.api.clone();
        let root = self.root.clone();
        let theme = self.highlight_theme.clone();
        self.file_task = Some(cx.spawn(async move |this, cx| {
            let (content, diff) =
                futures_util::join!(api.file_content(&root, &path), api.file_diff(&root, &path));
            // 整份高亮只在正文到手时算一次，且放在后台线程：几十毫秒的语法分析不该压在界面线程上。
            let content = content.map(|dto| FileBody::new(dto, &theme));
            this.update(cx, |this, cx| {
                this.content = Some(to_loaded(content));
                this.diff = Some(to_loaded(diff));
                this.refresh_marks();
                this.refresh_aligned();
                // 正文换了，命中跟着换：从搜索结果跳过来的那一行也在这时候落位。
                this.refresh_find(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// 重算正文的变更标记：行序由正文定，每行的状态由 diff 定。
    ///
    /// 少一份就给空表：没有标记的正文照常显示，比留一份对不上行的旧标记好。
    fn refresh_marks(&mut self) {
        self.line_marks.clear();
        let (Some(Loaded::Ready(content)), Some(Loaded::Ready(diff))) = (&self.content, &self.diff)
        else {
            return;
        };
        self.line_marks = line_changes(&diff.unified_diff, content.lines.len());
    }

    /// 重算双栏的对齐行。
    ///
    /// 只在「变更」栏可见时算：整份比加两份语法分析在大文件上是几十毫秒的量级，而这份数据
    /// 只被那一栏用。下面这些情形一律留空、回落统一 diff：不在这一栏、有任一侧被截断（两边
    /// 各自截在不同的位置，硬对齐会在边界处凭空多出一行替换）、这一份根本没有 diff 正文、
    /// 以及原来是二进制。
    fn refresh_aligned(&mut self) {
        self.aligned = None;
        if self.pane != Pane::Diff {
            return;
        }
        let (Some(Loaded::Ready(content)), Some(Loaded::Ready(diff))) = (&self.content, &self.diff)
        else {
            return;
        };
        if content.dto.binary || content.dto.truncated || diff.original_truncated {
            return;
        }
        if state_note(diff.state).is_some() {
            return;
        }
        let aligned = aligned_diff(
            &diff.original,
            &content.dto.text,
            language_for_path(&content.dto.path),
            &self.highlight_theme,
        );
        self.aligned = Some(aligned);
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

    /// 全局搜索的查询串；首尾空白不算内容。
    fn search_needle(&self, cx: &App) -> String {
        self.search_query.read(cx).value().trim().to_owned()
    }

    /// 搜索框里还有查询串吗；左列据此决定摆文件树还是摆结果。
    fn searching(&self, cx: &App) -> bool {
        !self.search_needle(cx).is_empty()
    }

    /// 查询串变了：起一趟防抖后的搜索。
    ///
    /// 防抖靠「换掉上一次的任务」：旧任务被取消，只有最后一次输入会真的发出请求。空查询串
    /// 连请求都不发——服务端按「没有命中」看待它，跑一趟没有意义。
    fn start_search(&mut self, cx: &mut Context<Self>) {
        let needle = self.search_needle(cx);
        if needle.is_empty() || self.root.is_empty() {
            self.search = None;
            self.search_task = None;
            cx.notify();
            return;
        }
        let api = self.api.clone();
        let root = self.root.clone();
        let case_sensitive = self.search_case_sensitive;
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let result = api.file_search(&root, &needle, case_sensitive).await;
            this.update(cx, |this, cx| {
                this.search = Some(to_loaded(result));
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// 切换全局搜索的大小写口径；换一次口径要重搜才有新结果。
    fn toggle_search_case(&mut self, cx: &mut Context<Self>) {
        self.search_case_sensitive = !self.search_case_sensitive;
        self.start_search(cx);
    }

    /// 点搜索结果里的一处：打开那份文件，把正文栏的查找对准同一处，再滚到那一行。
    ///
    /// 查找框填的是同一个查询串：跳过去之后还能用上一处/下一处在这份文件里翻。
    fn open_search_result(
        &mut self,
        path: String,
        line: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needle = self.search_needle(cx);
        let case_sensitive = self.search_case_sensitive;
        self.find.seed(&needle, case_sensitive, window, cx);
        self.find_open = true;
        self.pane = Pane::Content;
        // 正文是现取的：先记下要落到哪一行，等正文到手那一次 [`Self::refresh_find`] 才谈得上定位。
        self.select(path, cx);
        self.pending_find_line = Some(line);
        cx.notify();
    }

    /// 代码页认下的按键：Ctrl/Cmd+F 开查找栏，Esc 收起它。
    fn handle_shortcut(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let opens_find = keystroke.key.eq_ignore_ascii_case("f")
            && (keystroke.modifiers.control || keystroke.modifiers.platform);
        if opens_find {
            // 正文栏拿着焦点才认：这一页没显示时焦点不会落在它身上，别的页面因此不会误触。
            if !self.content_focus.is_focused(window) {
                return false;
            }
            self.open_find(window, cx);
            return true;
        }
        if keystroke.key == "escape" && self.find_open {
            self.close_find(window, cx);
            return true;
        }
        false
    }

    /// 打开查找栏：焦点交给查询框，并把上一次的查询串重新算一遍。
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = true;
        self.find.focus(window, cx);
        self.refresh_find(cx);
        cx.notify();
    }

    /// 收起查找栏并丢掉命中：没有查找栏时正文上不该留着高亮。
    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.clear(window, cx);
        self.find_open = false;
        self.clear_find_matches();
        cx.notify();
    }

    /// 丢掉命中与高亮。
    fn clear_find_matches(&mut self) {
        self.find_matches.clear();
        self.find_highlights.clear();
        self.pending_find_line = None;
    }

    fn toggle_find_case(&mut self, cx: &mut Context<Self>) {
        self.find.toggle_case();
        self.refresh_find(cx);
        cx.notify();
    }

    /// 跳到下一处/上一处。
    fn step_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.find.advance(forward);
        self.refresh_find_highlights(cx);
        self.pending_scroll = self
            .find_matches
            .get(self.find.current())
            .map(|range| self.line_of(range.start));
        cx.notify();
    }

    /// 重算当前文件里的命中、高亮与落点。
    ///
    /// 查询串、大小写、正文、跳转目标——任何一样变了都从这里过一遍：命中与高亮是同一份推导，
    /// 分成两条路会走岔。
    fn refresh_find(&mut self, cx: &App) {
        let needle = self.find.needle(cx);
        let case_sensitive = self.find.case_sensitive();
        let mut matches = Vec::new();
        if !needle.is_empty()
            && let Some(Loaded::Ready(content)) = &self.content
        {
            matches = find::literal_matches(&content.dto.text, &needle, case_sensitive);
        }
        self.find.set_count(matches.len());
        // 从搜索结果跳过来的那一处优先：它说「我点的是这一行」。
        if let Some(line) = self.pending_find_line.take() {
            let on_line = match &self.content {
                Some(Loaded::Ready(content)) => matches
                    .iter()
                    .position(|range| line_of(&content.dto.text, range.start) == line),
                _ => None,
            };
            self.find.set_current(on_line.unwrap_or(0));
        }
        self.find_matches = matches;
        self.refresh_find_highlights(cx);
        self.pending_scroll = self
            .find_matches
            .get(self.find.current())
            .map(|range| self.line_of(range.start));
    }

    /// 按当前那一处重算高亮：命中本身已经在 [`Self::find_matches`] 里了。
    fn refresh_find_highlights(&mut self, cx: &App) {
        self.find_highlights = self
            .find_matches
            .iter()
            .enumerate()
            .map(|(index, range)| {
                (
                    range.clone(),
                    find::match_style(index == self.find.current(), cx),
                )
            })
            .collect();
    }

    /// 正文里的字节偏移换成行号（0 起）：数一遍它前面有几个换行。
    fn line_of(&self, offset: usize) -> usize {
        match &self.content {
            Some(Loaded::Ready(content)) => line_of(&content.dto.text, offset),
            _ => 0,
        }
    }
    /// 把记下的那一行滚进视野。
    ///
    /// 行高由窗口给，而正文到手、切栏、跳转这几处都没有窗口，因此这里只记行号，真正的滚动
    /// 放在渲染入口（见 [`Self::apply_pending_scroll`]）。
    fn apply_pending_scroll(&mut self, window: &Window) {
        let Some(line) = self.pending_scroll.take() else {
            return;
        };
        let line_height = f32::from(window.line_height());
        let lead = (line as f32 - JUMP_LEAD_LINES).max(0.0);
        self.content_scroll
            .set_offset(point(px(0.), px(-(lead * line_height))));
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
            .child(div().text_sm().child("文件"))
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
        if self.root.is_empty() {
            return column
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(self.render_tree(cx)),
                )
                .into_any_element();
        }

        column = column.child(self.render_search_input(cx));
        // 有查询串时整列只讲搜索结果：那一刻文件树与改动清单都不是要看的东西，而这 240px 宽
        // 的一列也容不下两份东西。
        if self.searching(cx) {
            return column
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(self.render_search_results(cx)),
                )
                .into_any_element();
        }
        column
            .child(self.render_changes(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_tree(cx)),
            )
            .into_any_element()
    }

    /// 左列顶上的搜索框：全局搜索的入口，结果就排在它下面。
    fn render_search_input(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search_query)),
            )
            .child(find::render_case_toggle(
                "code-search-case",
                self.search_case_sensitive,
                |this: &mut CodeView, _, cx| this.toggle_search_case(cx),
                cx,
            ))
            .into_any_element()
    }

    /// 搜索结果：按文件分组，点一处即跳到那一行。
    fn render_search_results(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.search {
            None => placeholder("正在搜索…", cx),
            Some(Loaded::Failed(message)) => placeholder(message.clone(), cx),
            Some(Loaded::Ready(response)) if response.files.is_empty() => {
                placeholder("没有匹配。", cx)
            },
            Some(Loaded::Ready(response)) => {
                let needle_len = self.search_needle(cx).len();
                let mut list = v_flex().w_full().p_1();
                for file in &response.files {
                    list = list.child(search_file_row(&file.path, file.matches.len(), cx));
                    for item in &file.matches {
                        list =
                            list.child(self.render_search_match(&file.path, item, needle_len, cx));
                    }
                }
                if response.truncated {
                    list = list.child(change_note("（只列出前面这些）".to_owned(), cx));
                }
                div()
                    // 滚动容器必须带 id：`overflow_y_scroll` 挂在有状态元素上。
                    .id("code-search-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .child(list)
                    .into_any_element()
            },
        }
    }

    /// 一条命中：行号 + 摘录，命中那一段单独上色。
    fn render_search_match(
        &self,
        path: &str,
        item: &FileSearchMatchDto,
        needle_len: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = item.text.as_str();
        let start = clamp_to_char_boundary(text, item.column);
        let end = clamp_to_char_boundary(text, start + needle_len);
        let hover = cx.theme().list_hover;
        let open = path.to_owned();
        let line = item.line;

        h_flex()
            .id(SharedString::from(format!(
                "code-search-{path}-{}",
                item.line
            )))
            .items_center()
            .gap_2()
            .w_full()
            .min_w_0()
            .pl(px(6.0))
            .pr_1()
            .py_0p5()
            .rounded(cx.theme().radius)
            .text_xs()
            .hover(move |this| this.bg(hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_search_result(open.clone(), line, window, cx);
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(item.line.to_string()),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(text[..start].to_owned())
                    .child(
                        div()
                            .bg(cx.theme().accent)
                            .text_color(cx.theme().foreground)
                            .child(text[start..end].to_owned()),
                    )
                    .child(text[end..].to_owned()),
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

    /// 正文栏与变更栏当前该渲染的行区间：由滚动位置与视口高度算出（见 [`visible_rows`]）。
    ///
    /// 视口高度取自滚动容器；容器还没量过时（这一页第一次画之前）退回整窗高度——宁可多画几行，
    /// 也不要因为拿到 0 而只画几行、露出空白。两栏共用同一个滚动容器与同一条按行高换算的口径，
    /// 因此只有总行数不同。
    fn pane_rows(&self, total: usize, window: &Window) -> Range<usize> {
        let bounds = self.content_scroll.bounds();
        let viewport = if bounds.size.height > px(0.0) {
            bounds.size.height
        } else {
            window.viewport_size().height
        };
        // 框架的滚动偏移原样交给 [`visible_rows`]：它的向下为负的约定在那边处理。
        visible_rows(
            f32::from(self.content_scroll.offset().y),
            f32::from(viewport),
            f32::from(window.line_height()),
            total,
        )
    }

    fn render_pane(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        // 正文到手、切栏、跳转这几处都没有窗口，滚动这一步因此补在这里。
        self.apply_pending_scroll(window);
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
                    content_meta(&content.dto),
                    render_content(
                        content,
                        &self.line_marks,
                        &self.find_highlights,
                        self.pane_rows(content.lines.len(), window),
                        window,
                        cx,
                    ),
                ),
            },
            Pane::Diff => match &self.diff {
                None => return placeholder("正在读取…", cx),
                Some(Loaded::Failed(message)) => return placeholder(message, cx),
                Some(Loaded::Ready(diff)) => (
                    format!("{} · {}", state_label(diff.state), change_summary(diff)),
                    match &self.aligned {
                        Some(aligned) => render_aligned(
                            aligned,
                            self.pane_rows(aligned.rows.len(), window),
                            window,
                            cx,
                        ),
                        None => render_diff(diff, cx),
                    },
                ),
            },
        };

        let mut pane = v_flex().size_full().child(
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
        );
        // 查找栏只属于正文栏：变更栏画的是双栏对齐的格子，高亮无从谈起。
        if self.find_open && self.pane == Pane::Content {
            pane = pane.child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(find::render_find_bar(
                        &self.find,
                        find_actions(),
                        "关闭查找",
                        cx,
                    )),
            );
        }
        // 正文与 diff 都不换行，横向也放不下：两个方向都在这里滚。
        // 追踪滚动位置是为了算正文该渲染哪几行（见 [`Self::content_rows`]）。
        pane.child(pane_scroll(&self.content_focus, &self.content_scroll, body))
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

/// 正文里的字节偏移换成行号（0 起）：数一遍它前面有几个换行。
///
/// 只有跳转与重算用得上它，因此不值得为它留一份行号表；一份正文也就扫一遍。
fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count()
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
    content: &FileBody,
    marks: &[Option<LineChange>],
    find: &[(Range<usize>, HighlightStyle)],
    rows: Range<usize>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    if content.dto.binary {
        return placeholder("二进制文件，无法按代码展示。", cx);
    }
    render_code(
        &content.dto.text,
        &content.lines,
        LineLayers {
            syntax: &content.highlight,
            find,
        },
        marks,
        rows,
        window,
        cx,
    )
}

/// 搜索结果里一个文件的分组头：路径 + 命中数。
fn search_file_row(path: &str, count: usize, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap_2()
        .px_2()
        .pt_2()
        .pb_1()
        .text_xs()
        .text_color(cx.theme().foreground)
        .child(div().min_w_0().truncate().child(path.to_owned()))
        .child(
            div()
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .child(count.to_string()),
        )
        .into_any_element()
}

/// 把跨了线缆来的字节偏移夹进正文范围内，并收到字符边界上。
///
/// `column` 由服务端算好；它是跨了一次 HTTP 的值，坏掉时宁可少画一格，也不要让切片 panic。
fn clamp_to_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 正文栏查找栏的几个动作，指到 [`CodeView`] 上的方法。
fn find_actions() -> find::FindActions<CodeView> {
    find::FindActions {
        toggle_case: |this, _, cx| this.toggle_find_case(cx),
        prev: |this, _, cx| this.step_find(false, cx),
        next: |this, _, cx| this.step_find(true, cx),
        close: |this, window, cx| this.close_find(window, cx),
    }
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
