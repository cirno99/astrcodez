//! 新建项目弹窗：卡片外壳、提交与错误显示。
//!
//! 前端这件事由三个组件拼成（`Sidebar/NewProjectModal.tsx`、`Kanban/ProjectPathField.tsx`、
//! `Kanban/ProjectFolderPicker.tsx`）：路径那一半由 [`ProjectPathField`] 承担——新建卡片弹窗
//! 用的是同一个组件，候选列表与目录列举因此只有一份实现。
//!
//! 动作在这里判定，落到服务端的部分交给外壳：建会话、写回偏好、切页都是外壳的事——偏好文件
//! 是整份替换，只能有一个写者（见 `views/shell.rs`）。
//!
//! 刻意没跟的：
//! - 遮罩层与卡片是手绘的（铺满一层 + 流内居中卡片），不走组件库的 `Dialog`；
//! - 没有焦点归还与 Tab 焦点圈：前端的 `Modal` 打开时把焦点交给对话框容器、关闭时还给触发 元素、
//!   Tab 到头时绕回，这里三样都不做。Esc 因此不走「焦点在弹窗里」那条路，改用应用级 按键拦截器（见
//!   [`NewProjectModal::new`]）；
//! - 卡片没有阴影：前端用的是 `shadow-surface-lg`，这里与侧边栏的右键菜单一致，只留描边。

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Styled as _,
    Subscription, Window,
    component::{
        ActiveTheme as _, Disableable as _,
        button::{Button, ButtonVariants as _},
        h_flex, v_flex,
    },
    div, px,
};

use crate::{
    api::Api,
    icons::IconName,
    views::{
        icon_button,
        project_path_field::{ProjectPathField, ProjectPathFieldEvent, submittable_path},
    },
};

/// 卡片宽度，与前端 `dialogSurface` 的 `w-[460px]` 同值。
const MODAL_WIDTH: f32 = 460.0;
/// 文件夹选择器的卡片宽度，与前端 `w-[min(560px,92vw)]` 同值。
const PICKER_WIDTH: f32 = 560.0;
/// 遮罩层的内边距，与前端 `p-5` 同值；窗口比卡片窄时靠它留边。
const OVERLAY_PADDING: f32 = 20.0;

/// 弹窗对外的事件。服务端动作全在外壳手里。
#[derive(Debug, Clone)]
pub enum NewProjectEvent {
    /// 用户确认：在 `working_dir` 下建一条会话。
    Create(String),
    /// 用户从候选里删掉一条路径：清历史并记进忽略集，落盘在外壳。
    ForgetPath(String),
    /// 用户取消，或点了遮罩。
    Close,
}

pub struct NewProjectModal {
    /// 路径那一半：输入框、候选与文件夹选择器都在它里面。
    field: Entity<ProjectPathField>,
    /// 建会话的请求在飞：输入与取消都禁用，遮罩也不许关（前端 `closeOnOverlay={!loading}`）。
    loading: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NewProjectEvent> for NewProjectModal {}

impl NewProjectModal {
    pub fn new(
        api: Api,
        default_working_dir: String,
        extra_candidates: Vec<String>,
        history: Vec<String>,
        ignored: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = cx.new(|cx| {
            ProjectPathField::new(
                api,
                "new-project",
                default_working_dir,
                extra_candidates,
                history,
                ignored,
                window,
                cx,
            )
        });
        let view = cx.weak_entity();
        let subscriptions = vec![
            cx.subscribe_in(
                &field,
                window,
                |_, _, event: &ProjectPathFieldEvent, _, cx| match event {
                    ProjectPathFieldEvent::ForgetPath(path) => {
                        cx.emit(NewProjectEvent::ForgetPath(path.clone()));
                    },
                    // 选择器一开一关，卡面宽度与画什么都变；那是卡片外壳的事。
                    ProjectPathFieldEvent::PickerToggled => cx.notify(),
                },
            ),
            // Esc：候选面板（内层浮层）先收，没开面板时才是关弹窗。拦截器是应用级的、
            // 在动作派发之前触发，因此不依赖焦点——弹窗故意不取焦（前端的 `Modal` 把焦点
            // 交给对话框容器，这里连那一步都省了）。
            cx.intercept_keystrokes(move |event, _, cx| {
                if event.keystroke.key.as_str() != "escape" {
                    return;
                }
                let handled = view
                    .update(cx, |this, cx| this.dismiss_on_escape(cx))
                    .unwrap_or(false);
                if handled {
                    cx.stop_propagation();
                }
            }),
        ];
        Self {
            field,
            loading: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// 建会话失败：把消息显示在卡片里，并放开输入让用户改路径重试。
    pub fn show_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.loading = false;
        self.field
            .update(cx, |field, cx| field.set_enabled(true, cx));
        self.error = Some(message);
        cx.notify();
    }

    /// 关掉弹窗：点遮罩与「没开内层浮层时的 Esc」都走这里。
    ///
    /// 加载中与文件夹选择器开着时都不关——一次点击或一下 Esc 不该把用户正在做的事丢掉。
    /// 返回真表示这次按键已被消化（给 Esc 用；点遮罩不看返回值）。
    fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if self.loading || self.field.read(cx).picker_open() {
            return false;
        }
        cx.emit(NewProjectEvent::Close);
        true
    }

    /// Esc：候选面板开着时只收面板，面板没开才是关弹窗。
    fn dismiss_on_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.field.update(cx, |field, cx| field.dismiss_layers(cx)) {
            return true;
        }
        self.dismiss(cx)
    }

    /// 点遮罩：先收候选面板，再按 [`Self::dismiss`] 的判据决定关不关弹窗。
    fn dismiss_from_overlay(&mut self, cx: &mut Context<Self>) {
        self.field.update(cx, |field, cx| field.dismiss_layers(cx));
        self.dismiss(cx);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        cx.emit(NewProjectEvent::Close);
    }

    /// 确认：把去掉空白的路径交给外壳；失败由外壳调 [`Self::show_error`] 送回来。
    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let raw = self.field.read(cx).value(cx);
        let Some(working_dir) = submittable_path(&raw) else {
            return;
        };
        self.loading = true;
        self.field
            .update(cx, |field, cx| field.set_enabled(false, cx));
        self.error = None;
        cx.emit(NewProjectEvent::Create(working_dir));
        cx.notify();
    }

    /// 表单卡面。
    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let raw = self.field.read(cx).value(cx);
        let can_submit = !self.loading && submittable_path(&raw).is_some();
        let mut card = v_flex()
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
                            .child("新建项目"),
                    )
                    .child(icon_button(
                        "new-project-close",
                        IconName::Close,
                        "关闭",
                        cx,
                        |this: &mut Self, cx| this.cancel(cx),
                    )),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("工作目录"),
                    )
                    .child(self.field.clone()),
            );
        if let Some(error) = &self.error {
            card = card.child(
                div()
                    .mt_3()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().danger.opacity(0.15))
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        card.child(
            h_flex()
                .justify_end()
                .gap_2()
                .mt_4()
                .child(
                    Button::new("new-project-cancel")
                        .outline()
                        .label("取消")
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
                .child(
                    Button::new("new-project-create")
                        .primary()
                        .label(if self.loading {
                            "创建中..."
                        } else {
                            "创建"
                        })
                        .disabled(!can_submit)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
        )
        .into_any_element()
    }
}

impl Render for NewProjectModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picker_open = self.field.read(cx).picker_open();
        // 选择器开着时**替换**卡片内容，不叠第二层弹窗。
        let body = if picker_open {
            self.field.update(cx, |field, cx| field.render_picker(cx))
        } else {
            self.render_form(cx)
        };
        div()
            .id("new-project-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(OVERLAY_PADDING))
            .bg(cx.theme().overlay)
            // 点遮罩收起；点在卡片上由卡片自己拦下。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss_from_overlay(cx)),
            )
            .child(
                div()
                    .id("new-project-card")
                    .w_full()
                    .max_w(px(if picker_open { PICKER_WIDTH } else { MODAL_WIDTH }))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .p_6()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                    )
                    .child(body),
            )
    }
}
