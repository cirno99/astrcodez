//! 钩子实现：续跑决策、工具活动观测、人工插话重置。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use astrcode_extension_sdk::{
    extension::{
        ContinueAfterStopContext, ContinueAfterStopHandler, ContinueAfterStopResult, ExtensionCall,
        ExtensionError, HookResult, LifecycleContext, LifecycleHandler, PostToolUseContext,
        PostToolUseHandler, PostToolUseResult,
    },
    text::truncate_bytes_head,
    wire::host::{HostSessionInputRequest, HostWorkspaceReadOutput, HostWorkspaceReadRequest},
};

use crate::{
    plan::{self, Decision, Observation},
    prompt::{self, MAX_TASK_FILE_BYTES, RoundPrompt},
    state::{LoopStatus, LoopStore, StopReason, loops_dir_from_base},
};

/// 跨钩子的运行期观测：本 step 是否出现过工具调用。
///
/// `post_tool_use` 写入、续跑判定读取并消费，因此这个标志衡量的正是
/// 「自上次判定以来这一轮续跑有没有干活」。
#[derive(Default)]
pub(crate) struct RalphRuntime {
    tool_activity: Mutex<HashMap<String, bool>>,
}

impl RalphRuntime {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn note_tool_call(&self, session_id: &str) {
        self.lock().insert(session_id.to_owned(), true);
    }

    fn take_tool_activity(&self, session_id: &str) -> bool {
        self.lock().remove(session_id).unwrap_or(false)
    }

    fn reset_activity(&self, session_id: &str) {
        self.lock().remove(session_id);
    }

    // 锁只在上面几个同步方法里持有，持锁期间不会 panic，因此中毒分支不可达；
    // 仍然显式取回内部值，避免用 unwrap 把不可达路径写成 panic。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, bool>> {
        self.tool_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn store_for(call: &impl ExtensionCall) -> Result<LoopStore, ExtensionError> {
    let base = call.paths().require_session_data_dir()?;
    Ok(LoopStore::new(loops_dir_from_base(base)))
}

/// 读取任务文件，返回（正文, 是否截断）。
///
/// 读不到时返回错误而不是空正文：空正文会让模型以为清单本来就是空的，
/// 从而「什么都没做就宣布完成」——这正是 Completion Gate 要防的失败模式。
async fn read_task_file(call: &impl ExtensionCall, path: &str) -> Result<(String, bool), String> {
    let output = call
        .host()
        .workspace()
        .map_err(|error| format!("打开工作区失败：{error}"))?
        .read(HostWorkspaceReadRequest::new(path))
        .await
        .map_err(|error| format!("读取 {path} 失败：{error}"))?;
    let HostWorkspaceReadOutput::Text { content, .. } = output else {
        return Err(format!("{path} 不是文本文件"));
    };
    let truncated = content.len() > MAX_TASK_FILE_BYTES;
    Ok((
        truncate_bytes_head(&content, MAX_TASK_FILE_BYTES).to_owned(),
        truncated,
    ))
}

pub(crate) struct RalphContinueAfterStopHandler {
    runtime: Arc<RalphRuntime>,
}

impl RalphContinueAfterStopHandler {
    pub(crate) fn new(runtime: Arc<RalphRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait::async_trait]
impl ContinueAfterStopHandler for RalphContinueAfterStopHandler {
    async fn handle(
        &self,
        ctx: ContinueAfterStopContext,
    ) -> Result<ContinueAfterStopResult, ExtensionError> {
        let store = store_for(&ctx)?;
        let Some(mut state) = store.load_current().map_err(ExtensionError::Internal)? else {
            return Ok(ContinueAfterStopResult::EndTurn);
        };
        if !state.status.allows_advance() {
            return Ok(ContinueAfterStopResult::EndTurn);
        }

        let observation = Observation {
            assistant_text: ctx.assistant_text(),
            had_tool_call: self.runtime.take_tool_activity(ctx.session_id().as_ref()),
        };
        let Decision::Continue = plan::advance(&mut state, &observation) else {
            store.save(&state).map_err(ExtensionError::Internal)?;
            return Ok(ContinueAfterStopResult::EndTurn);
        };

        let (task_body, truncated) = match read_task_file(&ctx, &state.task_file).await {
            Ok(read) => read,
            Err(detail) => {
                state.set_status(LoopStatus::Stopped, Some(StopReason::TaskFileUnreadable));
                store.save(&state).map_err(ExtensionError::Internal)?;
                tracing::warn!(session_id = %ctx.session_id(), %detail, "Ralph：停止循环");
                return Ok(ContinueAfterStopResult::EndTurn);
            },
        };

        let text = prompt::render(&RoundPrompt {
            iteration: state.iteration,
            max_iterations: state.max_iterations,
            task_file: &state.task_file,
            task_body: &task_body,
            completion_promise: state.completion_promise.as_deref(),
            truncated,
        });

        let delivery = ctx
            .host()
            .session_control()?
            .defer_context(HostSessionInputRequest {
                target_session_id: ctx.session_id().to_string(),
                content: text,
            })
            .await;

        match delivery {
            Ok(_) => {
                store.save(&state).map_err(ExtensionError::Internal)?;
                Ok(ContinueAfterStopResult::ContinueOneStep)
            },
            Err(error) => {
                // handler 返回 Err 会被宿主当成 turn 失败，所以注入失败只能记进状态再停。
                state.set_status(LoopStatus::Stopped, Some(StopReason::InjectFailed));
                store.save(&state).map_err(ExtensionError::Internal)?;
                tracing::warn!(session_id = %ctx.session_id(), %error, "Ralph：注入失败，停止循环");
                Ok(ContinueAfterStopResult::EndTurn)
            },
        }
    }
}

pub(crate) struct RalphToolActivityHandler {
    runtime: Arc<RalphRuntime>,
}

impl RalphToolActivityHandler {
    pub(crate) fn new(runtime: Arc<RalphRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait::async_trait]
impl PostToolUseHandler for RalphToolActivityHandler {
    async fn handle(&self, ctx: PostToolUseContext) -> Result<PostToolUseResult, ExtensionError> {
        self.runtime.note_tool_call(ctx.session_id().as_ref());
        Ok(PostToolUseResult::Allow)
    }
}

pub(crate) struct RalphUserPromptHandler {
    runtime: Arc<RalphRuntime>,
}

impl RalphUserPromptHandler {
    pub(crate) fn new(runtime: Arc<RalphRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait::async_trait]
impl LifecycleHandler for RalphUserPromptHandler {
    async fn handle(&self, ctx: LifecycleContext) -> Result<HookResult, ExtensionError> {
        let session_id = ctx.session_id().to_string();
        // 扩展自己注入的提示走 turn 内吸收，不会派发这个事件，所以这里只可能是真人说话。
        self.runtime.reset_activity(&session_id);

        let store = store_for(&ctx)?;
        match store.load_current() {
            Ok(Some(mut state)) if state.status.allows_advance() => {
                state.reset_breakers();
                if let Err(error) = store.save(&state) {
                    tracing::warn!(session_id = %session_id, %error, "Ralph：重置熔断链失败");
                }
            },
            Ok(_) => {},
            Err(error) => {
                tracing::warn!(session_id = %session_id, %error, "Ralph：读取循环状态失败");
            },
        }
        Ok(HookResult::Allow)
    }
}
