//! Root engine facade (Phase 9a shim).
//!
//! Phase 9a extracted the **`src/engine/` files that have zero root-only
//! dependencies** into the `peko-engine` crate (chunker, event_processor,
//! state, stream_buffer, stream_orchestrator, tool_stream, parallel_gate,
//! error, events re-export). This module re-exports their public surface
//! through `crate::engine::*` so downstream callers continue to compile
//! unchanged.
//!
//! The remaining root-coupled files stay in `src/engine/` until Phase 9b
//! lifts the agent / session / tools couplings:
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
//! - [`stream_types`] — `ChannelOutput` holds `Vec<ToolCallInfo>` from
//!   `agents::stateless_service`. Plan: lift `ToolCallInfo` into
//!   `peko-message` (it's a serializable DTO), then move `stream_types`
//!   into `peko-engine`.

// Re-export everything from `peko-engine` for the moved pure files.
// Modules that still live in root (agentic_loop, async_completion,
// compaction_orchestrator, stream_types, tool_executor, tool_runtime)
// are declared by `mod` below so the shim preserves every pre-Phase 9
// import path.
pub use peko_engine::{
    chunker, error, event_processor, events, execution, parallel_gate, parse_tool_calls_from_text,
    state, stream_buffer, stream_orchestrator, tool_stream, AgentState, AgenticError, AgenticEvent,
    BlockChunker, BreakPreference, ChannelAction, ChunkerConfig, CoalesceConfig, CoalescingChunker,
    DeliveryMode, EventProcessor, ExecutionMode, LifecyclePhase, OrchestratorConfig,
    ProcessorConfig, StateMachine, StreamBuffer, StreamOrchestrator, StreamingToolCall, TaskId,
    TaskStatus, TaskSummary, ToolCallParseError, ToolCallStreamParser,
};

pub mod agentic_loop;
pub mod async_completion;
pub mod compaction_orchestrator;
pub mod stream_types;
pub mod tool_executor;
pub mod tool_runtime;

pub use agentic_loop::{AgenticLoop, AgenticResult, ToolCall};
pub use stream_types::{default_process_stream, ChannelOutput, EventStream, StreamingConfig};
