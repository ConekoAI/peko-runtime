//! Agentic loop, tool execution, prompt rendering, and compaction orchestration.
//! Host services implement the ports in `tooling`; inbox contracts are session-owned.

// Noise lints, consistent with the root crate's curated allow-list.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::should_implement_trait)]

pub mod agent_view;
pub mod agentic_loop;
pub mod async_completion;
pub mod async_inbox;
pub mod audit_sink;
pub mod chunker;
pub mod compaction_driver;
pub mod error;
pub mod event_processor;
pub mod events;
pub mod execution;
pub mod funnel;
pub mod parallel_gate;
pub mod prompt;
pub mod spec_gate;
pub mod stacked_metered_provider;
pub mod state;
pub mod stream_buffer;
pub mod stream_orchestrator;
pub mod stream_types;
pub mod synthetic_stream;
pub mod tool_executor;
pub mod tool_stream;

// Convenience re-exports at the crate root. Mirrors the surface that
// `src/engine/mod.rs` exposed pre-Phase 9, so the root shim's
// `pub use peko_engine::*` preserves every downstream import path.
pub use agent_view::AgentView;
pub use agentic_loop::{AgenticLoop, AgenticResult};
pub use async_completion::build_async_completion_message;
pub use async_inbox::{AsyncInboxItem, AsyncInboxLike};
pub use chunker::{BlockChunker, BreakPreference, ChunkerConfig, CoalescingChunker};
pub use compaction_driver::CompactionDriver;
// Phase 7 — the compaction data types + trait ports + eviction
// helper live in `peko-session` (the persistence-side owner) and
// are re-exported at this crate's root below.
pub use error::AgenticError;
pub use event_processor::{ChannelAction, EventProcessor, ProcessorConfig};
pub use events::{AgenticEvent, LifecyclePhase};
pub use execution::{ExecutionMode, TaskId, TaskStatus, TaskSummary};
pub use funnel::{execute_tool_via_core, execute_tool_via_core_with_context};
pub use peko_session::compaction::{
    drop_oldest_respecting_pairs, BackgroundCompactorFactory, CompactionConfig, CompactionEntry,
    CompactionLimitsState, CompactionQuota, CompactionRequest, CompactionResponse,
    CompactionResponseResult, CompactionResult, CompactionState, CompactorBackend,
    ContextUsageEstimate,
};
pub use prompt::renderer::{
    EmptyMcpPromptContextProvider, McpPromptContextProvider, PromptRenderer, RuntimeContextState,
};
pub use prompt::{
    builder::{PromptMode, SystemPromptBuilder},
    context::{IterationBudgetState, TurnPromptContext},
    memory::{
        directory_from_tool_params, discover_project_instructions, discover_shared_context,
        load_principal_memory, KB_DIR, PRINCIPAL_MEMORY_FILE, SHARED_CONTEXT_FILE,
    },
    placeholder::{replace_placeholders, Placeholder},
};
pub use spec_gate::{check as check_spec, SpecGateError};
// Phase 6 — `ProviderView` moved to `peko-providers` (next to `Provider`).
// The orphan rule forbids `impl ProviderView for Provider` from any
// crate other than `peko-providers`, so the trait + impl live together
// there. Engine keeps a thin re-export so the many `Arc<dyn ProviderView>`
// sites that pre-date Phase 6 keep compiling.
pub use peko_providers::ProviderView;
pub use peko_session::{SessionCore, SessionView};
pub use stacked_metered_provider::{compute_cost_usd, StackedMeteredProvider};
pub use state::{AgentState, StateMachine};
pub use stream_buffer::{CoalesceConfig, StreamBuffer};
pub use stream_orchestrator::{DeliveryMode, OrchestratorConfig, StreamOrchestrator};
pub use stream_types::{default_process_stream, ChannelOutput, EventStream, StreamingConfig};
pub use synthetic_stream::synthesize_stream_from_blocking;
pub use tool_executor::{ToolExecutionResult, ToolExecutor};
pub use tool_stream::{
    parse_tool_calls_from_text, StreamingToolCall, ToolCallParseError, ToolCallStreamParser,
};

pub mod tooling;
pub use tooling::{
    EngineHooks, PromptSectionRequest, PromptSections, ToolCallSpec, ToolFunnel, ToolingSeam,
};
