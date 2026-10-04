//! 待回答的 askUser 问卷卡片，对应 Web 前端的 `Chat/tools/AskUserCard.tsx`。
//!
//! 卡片自带交互态（当前题号、每题草稿、「其他」输入框、提交状态），由 `views::chat` 按工具
//! 调用 id 持有：问卷还在等回答时渲染在过程折叠区**外**，作答完成后随工具块回到折叠区里，
//! 那时它只负责把问答列出来。
//!
//! 强调色一律用 `primary` 而不是 `accent`——本主题的 `accent` 是一块浅色**背景**，
//! 当文字色或描边色用会与底色糊在一起（同 `views::chat::activity_color`）。

use astrcode_protocol::http::ConversationBlockDto;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window,
    component::{
        ActiveTheme as _, Disableable as _, Sizable as _, Size,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    div,
};

use crate::{
    api::Api,
    ask_user::{self, AskUserOption, AskUserQuestion, Draft, QuestionDraft},
    conversation::delta::block_id,
    icons::IconName,
};

/// 待回答/已作答的问卷卡片。
pub(crate) struct AskUserCard {
    api: Api,
    session_id: String,
    /// 工具调用 id：作答按它回填。
    call_id: String,
    /// 参数里解析出的问卷；空表示参数还没流进来。
    questions: Vec<AskUserQuestion>,
    /// 结果文本；空表示服务端还没回填结果。
    result: String,
    draft: Draft,
    /// 「其他（自定义输入）」的输入框：第一次要用时才建——`InputState` 需要 `Window`，
    /// 而卡片是在状态更新里创建的，那时手里没有窗口。
    other: Option<Entity<InputState>>,
    _other_subscription: Option<Subscription>,
    submitting: bool,
    /// 已提交但服务端还没回执：问卷仍挂着，但答案已经定了。
    submitted: Option<Vec<(String, String)>>,
    error: Option<String>,
}

/// 卡片当前该显示的东西。
enum CardState {
    /// 答案已经定了：来自服务端回填的结果，或本地刚提交的答案。
    Answers {
        answers: Vec<(String, String)>,
        /// 只有本地提交，还没等到结果。
        submitted: bool,
        /// 超时未响应，服务端按推荐选项代答。
        auto_selected: bool,
    },
    /// 参数还没流进来。
    Waiting,
    /// 问卷本体。
    Questions(Vec<AskUserQuestion>),
}

impl AskUserCard {
    /// 新建卡片；块里的问卷与结果在 [`AskUserCard::set_block`] 里同步。
    pub(crate) fn new(
        api: Api,
        session_id: String,
        block: &ConversationBlockDto,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut card = Self {
            api,
            session_id,
            call_id: block_id(block).to_owned(),
            questions: Vec::new(),
            result: String::new(),
            draft: Draft::default(),
            other: None,
            _other_subscription: None,
            submitting: false,
            submitted: None,
            error: None,
        };
        card.set_block(block, cx);
        card
    }

    /// 用最新的工具块刷新快照：问卷与结果都只从这里来。
    pub(crate) fn set_block(&mut self, block: &ConversationBlockDto, cx: &mut Context<Self>) {
        let ConversationBlockDto::ToolCall { text, .. } = block else {
            return;
        };
        let questions = ask_user::questions_for(block);
        let changed = questions != self.questions || *text != self.result;
        self.questions = questions;
        self.result = text.clone();
        self.draft.clamp(self.questions.len());
        if changed {
            cx.notify();
        }
    }

    fn state(&self) -> CardState {
        if let Some(completed) = ask_user::completed_answers(&self.result) {
            return CardState::Answers {
                answers: completed.answers,
                submitted: false,
                auto_selected: completed.auto_selected,
            };
        }
        if self.questions.is_empty() {
            return CardState::Waiting;
        }
        match &self.submitted {
            Some(answers) => CardState::Answers {
                answers: answers.clone(),
                submitted: true,
                auto_selected: false,
            },
            None => CardState::Questions(self.questions.clone()),
        }
    }

    /// 当前题的题干：作答草稿与「其他」输入框都按它索引。
    fn current_key(&self) -> Option<&str> {
        self.questions
            .get(self.draft.index())
            .map(|question| question.question.as_str())
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        let Some(answers) = self.draft.answers(&self.questions) else {
            return;
        };
        self.submitting = true;
        self.error = None;
        let api = self.api.clone();
        let session_id = self.session_id.clone();
        let call_id = self.call_id.clone();
        let submitted = answers.clone();
        cx.spawn(async move |this, cx| {
            let result = api.respond_ask_user(&session_id, &call_id, &answers).await;
            this.update(cx, |this, cx| {
                this.submitting = false;
                match result {
                    Ok(()) => this.submitted = Some(submitted),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn reject(&mut self, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        self.submitting = true;
        self.error = None;
        let api = self.api.clone();
        let session_id = self.session_id.clone();
        let call_id = self.call_id.clone();
        cx.spawn(async move |this, cx| {
            let result = api.reject_ask_user(&session_id, &call_id).await;
            this.update(cx, |this, cx| {
                this.submitting = false;
                if let Err(error) = result {
                    this.error = Some(error.to_string());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 换一道题；输入框里留着上一题的文字，要跟着换。
    fn go_to(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.questions.len().saturating_sub(1);
        if next {
            self.draft.next(last);
        } else {
            self.draft.previous();
        }
        self.sync_other_input(window, cx);
        cx.notify();
    }

    /// 把当前题草稿里的「其他」文字灌回输入框。
    fn sync_other_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft.current(&self.questions).other_text;
        let Some(input) = self.other.clone() else {
            return;
        };
        input.update(cx, |state, cx| state.set_value(text, window, cx));
    }

    /// 「自定义输入」的输入框；没有就现建一个，并把改动记进当前题的草稿。
    fn other_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = self.other.clone() {
            return input;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("输入你的回答…"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.store_other(cx);
            }
        });
        self._other_subscription = Some(subscription);
        self.other = Some(input.clone());
        input
    }

    /// 输入框里的文字落到当前题的草稿上。
    fn store_other(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.current_key().map(str::to_owned) else {
            return;
        };
        let Some(input) = self.other.clone() else {
            return;
        };
        let text = input.read(cx).value().to_string();
        self.draft.set_other_text(&key, text);
        cx.notify();
    }

    fn render_form(
        &mut self,
        questions: Vec<AskUserQuestion>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = self.draft.index();
        let current = &questions[index];
        let draft = self.draft.current(&questions);
        let answered = self.draft.answered_current(&questions);
        let can_submit = self.draft.answers(&questions).is_some();
        let submitting = self.submitting;

        let mut options = v_flex().gap_2();
        for option in &current.options {
            options = options.child(self.render_option(current, &draft, option, cx));
        }

        let mut other = v_flex()
            .gap_2()
            .p_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_dashed()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Checkbox::new(SharedString::from("ask-other"))
                            .checked(draft.use_other)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                if let Some(key) = this.current_key().map(str::to_owned) {
                                    this.draft.set_use_other(&key, *checked);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("其他（自定义输入）"),
                    ),
            );
        if draft.use_other {
            let input = self.other_input(window, cx);
            other = other.child(Input::new(&input));
        }

        let mut actions = h_flex().gap_2().flex_wrap();
        if index > 0 {
            actions = actions.child(
                Button::new("ask-prev")
                    .outline()
                    .small()
                    .label("上一题")
                    .disabled(submitting)
                    .on_click(cx.listener(|this, _, window, cx| this.go_to(false, window, cx))),
            );
        }
        if index + 1 == questions.len() {
            actions = actions.child(
                Button::new("ask-submit")
                    .primary()
                    .small()
                    .label(if submitting {
                        "提交中…"
                    } else {
                        "提交回答"
                    })
                    .disabled(!can_submit || submitting)
                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
            );
        } else {
            actions = actions.child(
                Button::new("ask-next")
                    .primary()
                    .small()
                    .label("下一题")
                    .disabled(!answered || submitting)
                    .on_click(cx.listener(|this, _, window, cx| this.go_to(true, window, cx))),
            );
        }
        actions = actions.child(
            Button::new("ask-reject")
                .outline()
                .small()
                .label("拒绝")
                .disabled(submitting)
                .on_click(cx.listener(|this, _, _, cx| this.reject(cx))),
        );

        let mut column = v_flex().gap_3();
        if questions.len() > 1 {
            column = column.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("问题 {} / {}", index + 1, questions.len())),
            );
        }
        column = column
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .self_start()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(cx.theme().primary)
                            .child(current.header.clone()),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(current.question.clone()),
                    )
                    .child(options),
            )
            .child(other)
            .child(actions);

        if let Some(error) = &self.error {
            column = column.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        column.into_any_element()
    }

    fn render_option(
        &self,
        question: &AskUserQuestion,
        draft: &QuestionDraft,
        option: &AskUserOption,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = if draft.use_other {
            false
        } else if question.multi_select {
            draft.selected.iter().any(|label| label == &option.label)
        } else {
            draft
                .selected
                .first()
                .is_some_and(|label| label == &option.label)
        };
        let border = if selected {
            cx.theme().primary
        } else {
            cx.theme().border
        };
        let hover_background = cx.theme().list_hover;
        let selected_option = question.clone();
        let label = option.label.clone();

        // 预览只在单选且选中时铺开：多选下同时展开几段预览会把选项挤没。
        let preview = (selected && !question.multi_select)
            .then(|| option.preview.clone())
            .flatten();

        v_flex()
            .id(SharedString::from(format!("ask-option-{}", option.label)))
            .gap_1()
            .w_full()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(border)
            .hover(move |this| this.bg(hover_background))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.draft.select(&selected_option, &label);
                cx.notify();
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(option.label.clone()),
                    )
                    .children(option.recommended.then(|| {
                        div()
                            .px_1()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().primary)
                            .text_xs()
                            .text_color(cx.theme().primary)
                            .child("推荐")
                    }))
                    .children(selected.then(|| {
                        IconName::Check
                            .element(Size::Small)
                            .text_color(cx.theme().primary)
                    })),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(option.description.clone()),
            )
            .children(preview.map(|preview| {
                div()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(preview)
            }))
            .into_any_element()
    }

    fn render_answers(
        answers: Vec<(String, String)>,
        submitted: bool,
        auto_selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut column = v_flex().gap_2().child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if submitted {
                    "已提交，等待继续"
                } else {
                    "用户回答"
                }),
        );
        if auto_selected {
            column = column.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("超时未响应，已自动选择推荐选项"),
            );
        }
        for (question, answer) in answers {
            column = column.child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(question),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("→"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(answer),
                    ),
            );
        }
        column.into_any_element()
    }
}

impl Render for AskUserCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = v_flex()
            .gap_3()
            .p_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().accent);

        match self.state() {
            CardState::Answers {
                answers,
                submitted,
                auto_selected,
            } => card.child(Self::render_answers(answers, submitted, auto_selected, cx)),
            CardState::Waiting => card.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("等待交互参数…"),
            ),
            CardState::Questions(questions) => card.child(self.render_form(questions, window, cx)),
        }
    }
}
