//! askUser 问卷的解析与作答状态，对应 Web 前端的 `Chat/tools/askUser.ts`。
//!
//! 问卷本体分两处：题目与选项在工具参数 `questions` 里，作答结果回填到工具结果文本的
//! `answers`。这里只做 JSON 推导与作答状态机，不碰 gpui——卡片渲染在
//! `views::ask_user_card`，于是「选项怎么选、答案怎么拼」可以脱离窗口测。
//!
//! 解析口径照搬前端：题干与页眉必须在场，选项不足两个的题整道丢弃。

use std::collections::HashMap;

use astrcode_protocol::http::{ConversationBlockDto, ToolCallStatusDto};
use serde_json::Value;

/// askUser 工具的线缆名。
///
/// 与扩展侧同值（`astrcode-extension-ask-user` 的 `ASK_USER_TOOL_NAME`）：内置插件只依赖
/// 插件系统，共享 UI 层不引它的常量，靠这条注释对齐。
const ASK_USER_TOOL_NAME: &str = "askUser";

/// 一道题的选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AskUserOption {
    pub(crate) label: String,
    pub(crate) description: String,
    /// 选中后展开的预览正文；前端只在单选且选中时显示。
    pub(crate) preview: Option<String>,
    pub(crate) recommended: bool,
}

/// 一道待回答的题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AskUserQuestion {
    pub(crate) header: String,
    /// 题干既是对用户说的话，也是回填答案的键。
    pub(crate) question: String,
    pub(crate) options: Vec<AskUserOption>,
    pub(crate) multi_select: bool,
}

/// 这块是不是 askUser 工具调用（不论是否还在等待）。
pub(crate) fn is_ask_user(block: &ConversationBlockDto) -> bool {
    matches!(
        block,
        ConversationBlockDto::ToolCall { name, .. } if name == ASK_USER_TOOL_NAME
    )
}

/// 这张问卷还在等用户回答。
pub(crate) fn is_pending(block: &ConversationBlockDto) -> bool {
    matches!(
        block,
        ConversationBlockDto::ToolCall {
            name,
            status: ToolCallStatusDto::Streaming,
            ..
        } if name == ASK_USER_TOOL_NAME
    )
}

/// 参数里解析出的题目；参数还没流进来时是空的。
pub(crate) fn questions_for(block: &ConversationBlockDto) -> Vec<AskUserQuestion> {
    let ConversationBlockDto::ToolCall { arguments_json, .. } = block else {
        return Vec::new();
    };
    arguments_json
        .as_ref()
        .map(questions_in)
        .unwrap_or_default()
}

/// `questions` 字段里解析出的题目；与工具参数走同一处解析。
///
/// 跨会话快照（见 [`crate::pending_ask_user`]）的题面与工具参数同形，共用这一处才不会出现
/// 「卡片画得出来、横幅画不出来」这种两套口径。
pub(crate) fn questions_in(value: &Value) -> Vec<AskUserQuestion> {
    value
        .get("questions")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(question).collect())
        .unwrap_or_default()
}


/// 折叠摘要行：`askUser · 首个页眉 · N questions`，与前端 `askUserSummary` 一致。
pub(crate) fn summary(block: &ConversationBlockDto) -> Option<String> {
    let questions = questions_for(block);
    let first = questions.first()?;
    let count = questions.len();
    Some(format!(
        "{ASK_USER_TOOL_NAME} · {} · {count} {}",
        first.header,
        if count == 1 { "question" } else { "questions" }
    ))
}

/// 结果文本里回填的作答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletedAnswers {
    /// 题干 → 答案。
    pub(crate) answers: Vec<(String, String)>,
    /// 超时未响应，服务端按推荐选项代答。
    pub(crate) auto_selected: bool,
}

/// 结果文本里的作答；尚未作答（`awaiting_user_input`）或没有答案时为 `None`。
pub(crate) fn completed_answers(text: &str) -> Option<CompletedAnswers> {
    let parsed: Value = serde_json::from_str(text.trim()).ok()?;
    let object = parsed.as_object()?;
    if object.get("status").and_then(Value::as_str) == Some("awaiting_user_input") {
        return None;
    }
    let answers = object.get("answers")?.as_object()?;
    let answers: Vec<(String, String)> = answers
        .iter()
        .filter_map(|(question, answer)| Some((question.clone(), answer.as_str()?.to_owned())))
        .collect();
    (!answers.is_empty()).then(|| CompletedAnswers {
        answers,
        auto_selected: object
            .get("autoSelected")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// 一道题的用户作答草稿。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct QuestionDraft {
    pub(crate) selected: Vec<String>,
    pub(crate) use_other: bool,
    pub(crate) other_text: String,
}

/// 整张问卷的作答状态：当前题号，以及每题各自的草稿。
///
/// 键是题干——回填答案时用的也是题干，两处必须是同一个键。
#[derive(Debug, Default)]
pub(crate) struct Draft {
    index: usize,
    by_question: HashMap<String, QuestionDraft>,
}

impl Draft {
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    /// 题数变了以后把题号拉回范围内。
    ///
    /// 参数是流式落下来的，一道题的选项可能在下一批增量里被改写；题号不能因此越界。
    pub(crate) fn clamp(&mut self, len: usize) {
        self.index = self.index.min(len.saturating_sub(1));
    }

    /// 当前题的草稿；没选过任何东西时是一份空草稿。
    pub(crate) fn current(&self, questions: &[AskUserQuestion]) -> QuestionDraft {
        questions
            .get(self.index)
            .and_then(|question| self.by_question.get(&question.question))
            .cloned()
            .unwrap_or_default()
    }

    /// 选中一个选项：多选是切换，单选是替换；两者都作废「其他」。
    pub(crate) fn select(&mut self, question: &AskUserQuestion, label: &str) {
        let draft = self
            .by_question
            .entry(question.question.clone())
            .or_default();
        draft.use_other = false;
        if !question.multi_select {
            draft.selected = vec![label.to_owned()];
            return;
        }
        match draft.selected.iter().position(|selected| selected == label) {
            Some(index) => {
                draft.selected.remove(index);
            },
            None => draft.selected.push(label.to_owned()),
        }
    }

    /// 勾选/取消「其他（自定义输入）」；勾上时已选项作废。
    pub(crate) fn set_use_other(&mut self, question: &str, use_other: bool) {
        let draft = self.by_question.entry(question.to_owned()).or_default();
        draft.use_other = use_other;
        if use_other {
            draft.selected.clear();
        }
    }

    /// 写入「其他」的正文；能写字就意味着已经切到「其他」。
    pub(crate) fn set_other_text(&mut self, question: &str, text: String) {
        let draft = self.by_question.entry(question.to_owned()).or_default();
        draft.other_text = text;
        draft.use_other = true;
        draft.selected.clear();
    }

    pub(crate) fn previous(&mut self) {
        self.index = self.index.saturating_sub(1);
    }

    pub(crate) fn next(&mut self, last: usize) {
        self.index = (self.index + 1).min(last);
    }

    /// 当前题是否已经作答：没作答就不能往下走。
    pub(crate) fn answered_current(&self, questions: &[AskUserQuestion]) -> bool {
        questions
            .get(self.index)
            .is_some_and(|question| self.answer(question).is_some())
    }

    /// 提交用的答案：题干 → 答案。有任何一题没作答就返回 `None`。
    pub(crate) fn answers(&self, questions: &[AskUserQuestion]) -> Option<Vec<(String, String)>> {
        let mut answers = Vec::with_capacity(questions.len());
        for question in questions {
            answers.push((question.question.clone(), self.answer(question)?));
        }
        (!answers.is_empty()).then_some(answers)
    }

    fn answer(&self, question: &AskUserQuestion) -> Option<String> {
        let draft = self.by_question.get(&question.question)?;
        if draft.use_other {
            let text = draft.other_text.trim();
            return (!text.is_empty()).then(|| text.to_owned());
        }
        if question.multi_select {
            return (!draft.selected.is_empty()).then(|| draft.selected.join(", "));
        }
        draft.selected.first().cloned()
    }
}

/// 一道题；选项不足两个的题整道丢弃。
pub(crate) fn question(raw: &Value) -> Option<AskUserQuestion> {
    let object = raw.as_object()?;
    let header = object.get("header")?.as_str()?.to_owned();
    let text = object.get("question")?.as_str()?.to_owned();
    let options: Vec<AskUserOption> = object
        .get("options")?
        .as_array()?
        .iter()
        .filter_map(option)
        .collect();
    (options.len() >= 2).then(|| AskUserQuestion {
        header,
        question: text,
        options,
        multi_select: object
            .get("multiSelect")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// 一个选项；缺少文案的选项直接丢掉。
fn option(raw: &Value) -> Option<AskUserOption> {
    let object = raw.as_object()?;
    Some(AskUserOption {
        label: object.get("label")?.as_str()?.to_owned(),
        description: object.get("description")?.as_str()?.to_owned(),
        preview: object
            .get("preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|preview| !preview.is_empty())
            .map(str::to_owned),
        recommended: object
            .get("recommended")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::http::{ConversationBlockDto, ToolCallStatusDto};
    use serde_json::{Value, json};

    use super::{AskUserQuestion, Draft, completed_answers, is_pending, questions_for, summary};

    fn ask_user(
        status: ToolCallStatusDto,
        arguments_json: Value,
        text: &str,
    ) -> ConversationBlockDto {
        ConversationBlockDto::ToolCall {
            id: "c1".into(),
            name: "askUser".into(),
            arguments: arguments_json.to_string(),
            text: text.into(),
            status,
            metadata: None,
            approval: None,
            arguments_json: Some(arguments_json),
        }
    }

    fn questionnaire(questions: Value) -> ConversationBlockDto {
        ask_user(
            ToolCallStatusDto::Streaming,
            json!({ "questions": questions }),
            "",
        )
    }

    /// 两道题：第一题单选，第二题多选。
    fn two_questions() -> Vec<AskUserQuestion> {
        questions_for(&questionnaire(json!([
            {
                "header": "数据库",
                "question": "选哪个数据库？",
                "options": [
                    { "label": "Postgres", "description": "关系型", "recommended": true },
                    { "label": "SQLite", "description": "嵌入式" }
                ]
            },
            {
                "header": "测试",
                "question": "要哪些测试？",
                "multiSelect": true,
                "options": [
                    { "label": "单元", "description": "快" },
                    { "label": "端到端", "description": "慢" }
                ]
            }
        ])))
    }

    #[test]
    fn questions_come_from_the_tool_arguments() {
        let block = questionnaire(json!([
            {
                "header": "数据库",
                "question": "选哪个？",
                "options": [
                    { "label": "Postgres", "description": "关系型", "recommended": true },
                    { "label": "SQLite", "description": "嵌入式", "preview": " file.db " }
                ]
            },
            {
                "header": "被丢掉",
                "question": "只有一个选项？",
                "options": [{ "label": "唯一", "description": "凑数" }]
            },
            {
                "header": "也丢掉",
                "question": "选项缺文案？",
                "options": [
                    { "label": "甲" },
                    { "label": "乙", "description": "有文案" }
                ]
            }
        ]));
        let questions = questions_for(&block);

        assert_eq!(questions.len(), 1, "选项不足两个的题整道丢弃");
        assert_eq!(questions[0].header, "数据库");
        assert!(questions[0].options[0].recommended);
        assert_eq!(
            questions[0].options[1].preview.as_deref(),
            Some("file.db"),
            "预览两端空白会被裁掉"
        );
        assert!(!questions[0].multi_select, "缺省是单选");
        assert_eq!(
            summary(&block).as_deref(),
            Some("askUser · 数据库 · 1 question")
        );
    }

    #[test]
    fn a_pending_questionnaire_is_recognised_by_name_and_status() {
        assert!(is_pending(&questionnaire(json!([]))));

        let answered = ask_user(ToolCallStatusDto::Complete, json!({}), "");
        assert!(!is_pending(&answered), "已作答的问卷不再等回答");

        let other_tool = ask_user(ToolCallStatusDto::Streaming, json!({}), "");
        let ConversationBlockDto::ToolCall { name, .. } = other_tool else {
            panic!("测试块应当是工具调用");
        };
        assert_eq!(name, "askUser", "名字是唯一的判据来源");
    }

    #[test]
    fn answers_follow_the_selection_rules() {
        let questions = two_questions();
        let mut draft = Draft::default();

        assert!(draft.answers(&questions).is_none(), "一题都没答就不能提交");
        draft.select(&questions[0], "Postgres");
        assert!(draft.answered_current(&questions));
        assert!(draft.answers(&questions).is_none(), "第二题还没答");

        draft.select(&questions[0], "SQLite");
        let last = questions.len() - 1;
        draft.next(last);
        assert_eq!(draft.index(), 1);
        draft.select(&questions[1], "单元");
        draft.select(&questions[1], "端到端");
        assert_eq!(
            draft.answers(&questions),
            Some(vec![
                ("选哪个数据库？".to_owned(), "SQLite".to_owned()),
                ("要哪些测试？".to_owned(), "单元, 端到端".to_owned()),
            ]),
            "单选是替换，多选按点选顺序拼接"
        );

        draft.next(last);
        assert_eq!(draft.index(), last, "已经在最后一题");
        draft.previous();
        assert_eq!(draft.index(), 0);
    }

    #[test]
    fn the_other_answer_replaces_the_selection() {
        let questions = two_questions();
        let mut draft = Draft::default();

        draft.select(&questions[0], "Postgres");
        draft.set_use_other("选哪个数据库？", true);
        assert!(
            draft.answers(&questions).is_none(),
            "切到「其他」但还没写字"
        );

        draft.set_other_text("选哪个数据库？", "  DuckDB  ".to_owned());
        assert_eq!(
            draft.answers(&questions),
            None,
            "第二题仍未作答，但只要第一题能给出答案就说明替换生效"
        );
        draft.select(&questions[1], "单元");
        assert_eq!(
            draft.answers(&questions),
            Some(vec![
                ("选哪个数据库？".to_owned(), "DuckDB".to_owned()),
                ("要哪些测试？".to_owned(), "单元".to_owned()),
            ]),
            "自定义答案两端空白裁掉"
        );

        draft.set_use_other("选哪个数据库？", false);
        assert!(
            !draft.answered_current(&questions),
            "取消「其他」后第一题回到未选择"
        );
    }

    #[test]
    fn completed_answers_come_from_the_result_text() {
        let completed = completed_answers(
            r#"{"questions":[],"answers":{"选哪个？":"Postgres"},"autoSelected":true}"#,
        )
        .expect("结果里有答案");
        assert_eq!(
            completed.answers,
            vec![("选哪个？".to_owned(), "Postgres".to_owned())]
        );
        assert!(completed.auto_selected);

        assert!(
            completed_answers(r#"{"status":"awaiting_user_input","questions":[]}"#).is_none(),
            "等待中不算已回答"
        );
        assert!(completed_answers(r#"{"answers":{}}"#).is_none());
        assert!(completed_answers("User rejected the question").is_none());
    }
}
