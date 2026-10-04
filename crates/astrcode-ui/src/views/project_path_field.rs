//! 工作目录输入：输入框、历史候选与文件夹选择器。
//!
//! 前端这件事由两个组件承担（`Kanban/ProjectPathField.tsx` + `Kanban/ProjectFolderPicker.tsx`）；
//! 桌面端有两个弹窗要用它（新建项目、新建卡片），所以单独成一个实体——候选列表与目录列举
//! 只能有一份实现，抄第二份就会有两处需要同时改的逻辑。
//!
//! 组件只管「路径填到哪儿」：删候选发 [`ProjectPathFieldEvent::ForgetPath`]，落盘在外壳
//! （偏好文件是整份替换，只能有一个写者）；提交动作留给各自的弹窗。

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    component::{
        ActiveTheme as _, Disableable as _, Size,
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    deferred, div, px,
};

use crate::{
    api::{Api, ApiError},
    icons::IconName,
    kanban::{DirectoryListing, forget_project_path, merge_project_path_candidates},
    views::icon_button,
};

/// 候选面板的高度上限，与前端 `max-h-[240px]` 同值。
const CANDIDATE_PANEL_MAX_HEIGHT: f32 = 240.0;
/// 文件夹列表的高度，与前端 `h-[280px]` 同值。
const PICKER_LIST_HEIGHT: f32 = 280.0;

/// 提交用的工作目录：去掉首尾空白后的值；纯空白表示还不能提交。
///
/// 提交判据与提交载荷共用它——「按钮亮不亮」和「实际建在哪个目录」必须是同一个判断。
pub(crate) fn submittable_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// 组件对外的事件。提交与落盘都不在这里做。
#[derive(Debug, Clone)]
pub(crate) enum ProjectPathFieldEvent {
    /// 用户从候选里删掉一条路径：清历史并记进忽略集。
    ForgetPath(String),
    /// 文件夹选择器开合：父弹窗据此换卡面宽度，并决定 Esc 与点遮罩要不要关弹窗。
    PickerToggled,
}

/// 文件夹选择器：一层目录的列举状态。
struct Picker {
    /// 正在列举的目录。列举失败时列表为空，靠它告诉用户刚才找的是哪儿。
    requested_path: String,
    listing: Option<DirectoryListing>,
    loading: bool,
    error: Option<String>,
}

pub(crate) struct ProjectPathField {
    api: Api,
    /// 元素 id 前缀。两个弹窗各带一份，id 不靠「反正不会同时出现」来保证唯一。
    id_prefix: &'static str,
    /// 路径为空时的默认值，也是选择器的起始目录。
    default_working_dir: String,
    /// 历史之外的候选来源（会话列表里的工作目录）。
    extra_candidates: Vec<String>,
    /// 路径历史与忽略集在本组件里的那一份；删候选后不必等落盘回来才更新显示。
    history: Vec<String>,
    ignored: Vec<String>,
    path: Entity<InputState>,
    /// 候选面板是否展开。
    candidates_open: bool,
    /// 输入与按钮是否可用；提交在飞时由父弹窗置否（前端 `disabled={loading}`）。
    enabled: bool,
    /// `None` 表示正在填表单，`Some` 表示文件夹选择器开着。
    picker: Option<Picker>,
    /// 目录列举任务；换掉句柄即取消上一次。
    list_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ProjectPathFieldEvent> for ProjectPathField {}

impl ProjectPathField {
    // 构造参数逐项对应字段（API、id 前缀、默认目录、候选/历史/忽略列表与窗口上下文），
    // 收敛成结构体会让调用点更啰嗦。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        api: Api,
        id_prefix: &'static str,
        default_working_dir: String,
        extra_candidates: Vec<String>,
        history: Vec<String>,
        ignored: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = if default_working_dir.is_empty() {
            "项目路径".to_string()
        } else {
            format!("默认：{default_working_dir}")
        };
        let path = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions =
            vec![
                cx.subscribe_in(&path, window, |this, _, event: &InputEvent, _, cx| {
                    match event {
                        // 点进输入框或开始输入都展开候选（前端 `onFocus` / `onChange` 同口径）。
                        InputEvent::Focus | InputEvent::Change => {
                            this.set_candidates_open(true, cx)
                        },
                        // 焦点移走就收起：候选行是普通元素，点它不会让输入框失焦，所以这条只管
                        // 「点到别的控件上」那一类。
                        InputEvent::Blur => this.set_candidates_open(false, cx),
                        _ => {},
                    }
                }),
            ];
        Self {
            api,
            id_prefix,
            default_working_dir,
            extra_candidates,
            history,
            ignored,
            path,
            candidates_open: false,
            enabled: true,
            picker: None,
            list_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// 输入框里此刻的值，未去空白。
    pub(crate) fn value(&self, cx: &App) -> String {
        self.path.read(cx).value().to_string()
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        cx.notify();
    }

    /// 文件夹选择器是否开着。
    pub(crate) fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    /// Esc 的第一道：候选面板开着时只收面板。
    ///
    /// 返回真表示这次按键已被消化。面板算内层浮层，前端 `Dropdown` 的 Escape 同样
    /// `stopPropagation`，免得一下 Esc 关掉两层。
    pub(crate) fn dismiss_layers(&mut self, cx: &mut Context<Self>) -> bool {
        if self.candidates_open {
            self.set_candidates_open(false, cx);
            return true;
        }
        false
    }

    /// 此刻的候选：历史优先，其后是默认目录与会话目录，剔除已忽略的。
    fn candidates(&self) -> Vec<String> {
        let mut sources = vec![self.default_working_dir.as_str()];
        sources.extend(self.extra_candidates.iter().map(String::as_str));
        merge_project_path_candidates(&self.history, &sources, &self.ignored)
    }

    fn set_candidates_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.candidates_open == open {
            return;
        }
        self.candidates_open = open;
        cx.notify();
    }

    /// 选中一条候选：写回输入框并收起面板（前端 `onChange(dir)` + 收起）。
    fn choose_candidate(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.path
            .update(cx, |state, cx| state.set_value(path, window, cx));
        self.set_candidates_open(false, cx);
    }

    /// 从候选里删掉一条：记进忽略集，并让外层落盘。
    fn forget_candidate(&mut self, path: &str, cx: &mut Context<Self>) {
        let (history, ignored) = forget_project_path(&self.history, &self.ignored, path);
        self.history = history;
        self.ignored = ignored;
        cx.emit(ProjectPathFieldEvent::ForgetPath(path.to_string()));
        cx.notify();
    }

    fn open_picker(&mut self, cx: &mut Context<Self>) {
        // 起始目录取输入框里的草稿；草稿为空（或只有空白）时退回默认目录，
        // 前端 `initialPath={value.trim() || defaultWorkingDir}` 同口径。
        let raw = self.path.read(cx).value().to_string();
        let initial = submittable_path(&raw).unwrap_or_else(|| self.default_working_dir.clone());
        self.candidates_open = false;
        self.picker = Some(Picker {
            requested_path: initial.clone(),
            listing: None,
            loading: true,
            error: None,
        });
        self.list_directory(initial, cx);
        cx.emit(ProjectPathFieldEvent::PickerToggled);
        cx.notify();
    }

    /// 列举一层目录；`path` 为空表示从服务端进程的当前目录开始。
    fn list_directory(&mut self, path: String, cx: &mut Context<Self>) {
        let api = self.api.clone();
        self.list_task = Some(cx.spawn(async move |this, cx| {
            let result = api.kanban_list_directories(&path).await;
            this.update(cx, |this, cx| this.apply_listing(path, result, cx))
                .ok();
        }));
    }

    fn apply_listing(
        &mut self,
        requested_path: String,
        result: Result<DirectoryListing, ApiError>,
        cx: &mut Context<Self>,
    ) {
        // 选择器或整个弹窗已经关了：这一趟结果没有去处。
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        picker.requested_path = requested_path;
        picker.loading = false;
        match result {
            Ok(listing) => {
                picker.listing = Some(listing);
                picker.error = None;
            },
            Err(error) => {
                picker.listing = None;
                picker.error = Some(error.to_string());
            },
        }
        cx.notify();
    }

    fn go_to(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        picker.requested_path = path.clone();
        picker.loading = true;
        self.list_directory(path, cx);
        cx.notify();
    }

    /// 「选择此文件夹」：写回输入框并回到表单；提交仍要用户自己按按钮。
    fn picker_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self
            .picker
            .as_ref()
            .and_then(|picker| picker.listing.as_ref())
            .map(|listing| listing.path.clone())
        else {
            return;
        };
        self.path
            .update(cx, |state, cx| state.set_value(path, window, cx));
        self.picker = None;
        cx.emit(ProjectPathFieldEvent::PickerToggled);
        cx.notify();
    }

    fn picker_cancel(&mut self, cx: &mut Context<Self>) {
        self.picker = None;
        cx.emit(ProjectPathFieldEvent::PickerToggled);
        cx.notify();
    }

    /// 候选面板：挂在输入框下方，高度封顶后滚动。
    fn render_candidates(&self, candidates: Vec<String>, cx: &mut Context<Self>) -> AnyElement {
        let hover_background = cx.theme().list_hover;
        let mut panel = v_flex()
            .id(SharedString::from(format!("{}-candidates", self.id_prefix)))
            .absolute()
            .top_full()
            .left_0()
            .w_full()
            .mt_1()
            .max_h(px(CANDIDATE_PANEL_MAX_HEIGHT))
            .overflow_y_scroll()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .py_1();
        for candidate in candidates {
            let choose = candidate.clone();
            let forget = candidate.clone();
            panel = panel.child(
                h_flex()
                    .id(SharedString::from(format!(
                        "{}-candidate-{candidate}",
                        self.id_prefix
                    )))
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .rounded(cx.theme().radius)
                    .hover(move |this| this.bg(hover_background))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_candidate(&choose, window, cx)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .child(candidate.clone()),
                    )
                    // 删除按钮在行内，得自己把点击拦下，否则一次点击会连带选中这条路径。
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "{}-forget-{candidate}",
                                self.id_prefix
                            )))
                            .px_1()
                            .rounded(cx.theme().radius)
                            .text_color(cx.theme().muted_foreground)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.forget_candidate(&forget, cx);
                            }))
                            .child(IconName::Trash.element(Size::Size(px(14.0)))),
                    ),
            );
        }
        // `deferred`：面板是输入框那一格的绝对定位子元素，排在按钮行之前；不延迟绘制的话
        // 它是被按钮盖住的下层，前两行点不着（前端把下拉挂到 body 上，效果相同）。
        deferred(panel).into_any_element()
    }

    /// 文件夹选择器卡面。父弹窗在 [`Self::picker_open`] 为真时改画它。
    pub(crate) fn render_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(picker) = &self.picker else {
            return div().into_any_element();
        };
        let current_path = picker
            .listing
            .as_ref()
            .map(|listing| listing.path.clone())
            .unwrap_or_else(|| picker.requested_path.clone());
        let parent = picker
            .listing
            .as_ref()
            .and_then(|listing| listing.parent.clone());

        let mut header = h_flex()
            .items_center()
            .gap_2()
            .mb_2()
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(IconName::Folder.element(Size::Small)),
            )
            .child(div().flex_1().min_w_0().truncate().text_xs().child(
                if current_path.is_empty() {
                    "服务端当前目录".to_string()
                } else {
                    current_path
                },
            ));
        if let Some(parent) = parent {
            let go_parent = parent.clone();
            header = header.child(
                Button::new(SharedString::from(format!(
                    "{}-picker-parent",
                    self.id_prefix
                )))
                .ghost()
                .label("上级目录")
                .disabled(picker.loading)
                .on_click(cx.listener(move |this, _, _, cx| this.go_to(go_parent.clone(), cx))),
            );
        }

        // 读取中、出错、空目录、条目四者是互斥的（前端同样分四支画）。
        let mut list = v_flex()
            .id(SharedString::from(format!(
                "{}-picker-list",
                self.id_prefix
            )))
            .h(px(PICKER_LIST_HEIGHT))
            .overflow_y_scroll()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background);
        if picker.loading {
            list = list.child(
                div()
                    .px_3()
                    .py_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("读取中…"),
            );
        } else if let Some(error) = &picker.error {
            // 起始路径可能来自用户手输的草稿，列不出来时给一条回到服务端当前目录的退路。
            list = list.child(
                v_flex()
                    .items_start()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(error.clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "{}-picker-server-cwd",
                            self.id_prefix
                        )))
                        .ghost()
                        .label("从服务端当前目录开始")
                        .on_click(cx.listener(|this, _, _, cx| this.go_to(String::new(), cx))),
                    ),
            );
        } else if let Some(listing) = &picker.listing {
            if listing.entries.is_empty() {
                list = list.child(
                    div()
                        .px_3()
                        .py_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("没有子目录"),
                );
            }
            let hover_background = cx.theme().list_hover;
            for entry in &listing.entries {
                let go_entry = entry.path.clone();
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!(
                            "{}-picker-entry-{}",
                            self.id_prefix, entry.path
                        )))
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .hover(move |this| this.bg(hover_background))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.go_to(go_entry.clone(), cx)),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(IconName::Folder.element(Size::Size(px(13.0)))),
                        )
                        .child(div().min_w_0().truncate().child(entry.name.clone())),
                );
            }
            if listing.truncated {
                list = list.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("目录过多，列表已截断"),
                );
            }
        }

        v_flex()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .mb(px(18.0))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .child("选择文件夹"),
                    )
                    .child(icon_button(
                        "picker-close",
                        IconName::Close,
                        cx,
                        |this: &mut Self, cx| this.picker_cancel(cx),
                    )),
            )
            .child(header)
            .child(list)
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .mt_4()
                    .child(
                        Button::new(SharedString::from(format!(
                            "{}-picker-cancel",
                            self.id_prefix
                        )))
                        .outline()
                        .label("取消")
                        .on_click(cx.listener(|this, _, _, cx| this.picker_cancel(cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "{}-picker-select",
                            self.id_prefix
                        )))
                        .primary()
                        .label("选择此文件夹")
                        .disabled(picker.loading || picker.listing.is_none())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.picker_select(window, cx)),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ProjectPathField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut field = div()
            .id(SharedString::from(format!("{}-path", self.id_prefix)))
            .relative()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.path).disabled(!self.enabled)),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "{}-pick-folder",
                            self.id_prefix
                        )))
                        .ghost()
                        .label("选择文件夹")
                        .disabled(!self.enabled)
                        .on_click(cx.listener(|this, _, _, cx| this.open_picker(cx))),
                    ),
            );
        // 没有候选就什么都不画（前端 `open={pathMenuOpen && pathCandidates.length > 0}`）：
        // 一个空面板只剩一道描边。
        let candidates = self.candidates();
        if self.candidates_open && !candidates.is_empty() {
            field = field.child(self.render_candidates(candidates, cx));
        }
        field
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submittable_path_trims_and_rejects_blank() {
        assert_eq!(submittable_path("  /w/a  ").as_deref(), Some("/w/a"));
        assert_eq!(submittable_path("   "), None);
        assert_eq!(submittable_path(""), None);
    }
}
