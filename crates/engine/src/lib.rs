//! `peko-engine` — Peko agentic engine (Phase 9a).
//!
//! Phase 9a moves the **`src/engine/` files that have zero root-only
//! dependencies** into the `peko-engine` crate. The remaining root-coupled
//! files (`agentic_loop`, `compaction_orchestrator`, `tool_executor`,
//! `tool_runtime`, `async_completion`, `stream_types`) stay in
//! `src/engine/` until Phase 9b lifts the agent / session / tool-coupling
//! edges (AgentView / SessionView trait ports, `ToolCallInfo` →
//! `peko-message`, `ToolSearchTool` synthetic shims, `BuiltinToolAdapter`
//! → `peko-tools-core`).
//!
//! What lives here:
//!
//! | Module              | Phase 9a responsibility |
//! |---------------------|-------------------------|
//! | [`chunker`]         | Block-level text chunking (`BlockChunker`, `CoalescingChunker`). |
//! | [`event_processor`] | Channel-action state machine (`EventProcessor`, `ProcessorConfig`). |
//! | [`execution`]       | Tool-execution primitives (`TaskId`, `ExecutionMode`, `TaskStatus`, `TaskSummary`). |
//! | [`parallel_gate`]   | F33 single-runtime RwLock gate for tool dispatch. |
//! | [`state`]           | `AgentState` / `StateMachine` — atomic Idle/Busy tracker. |
//! | [`stream_buffer`]   | Coalescing buffer between orchestrator and channel. |
//! | [`stream_orchestrator`] | `StreamEvent` → `AgenticEvent` transformation. |
//! | [`tool_stream`]     | Streaming tool-call text parser. |
//! | [`error`]           | F31c `AgenticError` taxonomy. |
//! | [`events`]          | Re-export of `peko_events` (`AgenticEvent`, `LifecyclePhase`). |
//!
//! Phase 9a is intentionally permissive: the crate boundary is established
//! for these pure files first, so Phase 9b follow-ups can lift each
//! residual coupling incrementally.

pub mod chunker;
pub mod error;
pub mod event_processor;
pub mod events;
pub mod execution;
pub mod parallel_gate;
pub mod state;
pub mod stream_buffer;
pub mod stream_orchestrator;
pub mod stream_types;
pub mod tool_stream;

// Convenience re-exports at the crate root. Mirrors the surface that
// `src/engine/mod.rs` exposed pre-Phase 9, so the root shim's
// `pub use peko_engine::*` preserves every downstream import path.
pub use chunker::{BlockChunker, BreakPreference, ChunkerConfig, CoalescingChunker};
pub use error::AgenticError;
pub use event_processor::{ChannelAction, EventProcessor, ProcessorConfig};
pub use events::{AgenticEvent, LifecyclePhase};
pub use execution::{ExecutionMode, TaskId, TaskStatus, TaskSummary};
pub use state::{AgentState, StateMachine};
pub use stream_buffer::{CoalesceConfig, StreamBuffer};
pub use stream_orchestrator::{DeliveryMode, OrchestratorConfig, StreamOrchestrator};
pub use stream_types::{ChannelOutput, EventStream, StreamingConfig, default_process_stream};
pub use tool_stream::{
    parse_tool_calls_from_text, StreamingToolCall, ToolCallParseError, ToolCallStreamParser,
};
