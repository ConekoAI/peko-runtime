//! `peko-engine` — Peko agentic engine (Phase 9a + 9b.N.1 + 9b.N.2 + 9b.N.3).
//!
//! Phase 9a moves the **`src/engine/` files that have zero root-only
//! dependencies** into the `peko-engine` crate. Phase 9b.1 followed up
//! with `stream_types` after lifting `ToolCallInfo` to `peko-message`.
//! Phase 9b.N.1 then lifted `async_completion` after its two remaining
//! imports (`AsyncTaskStatus`, `CompletionEvent`) gained
//! workspace-crate homes in Phase 7 (peko-extension-api) and Phase 8
//! (peko-extension-host). Phase 9b.N.2 lifted the F37
//! `execute_tool_via_core*` funnel functions out of
//! `src/engine/tool_runtime.rs`; `ToolRuntime` itself remains in root
//! pending BashTool's lift into `peko-tools-builtin`. Phase 9b.N.3
//! lifts `tool_executor.rs` after introducing the `SessionView` trait
//! port — the executor only needs `add_tool_result(...)` against the
//! session, which the trait exposes via an `Arc<RwLock<Session>>` impl
//! in root (orphan rule).
//!
//! The remaining root-coupled files (`agentic_loop`,
//! `compaction_orchestrator`, `tool_runtime`) stay in `src/engine/`
//! until later Phase 9b commits lift the agent / session /
//! builtin-tools couplings (AgentView trait port, `ToolSearchTool`
//! synthetic shims, `BuiltinToolAdapter` → `peko-tools-core`).
//!
//! What lives here:
//!
//! | Module              | Phase responsibility |
//! |---------------------|-----------------------|
//! | [`async_completion`] | Phase 9b.N.1 — synthetic user-role `LlmMessage` builder for completed async tasks. |
//! | [`chunker`]         | Block-level text chunking (`BlockChunker`, `CoalescingChunker`). |
//! | [`event_processor`] | Channel-action state machine (`EventProcessor`, `ProcessorConfig`). |
//! | [`events`]          | Re-export of `peko_events` (`AgenticEvent`, `LifecyclePhase`). |
//! | [`execution`]       | Tool-execution primitives (`TaskId`, `ExecutionMode`, `TaskStatus`, `TaskSummary`). |
//! | [`error`]           | F31c `AgenticError` taxonomy. |
//! | [`funnel`]          | Phase 9b.N.2 — F37 canonical `execute_tool_via_core*` chokepoint. |
//! | [`parallel_gate`]   | F33 single-runtime RwLock gate for tool dispatch. |
//! | [`session_view`]    | Phase 9b.N.3 — narrow `add_tool_result(...)` trait port for tool dispatch. |
//! | [`state`]           | `AgentState` / `StateMachine` — atomic Idle/Busy tracker. |
//! | [`stream_buffer`]   | Coalescing buffer between orchestrator and channel. |
//! | [`stream_orchestrator`] | `StreamEvent` → `AgenticEvent` transformation. |
//! | [`stream_types`]    | Phase 9b.1 — public `ChannelOutput`/`EventStream`/`StreamingConfig` (`ToolCallInfo` lifted to `peko-message`). |
//! | [`tool_executor`]   | Phase 9b.N.3 — `ToolExecutor` for the agentic loop. |
//! | [`tool_stream`]     | Streaming tool-call text parser. |
//!
//! Phase 9a/9b slices are intentionally permissive: the crate boundary
//! is established one file at a time so each residual coupling can be
//! lifted incrementally with its own narrow PR.

pub mod async_completion;
pub mod chunker;
pub mod error;
pub mod event_processor;
pub mod events;
pub mod execution;
pub mod funnel;
pub mod parallel_gate;
pub mod session_view;
pub mod state;
pub mod stream_buffer;
pub mod stream_orchestrator;
pub mod stream_types;
pub mod tool_executor;
pub mod tool_stream;

// Convenience re-exports at the crate root. Mirrors the surface that
// `src/engine/mod.rs` exposed pre-Phase 9, so the root shim's
// `pub use peko_engine::*` preserves every downstream import path.
pub use async_completion::build_async_completion_message;
pub use chunker::{BlockChunker, BreakPreference, ChunkerConfig, CoalescingChunker};
pub use error::AgenticError;
pub use event_processor::{ChannelAction, EventProcessor, ProcessorConfig};
pub use events::{AgenticEvent, LifecyclePhase};
pub use execution::{ExecutionMode, TaskId, TaskStatus, TaskSummary};
pub use funnel::{execute_tool_via_core, execute_tool_via_core_with_context};
pub use session_view::{SessionCore, SessionView};
pub use state::{AgentState, StateMachine};
pub use stream_buffer::{CoalesceConfig, StreamBuffer};
pub use stream_orchestrator::{DeliveryMode, OrchestratorConfig, StreamOrchestrator};
pub use stream_types::{default_process_stream, ChannelOutput, EventStream, StreamingConfig};
pub use tool_executor::{ToolExecutionResult, ToolExecutor};
pub use tool_stream::{
    parse_tool_calls_from_text, StreamingToolCall, ToolCallParseError, ToolCallStreamParser,
};
