//! 输入区的投递模式与待发队列。
//!
//! 对应前端 store 的 `composerDeliveryMode`、`pendingMessages` 与 `flushPendingQueued`：
//! 忙的时候按下的发送不直接出门，要么排进本地队列、要么注入当前 turn。判定与队列运算都在
//! 这里，不碰窗口；渲染留在 `views::chat`。

use astrcode_protocol::{http::ConversationControlStateDto, wire::PhaseDto};

/// 忙时按下发送，这条输入往哪去。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DeliveryMode {
    /// 排进待发队列，当前 turn 结束后按顺序提交。
    #[default]
    Queued,
    /// 直接注入当前 turn。
    Inject,
}

impl DeliveryMode {
    /// 点一下换到的那一档。
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Queued => Self::Inject,
            Self::Inject => Self::Queued,
        }
    }

    /// 切换按钮上的字。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queue",
            Self::Inject => "Inject",
        }
    }

    /// 切换按钮的悬停说明：说的是「下一条会怎样」，不是「现在是什么」。
    pub(crate) fn hint(self, can_inject: bool) -> &'static str {
        match self {
            Self::Queued => "下一条：Queue（默认）",
            Self::Inject if can_inject => "下一条：Inject 到当前 turn",
            Self::Inject => "Inject 需要 Agent 正在运行",
        }
    }
}

/// 会话是否处在执行阶段；取值与前端 `isExecutionPhase` 一致。
pub(crate) fn is_execution_phase(phase: PhaseDto) -> bool {
    matches!(
        phase,
        PhaseDto::Thinking | PhaseDto::Streaming | PhaseDto::CallingTool | PhaseDto::Compacting
    )
}

/// 现在能不能把输入注入当前 turn。
///
/// 与后端 `TurnRegistry` 对齐：必须有活跃 turn。压缩期间除外——压缩会换掉整篇转录，
/// 注进去的输入落不到新的历史里（前端 `canInjectMidTurn` 同判据）。
pub(crate) fn can_inject(control: Option<&ConversationControlStateDto>) -> bool {
    control.is_some_and(|control| {
        control.phase != PhaseDto::Compacting && control.active_turn_id.is_some()
    })
}

/// 队列里的一条待发输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingMessage {
    pub(crate) id: String,
    pub(crate) text: String,
}

/// 本地待发队列；顺序即提交顺序。
///
/// id 只用来定位「这一条」（元素 id 与行内动作）。前端用 `crypto.randomUUID()`，这里用队列
/// 自己的自增序号：同一个队列内唯一就够，不必引入随机数依赖。
#[derive(Debug, Default)]
pub(crate) struct PendingQueue {
    items: Vec<PendingMessage>,
    next_id: u64,
}

impl PendingQueue {
    pub(crate) fn items(&self) -> &[PendingMessage] {
        &self.items
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    /// 排到队尾，返回这一条的 id。
    pub(crate) fn push(&mut self, text: String) -> String {
        self.next_id += 1;
        let id = format!("queued-{}", self.next_id);
        self.items.push(PendingMessage {
            id: id.clone(),
            text,
        });
        id
    }

    /// 丢掉一条；返回它是否在队列里。
    pub(crate) fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|message| message.id != id);
        self.items.len() != before
    }

    /// 「编辑」：把这一条的正文取回来（并出队），由调用方写回输入区。
    pub(crate) fn take_text(&mut self, id: &str) -> Option<String> {
        let index = self.items.iter().position(|message| message.id == id)?;
        Some(self.items.remove(index).text)
    }

    /// 取回一条待发输入，放回队尾；重发失败时用，id 保持不变。
    pub(crate) fn restore(&mut self, message: PendingMessage) {
        self.items.retain(|item| item.id != message.id);
        self.items.push(message);
    }

    /// 取出全部待发输入，队列清空。
    ///
    /// 前端 `flushPendingQueued` 在此之前还做一次 inject → queued 的归一化；这里入队时就只有
    /// 一种投递方式（`DeliveryMode` 只决定下一条往哪去），那一步不需要。
    pub(crate) fn drain(&mut self) -> Vec<PendingMessage> {
        std::mem::take(&mut self.items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(phase: PhaseDto, active_turn: Option<&str>) -> ConversationControlStateDto {
        ConversationControlStateDto {
            phase,
            can_submit_prompt: phase == PhaseDto::Idle,
            can_request_compact: false,
            active_turn_id: active_turn.map(str::to_owned),
            retry_status: None,
        }
    }

    #[test]
    fn queue_keeps_order_and_hands_out_unique_ids() {
        let mut queue = PendingQueue::default();
        let first = queue.push("第一条".to_owned());
        let second = queue.push("第二条".to_owned());

        assert_ne!(first, second);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.items()[0].text, "第一条");

        assert!(queue.remove(&first));
        assert!(!queue.remove(&first), "同一条不能删两次");
        assert_eq!(queue.items().len(), 1);
        assert_eq!(queue.items()[0].id, second);
    }

    #[test]
    fn editing_takes_the_text_out_and_a_failed_resend_puts_it_back() {
        let mut queue = PendingQueue::default();
        let id = queue.push("半句话".to_owned());

        let message = PendingMessage {
            id: id.clone(),
            text: "半句话".to_owned(),
        };
        assert_eq!(queue.take_text(&id).as_deref(), Some("半句话"));
        assert!(queue.is_empty(), "取回编辑后不再排队");
        assert_eq!(queue.take_text(&id), None);

        queue.restore(message);
        assert_eq!(queue.items().len(), 1);
        assert_eq!(queue.items()[0].id, id);
    }

    #[test]
    fn restore_does_not_duplicate_a_message_that_is_still_queued() {
        let mut queue = PendingQueue::default();
        let id = queue.push("排队中".to_owned());
        queue.restore(PendingMessage {
            id: id.clone(),
            text: "排队中".to_owned(),
        });

        assert_eq!(queue.items().len(), 1);
    }

    #[test]
    fn drain_empties_the_queue_in_order() {
        let mut queue = PendingQueue::default();
        queue.push("一".to_owned());
        queue.push("二".to_owned());

        let drained: Vec<String> = queue.drain().into_iter().map(|item| item.text).collect();
        assert_eq!(drained, vec!["一".to_owned(), "二".to_owned()]);
        assert!(queue.is_empty());

        // 队列空过之后 id 不从 1 重来：元素 id 在同一会话里必须唯一。
        assert_eq!(queue.push("三".to_owned()), "queued-3");
    }

    #[test]
    fn execution_phases_match_the_ported_set() {
        assert!(is_execution_phase(PhaseDto::Thinking));
        assert!(is_execution_phase(PhaseDto::Streaming));
        assert!(is_execution_phase(PhaseDto::CallingTool));
        assert!(is_execution_phase(PhaseDto::Compacting));
        assert!(!is_execution_phase(PhaseDto::Idle));
        assert!(!is_execution_phase(PhaseDto::Error));
    }

    #[test]
    fn inject_needs_an_active_turn_and_is_off_while_compacting() {
        assert!(!can_inject(None));
        assert!(!can_inject(Some(&control(PhaseDto::Idle, None))));
        assert!(can_inject(Some(&control(PhaseDto::Streaming, Some("t1")))));
        assert!(!can_inject(Some(&control(
            PhaseDto::Compacting,
            Some("t1")
        ))));
    }

    #[test]
    fn delivery_mode_toggles_and_says_what_the_next_send_does() {
        assert_eq!(DeliveryMode::default(), DeliveryMode::Queued);
        assert_eq!(DeliveryMode::Queued.toggled(), DeliveryMode::Inject);
        assert_eq!(
            DeliveryMode::Inject.toggled().toggled(),
            DeliveryMode::Inject,
            "翻两次应当回到原档"
        );

        assert_eq!(DeliveryMode::Queued.label(), "Queue");
        assert_eq!(DeliveryMode::Inject.label(), "Inject");
        assert_eq!(
            DeliveryMode::Inject.hint(true),
            "下一条：Inject 到当前 turn"
        );
        assert_eq!(
            DeliveryMode::Inject.hint(false),
            "Inject 需要 Agent 正在运行"
        );
    }

    #[test]
    fn inject_follows_the_active_turn_not_the_phase() {
        // 后端是唯一权威：认的是有没有活跃 turn。阶段回来了、turn 还没清干净时照样能注入。
        assert!(can_inject(Some(&control(PhaseDto::Idle, Some("t1")))));
    }
}
