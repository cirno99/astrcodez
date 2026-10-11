//! `provider_request_error` 的会话级行为：重试决策、尝试计数，以及两条明确不拦的失败路径。

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use astrcode_core::{
    event::DurableEventPayload,
    llm::{LlmError, LlmEvent, LlmProvider, LlmRequest, LlmTokenUsage, ModelLimits},
    types::{SessionId, new_session_id, new_turn_id},
};
use astrcode_extension_sdk::{
    extension::{
        ExtensionError, ProviderRequestErrorKind, ProviderRequestErrorResult,
        internal::RuntimeProviderRequestErrorContext,
    },
    runtime_ports::{NoopRuntimePorts, TurnHooks},
};
use astrcode_session::{Session, SessionCreateParams, SessionExtensionPorts, SessionRuntimeState};
use astrcode_storage::{EventReader, SessionStore, in_memory::InMemoryEventStore};
use tokio::sync::mpsc;

mod common;

/// hook 单次被调用时看到的事实。
#[derive(Debug, PartialEq, Eq)]
struct Consultation {
    model_id: String,
    attempt: u32,
    error_kind: ProviderRequestErrorKind,
    error_message: String,
    /// 决策时刻 durable 错误事件的数量：被 Retry 的尝试不得留下错误记录。
    durable_errors_before_decision: usize,
}

struct ScriptedHooks {
    store: Arc<InMemoryEventStore>,
    decisions: Mutex<Vec<ProviderRequestErrorResult>>,
    consultations: Mutex<Vec<Consultation>>,
}

impl ScriptedHooks {
    fn new(
        store: Arc<InMemoryEventStore>,
        decisions: Vec<ProviderRequestErrorResult>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            decisions: Mutex::new(decisions),
            consultations: Mutex::new(Vec::new()),
        })
    }

    fn consultations(&self) -> Vec<Consultation> {
        std::mem::take(&mut *self.consultations.lock().unwrap())
    }
}

#[async_trait::async_trait]
impl TurnHooks for ScriptedHooks {
    async fn emit_provider_request_error(
        &self,
        ctx: RuntimeProviderRequestErrorContext,
    ) -> Result<ProviderRequestErrorResult, ExtensionError> {
        let events = self
            .store
            .replay_events(ctx.call().session_id())
            .await
            .map_err(|error| ExtensionError::Internal(error.to_string()))?;
        let durable_errors_before_decision = events
            .iter()
            .filter(|event| matches!(event.payload, DurableEventPayload::ErrorOccurred { .. }))
            .count();
        self.consultations.lock().unwrap().push(Consultation {
            model_id: ctx.model_id().to_owned(),
            attempt: ctx.attempt(),
            error_kind: ctx.error_kind(),
            error_message: ctx.error_message().to_owned(),
            durable_errors_before_decision,
        });
        Ok(self
            .decisions
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(ProviderRequestErrorResult::Fail))
    }
}

fn retry(reason: &str) -> ProviderRequestErrorResult {
    ProviderRequestErrorResult::Retry {
        delay_ms: Some(0),
        reason: reason.into(),
    }
}

/// 前 `failures` 次请求以 429 失败，之后成功。
struct RateLimitedLlm {
    failures: usize,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for RateLimitedLlm {
    async fn generate_request(
        &self,
        _request: LlmRequest,
    ) -> Result<mpsc::UnboundedReceiver<LlmEvent>, LlmError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.failures {
            return Err(LlmError::RateLimited {
                status: 429,
                retry_after_ms: None,
                message: "slow down".into(),
            });
        }
        Ok(successful_stream())
    }

    fn model_limits(&self) -> ModelLimits {
        test_model_limits()
    }
}

struct OverflowLlm {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for OverflowLlm {
    async fn generate_request(
        &self,
        _request: LlmRequest,
    ) -> Result<mpsc::UnboundedReceiver<LlmEvent>, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(LlmError::ContextWindowExceeded {
            message: "injected overflow".into(),
        })
    }

    fn model_limits(&self) -> ModelLimits {
        test_model_limits()
    }
}

/// 连接已建立、流中途断开。
struct MidStreamFailureLlm {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for MidStreamFailureLlm {
    async fn generate_request(
        &self,
        _request: LlmRequest,
    ) -> Result<mpsc::UnboundedReceiver<LlmEvent>, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(LlmEvent::ContentDelta {
            delta: "partial".into(),
        })
        .unwrap();
        tx.send(LlmEvent::Error {
            message: "connection reset".into(),
        })
        .unwrap();
        Ok(rx)
    }

    fn model_limits(&self) -> ModelLimits {
        test_model_limits()
    }
}

fn successful_stream() -> mpsc::UnboundedReceiver<LlmEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    tx.send(LlmEvent::Usage {
        usage: LlmTokenUsage {
            input_tokens: Some(10),
            cached_input_tokens: None,
            cache_creation_input_tokens: None,
            input_accounting: None,
            output_tokens: Some(2),
            reasoning_output_tokens: None,
            total_tokens: Some(12),
            source: None,
        },
    })
    .unwrap();
    tx.send(LlmEvent::ContentDelta { delta: "ok".into() })
        .unwrap();
    tx.send(LlmEvent::Done {
        finish_reason: "stop".into(),
    })
    .unwrap();
    rx
}

fn test_model_limits() -> ModelLimits {
    ModelLimits {
        max_input_tokens: 200_000,
        max_output_tokens: 4096,
    }
}

async fn spawn_session(
    store: Arc<InMemoryEventStore>,
    llm: Arc<dyn LlmProvider>,
    hooks: Arc<ScriptedHooks>,
) -> Session {
    let noop = Arc::new(NoopRuntimePorts);
    let extension_ports =
        SessionExtensionPorts::from_immutable_ports(noop.clone(), noop.clone(), hooks, noop);
    let services = common::test_runtime_services_with_extensions(llm, extension_ports);
    let session_id = new_session_id();
    let store_port: Arc<dyn SessionStore> = store;
    let runtime = Arc::new(SessionRuntimeState::new(session_id.clone(), store_port));
    let working_dir = std::env::temp_dir().join(session_id.as_str());
    std::fs::create_dir_all(&working_dir).unwrap();
    Session::create_with_params(SessionCreateParams {
        working_dir: working_dir.to_string_lossy().into_owned(),
        model_id: "mock-model".into(),
        parent_session_id: None,
        tool_selection: None,
        source_extension: None,
        extra_system_prompt: None,
        initial_system_prompt: None,
        runtime,
        runtime_services: services,
    })
    .await
    .unwrap()
}

async fn durable_error_count(store: &InMemoryEventStore, session_id: &SessionId) -> usize {
    store
        .replay_events(session_id)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.payload, DurableEventPayload::ErrorOccurred { .. }))
        .count()
}

#[tokio::test]
async fn retry_decision_reissues_the_request_without_recording_the_failed_attempt() {
    let store = Arc::new(InMemoryEventStore::new());
    let hooks = ScriptedHooks::new(Arc::clone(&store), vec![retry("again"), retry("once more")]);
    let llm = Arc::new(RateLimitedLlm {
        failures: 2,
        calls: AtomicUsize::new(0),
    });
    let session = spawn_session(Arc::clone(&store), llm.clone(), Arc::clone(&hooks)).await;
    let session_id = session.id().clone();

    let result = session
        .submit("hello".into(), new_turn_id(), None)
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();

    assert!(result.output.is_ok(), "{:?}", result.output);
    assert_eq!(llm.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        hooks.consultations(),
        vec![
            Consultation {
                model_id: "mock-model".into(),
                attempt: 1,
                error_kind: ProviderRequestErrorKind::RateLimit,
                error_message: "rate limited (429): slow down".into(),
                durable_errors_before_decision: 0,
            },
            Consultation {
                model_id: "mock-model".into(),
                attempt: 2,
                error_kind: ProviderRequestErrorKind::RateLimit,
                error_message: "rate limited (429): slow down".into(),
                durable_errors_before_decision: 0,
            },
        ]
    );
    assert_eq!(
        durable_error_count(&store, &session_id).await,
        0,
        "a retried attempt must not leave a durable error event"
    );
}

#[tokio::test]
async fn fail_decision_keeps_the_provider_failure() {
    let store = Arc::new(InMemoryEventStore::new());
    let hooks = ScriptedHooks::new(Arc::clone(&store), vec![ProviderRequestErrorResult::Fail]);
    let llm = Arc::new(RateLimitedLlm {
        failures: usize::MAX,
        calls: AtomicUsize::new(0),
    });
    let session = spawn_session(Arc::clone(&store), llm.clone(), Arc::clone(&hooks)).await;
    let session_id = session.id().clone();

    let result = session
        .submit("hello".into(), new_turn_id(), None)
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();

    assert!(result.output.is_err());
    assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.consultations().len(), 1);
    assert_eq!(durable_error_count(&store, &session_id).await, 1);
}

#[tokio::test]
async fn context_window_exceeded_is_not_offered_to_the_hook() {
    let store = Arc::new(InMemoryEventStore::new());
    let hooks = ScriptedHooks::new(Arc::clone(&store), vec![retry("again"); 8]);
    let llm = Arc::new(OverflowLlm {
        calls: AtomicUsize::new(0),
    });
    let session = spawn_session(Arc::clone(&store), llm, Arc::clone(&hooks)).await;

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        session
            .submit("overflow".into(), new_turn_id(), None)
            .await
            .unwrap()
            .wait(),
    )
    .await
    .expect("reactive compaction should terminate")
    .unwrap();

    assert!(result.output.is_err());
    assert!(
        hooks.consultations().is_empty(),
        "ContextWindowExceeded has its own recovery path and must not reach the hook"
    );
}

#[tokio::test]
async fn stream_failure_is_not_offered_to_the_hook() {
    let store = Arc::new(InMemoryEventStore::new());
    let hooks = ScriptedHooks::new(Arc::clone(&store), vec![retry("again")]);
    let llm = Arc::new(MidStreamFailureLlm {
        calls: AtomicUsize::new(0),
    });
    let session = spawn_session(Arc::clone(&store), llm.clone(), Arc::clone(&hooks)).await;
    let session_id = session.id().clone();

    let result = session
        .submit("hello".into(), new_turn_id(), None)
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();

    assert!(result.output.is_err());
    assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    assert!(
        hooks.consultations().is_empty(),
        "a mid-stream failure already published transcript events; replaying them is not allowed"
    );
    assert_eq!(durable_error_count(&store, &session_id).await, 1);
}
