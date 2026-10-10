//! Subagent DTOs lifted from root (`src/agents/{subagent_executor,
//! subagent_types}.rs` and
//! `src/async_exec/executor/registry.rs`).
//!
//! Phase 10e hoists the **shapes** AgentTool needs through its
//! `SubagentRuntime` port — the heavy `SubagentExecutor` itself
//! stays in root because it pulls in `AsyncExecutor`,
//! `Observability`, quota meters, and per-principal scope state
//! that aren't built-in-tool territory. The DTOs are pure data;
//! they can live alongside the tool.
//!
//! Sprint 8 Commit 4: the `AgentConfig` mirror DTO was deleted —
//! `SubagentRuntime::resolve_agent_config` now returns
//! `Arc<crate::agents::subagent_runtime_impl::AgentPrompt>` and
//! `SpawnRequest.subagent_config` carries the same. The workspace
//! Markdown is the single source of truth; `enable_*_tools` reads
//! were dropped in Commit 3.
//!
//! B3 (correctness, 2026-08-22): the `SpawnError` enum mirror was
//! deleted and the canonical `crate::agents::subagent_error::SpawnError`
//! is re-exported here instead. The two enums were 1:1 identical,
//! but the executor (`agents/subagent_executor.rs`) constructed the
//! root-side type while `AgentTool::format_error_response` downcast
//! the dto mirror — the downcast never matched in production, so
//! all six structured JSON error envelopes were test-only. The
//! re-export keeps every existing
//! `crate::tools::builtin::messaging::dto::SpawnError` import path
//! working while routing through the single canonical type.
//!
//! Root re-exports each type via `pub use crate::tools::builtin::messaging::...;`
//! so existing `crate::agents::agent_config::AgentConfig`,
//! `crate::agents::subagent_error::SpawnError`, and
//! `crate::agents::subagent_types::SubagentRunView` paths keep working.

// ─── SpawnError (re-exported from src/agents/subagent_error.rs) ───
//
// B3 (correctness, 2026-08-22): the dto mirror was deleted — the
// canonical root-side enum is the single source of truth. See the
// module-level doc above for the rationale. `format_error_response`
// downcasts this type via the same
// `crate::tools::builtin::messaging::dto::SpawnError` path so
// existing call sites and tests are unaffected by the unification.
pub use crate::agents::subagent_error::SpawnError;

// Session cleanup policy shared with the persistence layer.
pub use peko_session::SpawnCleanupPolicy;

// ─── ExecutionConfig (lifted from src/agents/subagent_executor.rs) ─

/// Configuration for subagent execution.
///
/// The historical `cleanup` / `label` knobs were dropped — every
/// caller always passed the default (`Keep` / `None`), and the
/// root-side `subagent_executor::ExecutionConfig` no longer carries
/// them either.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum execution time in seconds (0 = unlimited)
    pub timeout_seconds: u64,
    /// Phase 1 of `feature/multi-model-subagents`: optional
    /// catalog model id the parent picked for this spawn.
    /// Forwarded into `SpawnRequest.model` at the call site
    /// (`messaging/agent.rs::execute_spawn_blocking`). `None`
    /// means "inherit the parent's model".
    pub model_override: Option<String>,
    /// Retention cap on the session's closed compaction pages
    /// (ADR-051 FIFO page limit). `None` = unlimited. Applied by the
    /// executor to the session it creates or reseeds.
    pub page_limit: Option<u32>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            model_override: None,
            page_limit: None,
        }
    }
}

// ─── SubagentResult

/// Result of a subagent run.
#[derive(Debug, Clone)]
pub struct SubagentResult {
    /// Final status
    pub status: peko_tools_core::AsyncTaskStatus,
    /// Output content (if successful)
    pub output: Option<String>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Token usage (input, output, total)
    pub token_usage: Option<(usize, usize, usize)>,
    /// Completion timestamp
    pub completed_at: chrono::DateTime<chrono::Utc>,
}

// ─── SubagentRunView (lifted from src/agents/subagent_types.rs) ────

/// A read-only view of an async task entry, projected into the
/// subagent domain model. Constructed on demand by
/// `SubagentRunView::from_entry` (`agents::subagent_types`).
#[derive(Debug, Clone)]
pub struct SubagentRunView {
    pub run_id: String,
    pub child_session_key: String,
    /// The child's durable session id (UUID) — the form `session list`
    /// shows, Agent's `action = "resume"` consumes, and the metadata-
    /// chain diagnostic reads. `None` for legacy runs registered before
    /// this field existed. Spawn-registered runs store the overlay key
    /// in `child_session_key`; callers that need to chain a follow-up
    /// spawn against the same child should use THIS field as the new
    /// `parent_session_key`, not `child_session_key`.
    pub child_session_id: Option<String>,
    pub parent_session_key: String,
    pub task: String,
    pub status: peko_tools_core::AsyncTaskStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub cleanup: SpawnCleanupPolicy,
    pub label: Option<String>,
    pub result: Option<SubagentResult>,
    pub depth: u32,
}
