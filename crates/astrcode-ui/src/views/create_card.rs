//! 新建卡片弹窗：标题、工作目录、归属日与正文。
//!
//! 对应前端的 `Kanban/CreateCardModal.tsx`。路径那一半用与新建项目同一个
//! [`ProjectPathField`]，选择器开着时同样**替换**卡片内容、不叠第二层弹窗。
//!
//! 发请求与刷新看板都在看板页手里（`views/kanban.rs`）：卡片列表是它的状态，
//! 建完要一并重取。

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Styled as _,
    Subscription, Window,
    component::{
        ActiveTheme as _, Disableable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputState, Textarea, TextareaState},
        v_flex,
    },
    div, px,
};

use crate::{
    api::Api,
    icons::IconName,
    kanban::today_key,
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

/// 校验表单：标题与工作目录都必须有值。
///
/// 提交判据（按钮亮不亮）与提交载荷共用它——两处各判一次，迟早会不一致。
pub(crate) fn validate(title: &str, working_dir: &str) -> Result<(String, String), &'static str> {
    let title = title.trim();
    if title.is_empty() {
        return Err("卡片标题不能为空");
    }
    let Some(working_dir) = submittable_path(working_dir) else {
        return Err("需要指定工作目录");
    };
    Ok((title.to_string(), working_dir))
}

/// 归属日：留空就交给扩展（它取创建当天）。
///
/// 扩展对空串是 400（`resolve_day(Some(""))` 报错），所以空串不能原样发过去。
pub(crate) fn normalized_date(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// 弹窗对外的事件。服务端动作与看板刷新都在看板页手里。
#[derive(Debug, Clone)]
pub enum CreateCardEvent {
    /// 用户确认。
    Create {
        title: String,
        body: String,
        working_dir: String,
        /// `None` 表示交给扩展取创建当天。
        date: Option<String>,
    },
    /// 用户从候选里删掉一条路径：清历史并记进忽略集，落盘在外壳。
    ForgetPath(String),
    /// 用户取消，或点了遮罩。
    Close,
}

pub struct CreateCardModal {
    title: Entity<InputState>,
    /// 路径那一半：输入框、候选与文件夹选择器都在它里面。
    field: Entity<ProjectPathField>,
    /// 归属日：文本输入，默认今天是「回到今天」用的同一把日键。
    date: Entity<InputState>,
    body: Entity<TextareaState>,
    /// 新建请求在飞：输入与取消都禁用，遮罩也不许关（前端 `closeOnOverlay={!loading}`）。
    loading: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CreateCardEvent> for CreateCardModal {}

impl CreateCardModal {
    pub fn new(
        api: Api,
        default_working_dir: String,
        extra_candidates: Vec<String>,
        history: Vec<String>,
        ignored: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("卡片标题"));
        let date = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("归属日，例如 2026-03-07")
                .default_value(today_key())
        });
        // 正文是多行的，回车要能换行（卡片正文里换行是正常的），因此不 `submit_on_enter`。
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("补充说明（可选）")
                .auto_grow(3, 8)
        });
        let field = cx.new(|cx| {
            ProjectPathField::new(
                api,
                "kanban-card",
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
                        cx.emit(CreateCardEvent::ForgetPath(path.clone()));
                    },
                    // 选择器一开一关，卡面宽度与画什么都变。
                    ProjectPathFieldEvent::PickerToggled => cx.notify(),
                },
            ),
            // Esc：候选面板（内层浮层）先收，没开面板时才是关弹窗。理由与新建项目弹窗同一套：
            // 弹窗不取焦，快捷键走应用级拦截器而不走焦点。
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
            title,
            field,
            date,
            body,
            loading: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// 新建失败：把消息显示在卡片里，并放开输入让用户改完重试。
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
    fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if self.loading || self.field.read(cx).picker_open() {
            return false;
        }
        cx.emit(CreateCardEvent::Close);
        true
    }

    fn dismiss_on_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.field.update(cx, |field, cx| field.dismiss_layers(cx)) {
            return true;
        }
        self.dismiss(cx)
    }

    fn dismiss_from_overlay(&mut self, cx: &mut Context<Self>) {
        self.field.update(cx, |field, cx| field.dismiss_layers(cx));
        self.dismiss(cx);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        cx.emit(CreateCardEvent::Close);
    }

    /// 此刻的表单值：校验通过时给出标题与工作目录，否则给出该报的那一条。
    fn draft(&self, cx: &Context<Self>) -> Result<(String, String), &'static str> {
        let title = self.title.read(cx).value().to_string();
        let working_dir = self.field.read(cx).value(cx);
        validate(&title, &working_dir)
    }

    /// 确认：把表单值交给看板页；失败由看板页调 [`Self::show_error`] 送回来。
    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let Ok((title, working_dir)) = self.draft(cx) else {
            return;
        };
        self.loading = true;
        self.field
            .update(cx, |field, cx| field.set_enabled(false, cx));
        self.error = None;
        cx.emit(CreateCardEvent::Create {
            title,
            body: self.body.read(cx).value().to_string(),
            working_dir,
            date: normalized_date(&self.date.read(cx).value()),
        });
        cx.notify();
    }

    /// 表单卡面。
    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let can_submit = !self.loading && self.draft(cx).is_ok();
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
                            .child("新建卡片"),
                    )
                    .child(icon_button(
                        "kanban-card-close",
                        IconName::Close,
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
                            .child("标题"),
                    )
                    .child(Input::new(&self.title).disabled(self.loading)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .mt_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("工作目录"),
                    )
                    .child(self.field.clone()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .mt_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("归属日"),
                    )
                    .child(Input::new(&self.date).disabled(self.loading)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .mt_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("正文"),
                    )
                    .child(Textarea::new(&self.body).disabled(self.loading)),
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
                    Button::new("kanban-card-cancel")
                        .outline()
                        .label("取消")
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
                .child(
                    Button::new("kanban-card-add")
                        .primary()
                        .label(if self.loading {
                            "添加中..."
                        } else {
                            "添加到待办"
                        })
                        .disabled(!can_submit)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
        )
        .into_any_element()
    }
}

impl Render for CreateCardModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picker_open = self.field.read(cx).picker_open();
        let body = if picker_open {
            self.field.update(cx, |field, cx| field.render_picker(cx))
        } else {
            self.render_form(cx)
        };
        div()
            .id("kanban-card-overlay")
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
                    .id("kanban-card")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_needs_both_a_title_and_a_working_dir() {
        assert_eq!(
            validate("  接入看板  ", "  /w/a "),
            Ok(("接入看板".to_string(), "/w/a".to_string()))
        );
        assert_eq!(validate("   ", "/w/a"), Err("卡片标题不能为空"));
        assert_eq!(validate("t", "   "), Err("需要指定工作目录"));
    }

    /// 留空归属日由扩展取创建当天；空串会被扩展判成 400，因此必须是 `None`。
    #[test]
    fn a_blank_date_is_omitted_rather_than_sent_as_an_empty_string() {
        assert_eq!(normalized_date(""), None);
        assert_eq!(normalized_date("   "), None);
        assert_eq!(normalized_date(" 2026-3-7 ").as_deref(), Some("2026-3-7"));
    }
}
