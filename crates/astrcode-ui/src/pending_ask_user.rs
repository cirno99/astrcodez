//! 跨会话的待回答问卷：直播事件与轮询快照的归并。
//!
//! 对应前端 `store/delta/applyDelta.ts` 的 `customEvent` 两支与 `mergePendingAskUserSnapshot`。
//! 两条来源缺一不可：`ask_user.pending` / `ask_user.resolved` 是 `GlobalLive` 交付，而服务端
//! 只把它送进**别的**会话的流（`astrcode-server/src/http/stream.rs` 对同会话的全局事件直接丢弃），
//! 所以事件流没连上、或压根没打开某个会话时，「谁在等回答」只剩全局快照这一个来源；反过来，
//! 流连着时事件比 5 秒一次的快照新。
//!
//! 前端判「请求期间是否变过」用的是对象身份，这里换成直播事件序号：同一个意思，但不依赖身份。

use astrcode_protocol::http::{ConversationBlockDto, ToolCallStatusDto};
use serde_json::{Map, Value};

use crate::{ask_user, conversation::delta::block_id};

/// 问卷扩展的 id 与它声明的两个事件类型。
///
/// 与 `astrcode-extension-ask-user` 同值：内置插件只依赖插件系统，共享 UI 层不引它的常量。
pub(crate) const ASK_USER_EXTENSION_ID: &str = "astrcode-ask-user";
pub(crate) const PENDING_EVENT_TYPE: &str = "ask_user.pending";
pub(crate) const RESOLVED_EVENT_TYPE: &str = "ask_user.resolved";

/// 恢复出的工具块要用的工具名，与 `ask_user::ASK_USER_TOOL_NAME` 同值。
const ASK_USER_TOOL_NAME: &str = "askUser";

/// 一条待回答的问卷。
///
/// 键是「会话 id + 工具调用 id」：`callId` 只在 turn 内唯一，跨会话会撞，因此查表不能只看
/// `call_id`。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingQuestion {
    pub(crate) session_id: String,
    pub(crate) call_id: String,
    /// 解析后的题目：横幅要拿第一题的题干当文案。
    pub(crate) questions: Vec<ask_user::AskUserQuestion>,
    /// 原始载荷。恢复工具块时要把题面原样拼回参数，省掉把题目再序列化一遍。
    raw: Value,
}

impl PendingQuestion {
    /// 解一条问卷；缺 `sessionId` / `callId`、或一道完整的题都解不出来都算解码失败。
    ///
    /// 题面沿用工具参数那一套解析（见 `ask_user::questions_in`：选项不足两个的题整道丢弃），
    /// 于是「解出来了」与「卡片画得出来」是同一个判据。
    pub(crate) fn decode(raw: &Value) -> Option<Self> {
        let object = raw.as_object()?;
        let questions = ask_user::questions_in(raw);
        if questions.is_empty() {
            return None;
        }
        Some(Self {
            session_id: object.get("sessionId")?.as_str()?.to_owned(),
            call_id: object.get("callId")?.as_str()?.to_owned(),
            questions,
            raw: raw.clone(),
        })
    }

    /// 用问卷恢复出的工具块，对应前端 `recoveredAskUserBlock`。
    ///
    /// 只带 `questions` 与 `metadata`：`autoSelectAt` / `serverTime` 是服务端的内部状态，拼进
    /// 参数只会让工具块的参数行多出两块没人读的字段。
    pub(crate) fn recovered_block(&self) -> ConversationBlockDto {
        let mut arguments = Map::new();
        if let Some(questions) = self.raw.get("questions") {
            arguments.insert("questions".to_owned(), questions.clone());
        }
        if let Some(metadata) = self.raw.get("metadata") {
            arguments.insert("metadata".to_owned(), metadata.clone());
        }
        let arguments_json = Value::Object(arguments);
        ConversationBlockDto::ToolCall {
            id: self.call_id.clone(),
            name: ASK_USER_TOOL_NAME.to_owned(),
            arguments: arguments_json.to_string(),
            text: String::new(),
            status: ToolCallStatusDto::Streaming,
            metadata: None,
            approval: None,
            arguments_json: Some(arguments_json),
        }
    }
}

/// 这条问卷在当前会话的块里有没有可见的问卷卡片。
///
/// 流断过之后 live 工具块会丢，此时才拿快照把卡片恢复出来（前端 `pendingAskUserHasVisibleBlock`）。
pub(crate) fn has_visible_block(
    blocks: &[ConversationBlockDto],
    question: &PendingQuestion,
) -> bool {
    blocks
        .iter()
        .any(|block| block_id(block) == question.call_id && ask_user::is_pending(block))
}

/// 全局快照（`GET /api/extensions/astrcode-ask-user/questions`）里的问卷；有一条解不出来就整份
/// 作废。
///
/// 与前端 `decodePendingAskUserQuestionsResponse` 同口径：两端是同一个协议，一条坏数据说明契约
/// 已经错位，静默丢掉它只会让界面显示一份「少了点东西」的列表。
pub(crate) fn decode_snapshot(value: &Value) -> Option<Vec<PendingQuestion>> {
    value
        .get("questions")?
        .as_array()?
        .iter()
        .map(PendingQuestion::decode)
        .collect()
}

/// 表里的一条：问卷本身 + 它落地时的直播序号。
#[derive(Debug)]
struct Tracked {
    question: PendingQuestion,
    seq: u64,
}

/// 待回答问卷的全局集合。
///
/// 键里带会话 id，所以它跨会话存在，不能挂在 `ConversationState` 上：切会话不该把别的会话
/// 正在等回答这件事抹掉。
#[derive(Debug, Default)]
pub(crate) struct PendingQuestions {
    entries: Vec<Tracked>,
    /// 直播事件序号，只增不减。
    seq: u64,
    /// 直播里刚落地的回答；只用来挡住「请求期间取回的旧快照」，合并一次即清空。
    resolved: Vec<(String, String, u64)>,
}

impl PendingQuestions {
    /// 当前直播序号：轮询发起前记下它，合并时用来区分「这份快照之后到的」。
    pub(crate) fn live_seq(&self) -> u64 {
        self.seq
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 某个会话的问卷，按到达顺序。
    pub(crate) fn for_session<'a>(
        &'a self,
        session_id: &'a str,
    ) -> impl Iterator<Item = &'a PendingQuestion> + 'a {
        self.entries
            .iter()
            .map(|entry| &entry.question)
            .filter(move |question| question.session_id == session_id)
    }

    /// 除了这个会话以外的问卷；没有当前会话（没打开任何会话）时就是全部。
    pub(crate) fn others<'a>(
        &'a self,
        session_id: Option<&'a str>,
    ) -> impl Iterator<Item = &'a PendingQuestion> + 'a {
        self.entries
            .iter()
            .map(|entry| &entry.question)
            .filter(move |question| Some(question.session_id.as_str()) != session_id)
    }

    /// 应用一条直播事件，返回表是否变过。
    ///
    /// 不是本扩展的事件、不是这两个事件类型、载荷解不出来，都不改动任何东西：事件类型的
    /// 演进与别的扩展的事件会落在同一路增量里。
    pub(crate) fn apply_event(
        &mut self,
        extension_id: &str,
        event_type: &str,
        payload: &Value,
    ) -> bool {
        if extension_id != ASK_USER_EXTENSION_ID {
            return false;
        }
        let before = self.seq;
        match event_type {
            PENDING_EVENT_TYPE => self.push_pending(payload),
            RESOLVED_EVENT_TYPE => self.resolve(payload),
            _ => {},
        }
        self.seq != before
    }


    /// 用全局快照重建基线；`start_seq` 是发起这次请求时的 [`Self::live_seq`]。
    pub(crate) fn merge_snapshot(&mut self, snapshot: Vec<PendingQuestion>, start_seq: u64) {
        let mut merged: Vec<Tracked> = Vec::new();
        for question in snapshot {
            // 请求期间已有回答落地：旧快照不能把它复活。
            if resolved_after(
                &self.resolved,
                start_seq,
                &question.session_id,
                &question.call_id,
            ) {
                continue;
            }
            merged.push(Tracked {
                question,
                seq: start_seq,
            });
        }
        // 请求期间新到的直播条目比快照新：同键盖掉快照那一份，不同键补进来。更早的条目交给
        // 快照定去留——快照里没有它，说明它已经不在了。
        for entry in self.entries.drain(..) {
            if entry.seq <= start_seq {
                continue;
            }
            let question = &entry.question;
            if resolved_after(
                &self.resolved,
                start_seq,
                &question.session_id,
                &question.call_id,
            ) {
                continue;
            }
            merged.retain(|existing| {
                existing.question.session_id != question.session_id
                    || existing.question.call_id != question.call_id
            });
            merged.push(entry);
        }
        self.entries = merged;
        self.resolved.clear();
    }

    /// 直播里的 `ask_user.pending`。
    fn push_pending(&mut self, payload: &Value) {
        let Some(question) = PendingQuestion::decode(payload) else {
            return;
        };
        // 同一条问卷会重复到达（重订阅、重放）：已在表里就保持原值。
        if self.find(&question.session_id, &question.call_id).is_some() {
            return;
        }
        self.seq += 1;
        let seq = self.seq;
        // 同一个 callId 被新的一轮复用：先前那条「已作答」的墓碑要撤掉，否则新问卷会被当成
        // 已经答过的处理掉。
        self.resolved.retain(|(session_id, call_id, _)| {
            session_id != &question.session_id || call_id != &question.call_id
        });
        self.entries.push(Tracked { question, seq });
    }

    /// 直播里的 `ask_user.resolved`。
    fn resolve(&mut self, payload: &Value) {
        let Some(object) = payload.as_object() else {
            return;
        };
        let (Some(session_id), Some(call_id)) = (
            object.get("sessionId").and_then(Value::as_str),
            object.get("callId").and_then(Value::as_str),
        ) else {
            return;
        };
        self.seq += 1;
        let seq = self.seq;
        self.entries.retain(|entry| {
            entry.question.session_id != session_id || entry.question.call_id != call_id
        });
        self.resolved
            .push((session_id.to_owned(), call_id.to_owned(), seq));
    }

    fn find(&self, session_id: &str, call_id: &str) -> Option<&Tracked> {
        self.entries.iter().find(|entry| {
            entry.question.session_id == session_id && entry.question.call_id == call_id
        })
    }
}

/// 这个键是不是在快照请求开始之后才被回答的。
fn resolved_after(
    resolved: &[(String, String, u64)],
    start_seq: u64,
    session_id: &str,
    call_id: &str,
) -> bool {
    resolved
        .iter()
        .any(|(resolved_session, resolved_call, seq)| {
            *seq > start_seq && resolved_session == session_id && resolved_call == call_id
        })
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::http::ToolCallStatusDto;
    use serde_json::{Value, json};

    use super::*;

    fn payload(session_id: &str, call_id: &str) -> Value {
        json!({
            "sessionId": session_id,
            "callId": call_id,
            "autoSelectAt": 60_000,
            "serverTime": 1_000,
            "questions": [{
                "header": "Approach",
                "question": "Which approach?",
                "options": [
                    { "label": "A", "description": "First", "recommended": true },
                    { "label": "B", "description": "Second" }
                ]
            }]
        })
    }

    fn snapshot(entries: &[(&str, &str)]) -> Vec<PendingQuestion> {
        entries
            .iter()
            .map(|(session_id, call_id)| {
                PendingQuestion::decode(&payload(session_id, call_id)).expect("样例问卷应当可解")
            })
            .collect()
    }

    fn event(state: &mut PendingQuestions, event_type: &str, payload: &Value) {
        state.apply_event(ASK_USER_EXTENSION_ID, event_type, payload);
    }

    #[test]
    fn a_pending_event_lands_once_and_keeps_the_first_copy() {
        let mut state = PendingQuestions::default();
        event(
            &mut state,
            PENDING_EVENT_TYPE,
            &payload("session-1", "call-1"),
        );
        let seq = state.live_seq();
        event(
            &mut state,
            PENDING_EVENT_TYPE,
            &payload("session-1", "call-1"),
        );

        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.live_seq(), seq, "重复到达不改动表、也不推进序号");
        assert!(state.entries[0].question.questions[0].options[0].recommended);
    }

    #[test]
    fn a_call_id_reused_by_another_session_is_a_separate_row() {
        let mut state = PendingQuestions::default();
        event(
            &mut state,
            PENDING_EVENT_TYPE,
            &payload("session-1", "call-1"),
        );
        event(
            &mut state,
            PENDING_EVENT_TYPE,
            &payload("session-2", "call-1"),
        );
        event(
            &mut state,
            RESOLVED_EVENT_TYPE,
            &json!({ "sessionId": "session-1", "callId": "call-1" }),
        );

        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].question.session_id, "session-2");
        assert_eq!(state.for_session("session-1").count(), 0);
    }

    #[test]
    fn only_this_extensions_two_event_types_are_taken() {
        let mut state = PendingQuestions::default();
        state.apply_event("astrcode-kanban", PENDING_EVENT_TYPE, &payload("s", "c"));
        event(&mut state, "ask_user.something_new", &payload("s", "c"));

        assert!(state.is_empty());
        assert_eq!(state.live_seq(), 0, "不认的事件不推进序号");
    }

    #[test]
    fn an_undecodable_payload_is_ignored() {
        let mut state = PendingQuestions::default();
        event(
            &mut state,
            PENDING_EVENT_TYPE,
            &json!({ "callId": "call-1" }),
        );
        event(&mut state, PENDING_EVENT_TYPE, &json!({}));
        event(&mut state, RESOLVED_EVENT_TYPE, &json!({ "sessionId": 1 }));

        assert!(state.is_empty());
        assert_eq!(state.live_seq(), 0);
    }

    #[test]
    fn a_snapshot_covers_every_session() {
        let mut state = PendingQuestions::default();
        state.merge_snapshot(
            snapshot(&[("session-1", "call-1"), ("session-2", "call-2")]),
            0,
        );

        assert_eq!(state.for_session("session-1").count(), 1);
        assert_eq!(state.others(Some("session-1")).count(), 1);
        assert_eq!(state.others(None).count(), 2, "没开会话时全是别人的");
    }

    #[test]
    fn a_question_that_arrived_during_the_request_beats_the_snapshot() {
        let mut state = PendingQuestions::default();
        state.merge_snapshot(snapshot(&[("session-1", "call-old")]), 0);
        let start_seq = state.live_seq();
        event(
            &mut state,
            PENDING_EVENT_TYPE,
            &payload("session-2", "call-new"),
        );
        state.merge_snapshot(snapshot(&[("session-1", "call-1")]), start_seq);

        assert_eq!(state.entries.len(), 2);
        assert_eq!(
            state.others(Some("session-1")).next().unwrap().call_id,
            "call-new",
            "请求期间到达的那条不在快照里，也不该被丢掉"
        );
        assert_eq!(
            state.for_session("session-1").next().unwrap().call_id,
            "call-1",
            "请求之前那条不在快照里，就该消失"
        );
    }

    #[test]
    fn a_snapshot_does_not_revive_what_was_answered_during_the_request() {
        let mut state = PendingQuestions::default();
        state.merge_snapshot(snapshot(&[("session-1", "call-1")]), 0);
        let start_seq = state.live_seq();
        event(
            &mut state,
            RESOLVED_EVENT_TYPE,
            &json!({ "sessionId": "session-1", "callId": "call-1" }),
        );
        state.merge_snapshot(snapshot(&[("session-1", "call-1")]), start_seq);

        assert!(state.is_empty(), "旧快照不能把已回答的问卷复活");
    }

    #[test]
    fn an_answer_from_before_the_request_leaves_the_snapshot_authoritative() {
        let mut state = PendingQuestions::default();
        event(
            &mut state,
            RESOLVED_EVENT_TYPE,
            &json!({ "sessionId": "session-1", "callId": "call-1" }),
        );
        let start_seq = state.live_seq();
        // 墓碑早于这次请求：同一轮复用 callId 时，不能因为「答过」就把快照里的新问卷丢掉。
        state.merge_snapshot(snapshot(&[("session-1", "call-1")]), start_seq);

        assert_eq!(state.for_session("session-1").count(), 1);
    }

    #[test]
    fn a_reused_call_id_replaces_the_stale_snapshot_entry() {
        let mut state = PendingQuestions::default();
        state.merge_snapshot(snapshot(&[("session-1", "call-1")]), 0);
        let start_seq = state.live_seq();
        event(
            &mut state,
            RESOLVED_EVENT_TYPE,
            &json!({ "sessionId": "session-1", "callId": "call-1" }),
        );
        let mut reused = payload("session-1", "call-1");
        reused["questions"][0]["question"] = json!("新的问题");
        event(&mut state, PENDING_EVENT_TYPE, &reused);
        state.merge_snapshot(snapshot(&[("session-1", "call-1")]), start_seq);

        assert_eq!(state.entries.len(), 1);
        assert_eq!(
            state.entries[0].question.questions[0].question, "新的问题",
            "直播里重发的问卷要盖掉快照里那一份旧的"
        );
    }

    #[test]
    fn a_snapshot_with_a_broken_entry_is_rejected_whole() {
        let response = json!({
            "questions": [payload("session-1", "call-1"), { "sessionId": "session-2" }]
        });

        assert!(decode_snapshot(&response).is_none());
        assert!(decode_snapshot(&json!({})).is_none());
    }

    #[test]
    fn the_recovered_block_carries_the_questions_back_as_arguments() {
        let question = PendingQuestion::decode(&payload("session-1", "call-1")).expect("可解");
        let block = question.recovered_block();

        let ConversationBlockDto::ToolCall {
            id,
            name,
            status,
            text,
            arguments_json,
            ..
        } = &block
        else {
            panic!("恢复出来的应当是工具调用块");
        };
        assert_eq!(id, "call-1");
        assert_eq!(name, ASK_USER_TOOL_NAME);
        assert_eq!(*status, ToolCallStatusDto::Streaming);
        assert!(text.is_empty());
        assert_eq!(
            ask_user::questions_in(arguments_json.as_ref().unwrap()).len(),
            1
        );
        assert!(
            arguments_json
                .as_ref()
                .unwrap()
                .get("autoSelectAt")
                .is_none(),
            "服务端的内部状态不进参数"
        );
        assert!(ask_user::is_pending(&block));
    }

    #[test]
    fn a_pending_block_for_the_same_call_hides_the_recovered_card() {
        let question = PendingQuestion::decode(&payload("session-1", "call-1")).expect("可解");
        let mut block = question.recovered_block();
        assert!(has_visible_block(std::slice::from_ref(&block), &question));

        let ConversationBlockDto::ToolCall { status, .. } = &mut block else {
            panic!("恢复出来的应当是工具调用块");
        };
        *status = ToolCallStatusDto::Complete;
        assert!(!has_visible_block(std::slice::from_ref(&block), &question));
    }
}
