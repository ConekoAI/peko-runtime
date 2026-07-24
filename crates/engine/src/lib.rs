//! `peko-engine` — Peko agentic engine (Phase 9a + 9b.N.1 + 9b.N.2 + 9b.N.3 + 9b.N.4).
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
//! in root (orphan rule). Phase 9b.N.4 lifts `compaction_orchestrator.rs`
//! after introducing the `CompactorBackend` trait port — the orchestrator
//! only needs the dual-threshold check + oneshot submit pattern, which
//! the trait exposes via the root-owned `BackgroundCompactor` impl.
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
//! | [`agentic_loop`]    | Phase 9b.N.5b.9 — the lifted agentic loop (`AgenticLoop`, `AgenticResult`). |
//! | [`async_completion`] | Phase 9b.N.1 — synthetic user-role `LlmMessage` builder for completed async tasks. |
//! | [`chunker`]         | Block-level text chunking (`BlockChunker`, `CoalescingChunker`). |
//! | [`compaction`]      | Phase 9b.N.4 — `CompactorBackend` trait port + data types lifted from `src/session/compaction.rs`. |
//! | [`event_processor`] | Channel-action state machine (`EventProcessor`, `ProcessorConfig`). |
//! | [`events`]          | Re-export of `peko_events` (`AgenticEvent`, `LifecyclePhase`). |
//! | [`execution`]       | Tool-execution primitives (`TaskId`, `ExecutionMode`, `TaskStatus`, `TaskSummary`). |
//! | [`error`]           | F31c `AgenticError` taxonomy. |
//! | [`funnel`]          | Phase 9b.N.2 — F37 canonical `execute_tool_via_core*` chokepoint. |
//! | [`parallel_gate`]   | F33 single-runtime RwLock gate for tool dispatch. |
//! | [`prompt`]          | Phase 9b.N.5b.4 — `PromptRenderer` + `TurnPromptContext` + placeholder/memory helpers lifted from `src/agents/prompt/`. |
//! | [`session_view`]    | Phase 9b.N.3 — narrow `add_tool_result(...)` trait port for tool dispatch. |
//! | [`state`]           | `AgentState` / `StateMachine` — atomic Idle/Busy tracker. |
//! | [`stacked_metered_provider`] | Phase 9b.N.5b.8 — `StackedMeteredProvider` lifted from `src/providers/stacked_metered.rs` (refactored to wrap `Arc<dyn ProviderView>`). |
//! | [`stream_buffer`]   | Coalescing buffer between orchestrator and channel. |
//! | [`stream_orchestrator`] | `StreamEvent` → `AgenticEvent` transformation. |
//! | [`stream_types`]    | Phase 9b.1 — public `ChannelOutput`/`EventStream`/`StreamingConfig` (`ToolCallInfo` lifted to `peko-message`). |
//! | [`synthetic_stream`] | Phase 9b.N.5b.5 — `synthesize_stream_from_blocking` lifted from `src/providers/synthetic_stream.rs`. |
//! | [`tool_executor`]   | Phase 9b.N.3 — `ToolExecutor` for the agentic loop. |
//! | [`tool_stream`]     | Streaming tool-call text parser. |
//!
//! Phase 9a/9b slices are intentionally permissive: the crate boundary
//! is established one file at a time so each residual coupling can be
//! lifted incrementally with its own narrow PR.

pub mod agent_view;
pub mod agentic_loop;
pub mod async_completion;
pub mod async_inbox;
pub mod chunker;
pub mod compaction;
pub mod compaction_orchestrator;
pub mod error;
pub mod event_processor;
pub mod events;
pub mod execution;
pub mod funnel;
pub mod iteration_state;
pub mod parallel_gate;
pub mod prompt;
pub mod session_view;
pub mod stacked_metered_provider;
pub mod state;
pub mod stream_buffer;
pub mod stream_orchestrator;
pub mod stream_types;
pub mod synthetic_stream;
pub mod tool_executor;
pub mod tool_search_metadata;
pub mod tool_stream;

// Convenience re-exports at the crate root. Mirrors the surface that
// `src/engine/mod.rs` exposed pre-Phase 9, so the root shim's
// `pub use peko_engine::*` preserves every downstream import path.
pub use agent_view::AgentView;
pub use agentic_loop::{AgenticLoop, AgenticResult, ToolCall};
pub use async_completion::build_async_completion_message;
pub use async_inbox::{AsyncInboxItem, AsyncInboxLike};
pub use chunker::{BlockChunker, BreakPreference, ChunkerConfig, CoalescingChunker};
pub use compaction_orchestrator::CompactionOrchestrator;
// Phase 7 — the compaction data types + trait ports + eviction
// helper live in `peko-session` (the persistence-side owner).
// Re-export through `peko_engine::compaction::{...}` so the
// pre-Phase-7 import paths (`peko_engine::compaction::CompactionConfig`,
// etc.) keep compiling. The legacy
// `crates/engine/src/compaction/types.rs` / `backend.rs` / `factory.rs`
// / `eviction.rs` are deleted in Phase 7.2 once their last consumers
// migrate.
pub use error::AgenticError;
pub use event_processor::{ChannelAction, EventProcessor, ProcessorConfig};
pub use events::{AgenticEvent, LifecyclePhase};
pub use execution::{ExecutionMode, TaskId, TaskStatus, TaskSummary};
pub use funnel::{execute_tool_via_core, execute_tool_via_core_with_context};
pub use iteration_state::{
    CapabilityChange, CapabilityChangeKind, CapabilityDiff, CapabilityDiffTracker,
};
pub use peko_session::compaction::{
    drop_oldest_respecting_pairs, BackgroundCompactorFactory, CompactionConfig, CompactionEntry,
    CompactionQuota, CompactionRequest, CompactionResponse, CompactionResponseResult,
    CompactionResult, CompactionState, CompactorBackend, ContextUsageEstimate,
};
pub use prompt::renderer::PromptRenderer;
pub use prompt::{
    builder::{PromptMode, SystemPromptBuilder},
    context::{IterationBudgetState, QuotaStateView, TurnPromptContext},
    memory::{
        directory_from_tool_params, discover_shared_context, load_principal_memory,
        PRINCIPAL_MEMORY_FILE, SHARED_CONTEXT_FILE,
    },
    placeholder::{replace_placeholders, Placeholder},
};
// Phase 6 — `ProviderView` moved to `peko-providers` (next to `Provider`).
// The orphan rule forbids `impl ProviderView for Provider` from any
// crate other than `peko-providers`, so the trait + impl live together
// there. Engine keeps a thin re-export so the many `Arc<dyn ProviderView>`
// sites that pre-date Phase 6 keep compiling.
pub use peko_providers::ProviderView;
pub use session_view::{SessionCore, SessionView};
pub use stacked_metered_provider::StackedMeteredProvider;
pub use state::{AgentState, StateMachine};
pub use stream_buffer::{CoalesceConfig, StreamBuffer};
pub use stream_orchestrator::{DeliveryMode, OrchestratorConfig, StreamOrchestrator};
pub use stream_types::{default_process_stream, ChannelOutput, EventStream, StreamingConfig};
pub use synthetic_stream::synthesize_stream_from_blocking;
pub use tool_executor::{ToolExecutionResult, ToolExecutor};
pub use tool_search_metadata::{
    synthetic_description, synthetic_parameters, TOOL_SEARCH_DEFAULT_LIMIT, TOOL_SEARCH_TOOL_NAME,
};
pub use tool_stream::{
    parse_tool_calls_from_text, StreamingToolCall, ToolCallParseError, ToolCallStreamParser,
};
