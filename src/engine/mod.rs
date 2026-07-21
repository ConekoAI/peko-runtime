//! Root engine facade (Phase 9a + 9b.1 shim).
//!
//! Phase 9a extracted the `src/engine/` files with zero root-only
//! dependencies into the `peko-engine` crate (chunker, event_processor,
//! state, stream_buffer, stream_orchestrator, tool_stream, parallel_gate,
//! error, events re-export). Phase 9b.1 followed up with `stream_types`
//! after lifting `ToolCallInfo` to `peko-message`. This module
//! re-exports their public surface through `crate::engine::*` so
//! downstream callers continue to compile unchanged.
//!
//! The remaining root-coupled files stay in `src/engine/` until later
//! Phase 9b commits lift each residual coupling:
//!
//! - [`agentic_loop`] — `Arc<Agent>`, `CapabilityDiffTracker`,
//!   `ToolSearchTool` synthetic shims, `agents::prompt::*` types.
//!   Plan: introduce `AgentView` trait in `peko-extension-host` and lift
//!   the small renderer state types (`PromptRenderer`,
//!   `TurnPromptContext`, `IterationBudgetState`, `QuotaStateView`,
//!   `CapabilityDiffTracker`, `HOOK_TIMEOUT`,
//!   `load_principal_memory`) into `peko-engine`.
//! - [`compaction_orchestrator`] — embeds root's `BackgroundCompactor`,
//!   holds `Arc<RwLock<Session>>`. Plan: lift
//!   `session::compaction::*` into `peko-engine` and introduce
//!   `SessionView` trait for the session write path.
//! - [`tool_executor`] — `&Arc<RwLock<Session>>` parameter. Plan:
//!   `SessionView` trait.
//! - [`tool_runtime`] — `BuiltinToolAdapter` + concrete `tools::builtin::*`
//!   imports. Plan: move concrete tool impls and adapter into
//!   `peko-tools-core`, keep the synthetic stubs in `peko-engine`.
//! - [`async_completion`] — `extensions::framework::async_exec::executor::*`
//!   re-export. Plan: clean up after the framework moves in a follow-up
//!   bulk-move PR (per [[workspace-phase8-commit2-scope-down]]).

// Re-export everything from `peko-engine` for the moved files
// (Phase 9a pure subset + Phase 9b.1 stream_types). Modules that still
// live in root (agentic_loop, async_completion, compaction_orchestrator,
// tool_executor, tool_runtime) are declared by `mod` below so the shim
// preserves every pre-Phase 9 import path.
pub use peko_engine::{
    ChannelOutput, EventStream, StreamingConfig, chunker, default_process_stream, error,
    event_processor, events, execution, parallel_gate, parse_tool_calls_from_text, state,
    stream_buffer, stream_orchestrator, tool_stream, AgentState, AgenticError, AgenticEvent,
    BlockChunker, BreakPreference, ChannelAction, ChunkerConfig, CoalesceConfig,
    CoalescingChunker, DeliveryMode, EventProcessor, ExecutionMode, LifecyclePhase,
    OrchestratorConfig, ProcessorConfig, StateMachine, StreamBuffer, StreamOrchestrator,
    StreamingToolCall, TaskId, TaskStatus, TaskSummary, ToolCallParseError, ToolCallStreamParser,
};

pub mod agentic_loop;
pub mod async_completion;
pub mod compaction_orchestrator;
pub mod tool_executor;
pub mod tool_runtime;

pub use agentic_loop::{AgenticLoop, AgenticResult, ToolCall};
