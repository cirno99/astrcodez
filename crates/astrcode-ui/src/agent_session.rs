//! 子 Agent 会话的展示状态与增量归并。
//!
//! 对应前端 `store/delta/blockHelpers.ts` 的 `applyAgentSessionUpdate` 与
//! `store/delta/applyDelta.ts` 里 `agentSessionUpdated` / `agentSessionRemoved` 两支。
//! 这里只做纯推导，不碰 gpui——卡片渲染在 `views::chat`。
//!
//! 快照里的链接（[`AgentSessionLinkDto`]）不带阶段与当前工具：那两个字段只在直播期间由
//! `Progress` 增量补上，所以它们在这里是本地的可选状态，不是线缆契约的一部分。

use astrcode_protocol::{
    http::{
        AgentSessionLinkDto, AgentSessionStatusDto, AgentSessionUpdateDto, ConversationBlockDto,
    },
    wire::PhaseDto,
};

/// agent 工具的线缆名。
///
/// 与扩展侧同值（`astrcode-extension-agent-tools` 的 `AGENT_TOOL_NAME`）：内置插件只依赖
/// 插件系统，共享 UI 层不引它的常量，靠这条注释对齐。
pub(crate) const AGENT_TOOL_NAME: &str = "agent";

/// 一个子 Agent 会话的展示状态。
///
/// 字段就是卡片要画的那几项，归并规则也与前端逐字段一致，因此同值判断直接用 `PartialEq`。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentSession {
    pub(crate) child_session_id: String,
    /// 派生它的那次工具调用 id；卡片靠它挂回工具行。
    pub(crate) tool_call_id: Option<String>,
    pub(crate) agent_name: String,
    pub(crate) task: String,
    pub(crate) status: AgentSessionStatusDto,
    pub(crate) final_session_id: Option<String>,
    pub(crate) summary: Option<String>,
    pub(crate) error: Option<String>,
    /// 运行阶段；只由 `Progress` 增量维护，快照基线里没有。
    pub(crate) phase: Option<PhaseDto>,
    /// 当前正在调用的工具名；与 [`Self::phase`] 同源。
    pub(crate) current_tool: Option<String>,
}

impl AgentSession {
    /// 由快照链接建基线；阶段与当前工具留空。
    pub(crate) fn from_link(link: &AgentSessionLinkDto) -> Self {
        Self {
            child_session_id: link.child_session_id.clone(),
            tool_call_id: link.tool_call_id.clone(),
            agent_name: link.agent_name.clone(),
            task: link.task.clone(),
            status: link.status,
            final_session_id: link.final_session_id.clone(),
            summary: link.summary.clone(),
            error: link.error.clone(),
            phase: None,
            current_tool: None,
        }
    }
}

/// 这块是不是 agent 工具调用。
pub(crate) fn is_agent_tool(block: &ConversationBlockDto) -> bool {
    matches!(block, ConversationBlockDto::ToolCall { name, .. } if name == AGENT_TOOL_NAME)
}

/// 增量命中的子会话 id；每个 variant 的第一个字段都是它，查找时统一从这里取。
pub(crate) fn updated_child_session_id(update: &AgentSessionUpdateDto) -> &str {
    match update {
        AgentSessionUpdateDto::Spawned {
            child_session_id, ..
        }
        | AgentSessionUpdateDto::Completed {
            child_session_id, ..
        }
        | AgentSessionUpdateDto::Failed {
            child_session_id, ..
        }
        | AgentSessionUpdateDto::Progress {
            child_session_id, ..
        } => child_session_id,
    }
}

/// 归并一条增量；返回 `None` 表示这条增量不该改动任何东西。
///
/// `spawned` 是唯一的建项入口，而且总是整项覆盖（前端同此，重放同一次派生也应变回运行态）。
/// 其余三个 variant 都以当前项为准：当前项不存在（它可能是别处建过的）就整条丢掉，因此
/// 变体里带的那份 id 只用于查找，不再回写。
pub(crate) fn apply_update(
    current: Option<&AgentSession>,
    update: &AgentSessionUpdateDto,
) -> Option<AgentSession> {
    match update {
        AgentSessionUpdateDto::Spawned {
            child_session_id,
            tool_call_id,
            agent_name,
            task,
        } => Some(AgentSession {
            child_session_id: child_session_id.clone(),
            tool_call_id: tool_call_id.clone(),
            agent_name: agent_name.clone(),
            task: task.clone(),
            status: AgentSessionStatusDto::Running,
            final_session_id: None,
            summary: None,
            error: None,
            // 刚派生的子会话还没开口，前端也把它置于思考中。
            phase: Some(PhaseDto::Thinking),
            current_tool: None,
        }),
        AgentSessionUpdateDto::Completed {
            final_session_id,
            summary,
            ..
        } => {
            let mut next = current?.clone();
            next.status = AgentSessionStatusDto::Completed;
            next.final_session_id = Some(final_session_id.clone());
            next.summary = Some(summary.clone());
            next.error = None;
            next.phase = None;
            next.current_tool = None;
            Some(next)
        },
        AgentSessionUpdateDto::Failed {
            final_session_id,
            error,
            ..
        } => {
            let mut next = current?.clone();
            next.status = AgentSessionStatusDto::Failed;
            next.final_session_id = Some(final_session_id.clone());
            next.summary = None;
            next.error = Some(error.clone());
            next.phase = None;
            next.current_tool = None;
            Some(next)
        },
        AgentSessionUpdateDto::Progress {
            phase,
            current_tool,
            ..
        } => {
            let mut next = current?.clone();
            // 收尾之后的迟到进度不该把它推回运行态（前端同样只看运行中的项）。
            if next.status == AgentSessionStatusDto::Running {
                next.phase = Some(*phase);
                next.current_tool = current_tool.clone();
            }
            Some(next)
        },
    }
}

/// 工具行尾部的子会话状态。
///
/// 只有运行中的子会话才顶掉工具行原本的状态文字（前端 `streamingStatusText` 同序：
/// 待审批 → 待回答 → 子 Agent 运行中 → 耗时）。
pub(crate) fn runtime_label(session: Option<&AgentSession>) -> Option<String> {
    let session = session.filter(|session| session.status == AgentSessionStatusDto::Running)?;
    Some(match &session.current_tool {
        Some(tool) => format!("子Agent · {tool}"),
        None => "子Agent运行中".to_owned(),
    })
}

/// 卡片上的三态文字，对应前端 `AgentChildSessionPanel` 的 `status`。
pub(crate) fn status_label(status: AgentSessionStatusDto) -> &'static str {
    match status {
        AgentSessionStatusDto::Running => "运行中",
        AgentSessionStatusDto::Completed => "已完成",
        AgentSessionStatusDto::Failed => "失败",
    }
}

/// 子会话的阶段文字，对应前端 `AgentChildSessionPanel` 的 `PHASE_LABELS`。
///
/// 与 `views::chat` / `views::sidebar` 里那两份 `phase_label` 不同：那两个说的是控制态与
/// 会话列表行的语气（「思考中…」「空闲」），卡片这一处照前端用短标签。
pub(crate) fn phase_label(phase: PhaseDto) -> &'static str {
    match phase {
        PhaseDto::Idle => "就绪",
        PhaseDto::Thinking => "思考中",
        PhaseDto::Streaming => "生成中",
        PhaseDto::CallingTool => "调用工具",
        PhaseDto::Compacting => "压缩中",
        PhaseDto::Error => "错误",
    }
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::http::AgentSessionUpdateDto;

    use super::*;

    fn spawned() -> AgentSessionUpdateDto {
        AgentSessionUpdateDto::Spawned {
            child_session_id: "child-1".into(),
            tool_call_id: Some("call-1".into()),
            agent_name: "explorer".into(),
            task: "scan repo".into(),
        }
    }

    fn running() -> AgentSession {
        apply_update(None, &spawned()).expect("spawned 是建项入口")
    }

    #[test]
    fn spawning_starts_the_child_in_thinking() {
        let session = running();
        assert_eq!(session.status, AgentSessionStatusDto::Running);
        assert_eq!(session.phase, Some(PhaseDto::Thinking));
        assert_eq!(session.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(session.current_tool, None);
    }

    #[test]
    fn spawning_again_resets_a_finished_child() {
        let finished = apply_update(
            Some(&running()),
            &AgentSessionUpdateDto::Completed {
                child_session_id: "child-1".into(),
                final_session_id: "final-1".into(),
                summary: "done".into(),
            },
        )
        .expect("completed 需要当前项");

        let respawned = apply_update(Some(&finished), &spawned()).expect("spawned 是建项入口");
        assert_eq!(respawned.status, AgentSessionStatusDto::Running);
        assert_eq!(respawned.summary, None);
        assert_eq!(respawned.final_session_id, None);
    }

    #[test]
    fn terminal_and_progress_updates_need_an_existing_child() {
        for update in [
            AgentSessionUpdateDto::Completed {
                child_session_id: "child-1".into(),
                final_session_id: "final-1".into(),
                summary: "done".into(),
            },
            AgentSessionUpdateDto::Failed {
                child_session_id: "child-1".into(),
                final_session_id: "final-1".into(),
                error: "boom".into(),
            },
            AgentSessionUpdateDto::Progress {
                child_session_id: "child-1".into(),
                phase: PhaseDto::CallingTool,
                current_tool: Some("read".into()),
            },
        ] {
            assert!(apply_update(None, &update).is_none());
        }
    }

    #[test]
    fn failing_drops_the_summary_it_may_have_had() {
        let completed = apply_update(
            Some(&running()),
            &AgentSessionUpdateDto::Completed {
                child_session_id: "child-1".into(),
                final_session_id: "final-1".into(),
                summary: "done".into(),
            },
        )
        .expect("completed 需要当前项");
        assert_eq!(completed.summary.as_deref(), Some("done"));
        assert_eq!(completed.phase, None);

        let failed = apply_update(
            Some(&completed),
            &AgentSessionUpdateDto::Failed {
                child_session_id: "child-1".into(),
                final_session_id: "final-1".into(),
                error: "boom".into(),
            },
        )
        .expect("failed 需要当前项");
        assert_eq!(failed.status, AgentSessionStatusDto::Failed);
        assert_eq!(failed.summary, None);
        assert_eq!(failed.error.as_deref(), Some("boom"));
    }

    #[test]
    fn late_progress_does_not_revive_a_finished_child() {
        let finished = apply_update(
            Some(&running()),
            &AgentSessionUpdateDto::Failed {
                child_session_id: "child-1".into(),
                final_session_id: "final-1".into(),
                error: "boom".into(),
            },
        )
        .expect("failed 需要当前项");

        let progressed = apply_update(
            Some(&finished),
            &AgentSessionUpdateDto::Progress {
                child_session_id: "child-1".into(),
                phase: PhaseDto::CallingTool,
                current_tool: Some("read".into()),
            },
        )
        .expect("progress 需要当前项");
        assert_eq!(progressed, finished);
    }

    #[test]
    fn the_row_label_names_the_running_childs_tool() {
        let mut child = running();
        assert_eq!(
            runtime_label(Some(&child)).as_deref(),
            Some("子Agent运行中")
        );

        child.current_tool = Some("read".into());
        assert_eq!(
            runtime_label(Some(&child)).as_deref(),
            Some("子Agent · read")
        );
    }

    #[test]
    fn a_finished_child_leaves_the_row_label_alone() {
        let finished = apply_update(
            Some(&running()),
            &AgentSessionUpdateDto::Completed {
                child_session_id: "child-1".into(),
                final_session_id: "final-1".into(),
                summary: "done".into(),
            },
        )
        .expect("completed 需要当前项");

        assert_eq!(runtime_label(Some(&finished)), None);
        assert_eq!(runtime_label(None), None);
    }

    #[test]
    fn progress_lands_on_a_running_child() {
        let progressed = apply_update(
            Some(&running()),
            &AgentSessionUpdateDto::Progress {
                child_session_id: "child-1".into(),
                phase: PhaseDto::CallingTool,
                current_tool: Some("read".into()),
            },
        )
        .expect("progress 需要当前项");
        assert_eq!(progressed.phase, Some(PhaseDto::CallingTool));
        assert_eq!(progressed.current_tool.as_deref(), Some("read"));
    }
}
