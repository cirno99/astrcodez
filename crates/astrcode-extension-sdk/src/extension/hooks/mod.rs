//! Extension hook types: contexts, handlers, results, commands, and registration structs.

mod commands;
mod contexts;
mod handlers;
mod results;
mod types;

pub use commands::{
    CommandAvailability, CommandCompletionItem, CommandCompletions, CommandExecution,
    ExtensionCommandResult, SessionCommandKind, SlashCommand,
};
pub use contexts::{
    CommandCompletionContext, CommandContext, CommandDiscoveryContext, ContinueAfterStopContext,
    ContinueAfterStopPayload, HookContext, LifecycleContext, LifecyclePayload, PostCompactContext,
    PostCompactPayload, PostToolUseContext, PostToolUsePayload, PreCompactContext,
    PreCompactPayload, PreToolUseContext, PreToolUsePayload, PromptBuildContext,
    PromptBuildPayload, ProviderContext, ProviderPayload, ProviderRequestErrorContext,
    ProviderRequestErrorPayload, ProviderSettlementContext, ProviderSettlementPayload,
    ToolDiscoveryContext, UserMessageEnvelopeContext, UserMessageEnvelopePayload,
};
// `extension::internal` is the only public path to dispatcher-owned hook inputs.
pub use contexts::{
    HookInput, RuntimeContinueAfterStopContext, RuntimeHookCallContext, RuntimeLifecycleContext,
    RuntimePostCompactContext, RuntimePostToolUseContext, RuntimePreCompactContext,
    RuntimePreToolUseContext, RuntimePromptBuildContext, RuntimeProviderContext,
    RuntimeProviderRequestErrorContext, RuntimeProviderSettlementContext,
    RuntimeUserMessageEnvelopeContext,
};
pub use handlers::{
    CommandDiscovery, CommandDiscoveryHandler, CommandHandler, ContinueAfterStopHandler,
    DiscoveredCommand, DiscoveredTool, LifecycleHandler, PostCompactHandler, PostToolUseHandler,
    PreCompactHandler, PreToolUseHandler, PromptBuildHandler, ProviderContributionHandler,
    ProviderHandler, ProviderRequestErrorHandler, ToolDiscovery, ToolDiscoveryHandler, ToolHandler,
    ToolInputTransformHandler, UserMessageEnvelopeHandler,
};
pub use results::{
    ContinueAfterStopResult, HookResult, PostToolUseResult, PreCompactResult, PreToolUseAdmission,
    PreToolUseRequirement, PreToolUseResult, PreparedProviderContribution, PreparedProviderEffect,
    ProviderRequestErrorResult, ProviderResult, ToolInputTransformResult,
    UserMessageEnvelopeResult,
};
pub use types::{
    CompactContributions, CompactEvent, CompactRetainedContext, ContinueAfterStopLimit,
    ContinueAfterStopOptions, ContinueAfterStopRegistration, ExchangeSummary, ExtensionError,
    HookMode, MAX_PROVIDER_REQUEST_ERROR_RETRIES, PromptContributions, ProviderContributionId,
    ProviderEvent, ProviderRequestErrorKind, ProviderRequestErrorRegistration, ProviderRequestId,
    StatusItemUpdatePayload, ToolHookRegistration, ToolHookTarget, ToolUseRegistration,
    UserMessageEnvelopeRegistration,
};
