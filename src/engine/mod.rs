//! Root engine facade (Phase 9a + 9b.1 + 9b.N.1 + 9b.N.2 + 9b.N.3 + 9b.N.4 + 9b.N.5a shim).
//!
//! Phase 9a extracted the `src/engine/` files with zero root-only
//! dependencies into the `peko-engine` crate (chunker, event_processor,
//! state, stream_buffer, stream_orchestrator, tool_stream, parallel_gate,
//! error, events re-export). Phase 9b.1 followed up with `stream_types`
//! after lifting `ToolCallInfo` to `peko-message`. Phase 9b.N.1 lifted
//! `async_completion` after its two remaining imports
//! (`AsyncTaskStatus`, `CompletionEvent`) gained workspace-crate homes in
//! Phase 7/8. Phase 9b.N.2 lifted the F37 `execute_tool_via_core*`
//! helpers out of `tool_runtime.rs`; `ToolRuntime` itself remains in
//! root pending BashTool's lift into `peko-tools-builtin`. Phase
//! 9b.N.3 lifted `tool_executor.rs` after introducing two trait ports:
//! `ToolFunnel` (peko-extension-host) abstracts `ExtensionCore`'s
//! engine-facing surface, and `SessionView` (peko-engine) abstracts
//! the single `add_tool_result` write path. Phase 9b.N.4 lifts
//! `compaction_orchestrator.rs` after extending both trait ports
//! (`ToolFunnel` gains three hook-firing methods for the compaction /
//! session-state hooks; `SessionView` gains `record_compaction` /
//! `load_previous_compaction_summary` / `update_context_cache`) and
//! introducing a new `CompactorBackend` trait so the orchestrator
//! holds a `Box<dyn CompactorBackend>` instead of a concrete
//! `BackgroundCompactor`. Phase 9b.N.5a introduces the trait ports
//! `agentic_loop.rs` will consume in 9b.N.5b: `AgentView`
//! (peko-engine) abstracts `Agent`'s engine-facing surface,
//! `AsyncInboxLike` (peko-engine) abstracts `SharedSessionInbox`,
//! `CapabilityDiffTracker` lifts into `peko-engine::iteration_state`,
//! and `ToolFunnel` gains `invoke_stop_hook` /
//! `invoke_after_agent_hook`. The actual `agentic_loop.rs` lift is
//! Phase 9b.N.5b. This module re-exports their public surface
//! through `crate::engine::*` so downstream callers continue to
//! compile unchanged.
//!
//! The remaining root-coupled files stay in `src/engine/` until later
//! Phase 9b commits lift each residual coupling:
//!
//! - [`agentic_loop_compat`] — root-side test module for the lifted
//!   `peko_engine::AgenticLoop` (Phase 9b.N.5b.9). The actual production
//!   loop now lives in `peko_engine::agentic_loop`. Tests stay in root
//!   because they need root-only fixture types (`Agent`, `ExtensionCore`,
//!   `Subject`, `SessionManager`, etc.) that `peko-engine` cannot depend
//!   on. Mirrors the `tool_executor_compat` precedent.
//! - [`tool_runtime`] — `BuiltinToolAdapter` + concrete `tools::builtin::*`
//!   imports. Plan: move concrete tool impls and adapter into
//!   `peko-tools-core`, keep the synthetic stubs in `peko-engine`.

// Re-export everything from `peko-engine` for the moved files
// (Phase 9a pure subset + Phase 9b.1 stream_types + Phase 9b.N.1
// async_completion + Phase 9b.N.2 funnel + Phase 9b.N.3
// tool_executor + session_view + Phase 9b.N.4 compaction + Phase
// 9b.N.5a agent_view + async_inbox + iteration_state).
// Modules that still live in root (tool_runtime) are
// declared by `mod` below so the shim preserves every pre-Phase 9
// import path. The `agentic_loop` is now lifted into `peko-engine`
// (Phase 9b.N.5b.9); only the test module stays in root via
// `agentic_loop_compat`.
pub use peko_engine::{
    agentic_loop, async_completion, async_inbox, build_async_completion_message, chunker,
    compaction, default_process_stream, error, event_processor, events, execute_tool_via_core,
    execute_tool_via_core_with_context, execution, funnel, iteration_state, parallel_gate,
    parse_tool_calls_from_text, state, stream_buffer, stream_orchestrator, tool_executor,
    tool_stream, AgentState, AgentView, AgenticError, AgenticEvent, AgenticLoop, AgenticResult,
    AsyncInboxItem, AsyncInboxLike, BlockChunker, BreakPreference, CapabilityChange,
    CapabilityChangeKind, CapabilityDiff, CapabilityDiffTracker, ChannelAction, ChannelOutput,
    ChunkerConfig, CoalesceConfig, CoalescingChunker, CompactionConfig, CompactionEntry,
    CompactionQuota, CompactionRequest, CompactionResponse, CompactionResponseResult,
    CompactionResult, CompactionState, CompactorBackend, ContextUsageEstimate, DeliveryMode,
    EventProcessor, EventStream, ExecutionMode, LifecyclePhase, OrchestratorConfig,
    ProcessorConfig, SessionCore, SessionView, StateMachine, StreamBuffer, StreamOrchestrator,
    StreamingConfig, StreamingToolCall, TaskId, TaskStatus, TaskSummary, ToolCall,
    ToolCallParseError, ToolCallStreamParser, ToolExecutionResult, ToolExecutor,
};

pub mod agent_view_compat;
pub mod agentic_loop_compat;
pub mod async_inbox_compat;
pub mod background_compactor_factory_compat;
// `extension_core_funnel_compat` was removed in Phase 8a: the
// `impl ToolFunnel for ExtensionCore` lives next to ExtensionCore in
// `peko_extension_host::tool_funnel_impl`.
pub mod tool_runtime;
