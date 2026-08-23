//! Subagent DTOs lifted from root (`src/agents/{subagent_executor,
//! subagent_types}.rs` and
//! `src/extensions/framework/async_exec/executor/registry.rs`).
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

use serde::{Deserialize, Serialize};

// ─── SpawnError (re-exported from src/agents/subagent_error.rs) ───
//
// B3 (correctness, 2026-08-22): the dto mirror was deleted — the
// canonical root-side enum is the single source of truth. See the
// module-level doc above for the rationale. `format_error_response`
// downcasts this type via the same
// `crate::tools::builtin::messaging::dto::SpawnError` path so
// existing call sites and tests are unaffected by the unification.
pub use crate::agents::subagent_error::SpawnError;

// ─── SpawnCleanupPolicy (re-export of peko_extension_api) ─────────
//
// The enum's canonical home moved from `peko_extension_host` to
// `peko_extension_api` in Phase 8b to break the cycle that arose
// when the host crate grew a `peko_tools_builtin` dep
// (`async_exec/executor/async_runtime_impl.rs` adapts the host's
// `AsyncExecutor` to the `AsyncRuntime` port). We re-export it here
// so consumers of the messaging module can refer to one place;
// root's `crate::tools::builtin::session::types::SpawnCleanupPolicy` shim is
// preserved for backwards compat.
pub use peko_extension_api::SpawnCleanupPolicy;

// ─── ExecutionConfig (lifted from src/agents/subagent_executor.rs) ─

/// Configuration for subagent execution.
///
/// Sprint 7 Commit 3: `cleanup` and `label` were dropped — every
/// caller always passed the default (`Keep` / `None`). The
/// projection onto root-side `subagent_executor::ExecutionConfig`
/// in `agents/subagent_runtime_impl.rs` now hardcodes the default
/// for those two. B4 cleanup: `format_announcement` was deleted —
/// `cleanup` / `label` are now dead in the root-side type too, but
/// retained for backward-compat reads of legacy JSON config.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum execution time in seconds (0 = unlimited)
    pub timeout_seconds: u64,
    /// Whether to announce completion to parent
    pub announce_completion: bool,
    /// Maximum spawn depth (0 = unlimited)
    pub max_depth: u32,
    /// Phase 1 of `feature/multi-model-subagents`: optional
    /// catalog model id the parent picked for this spawn.
    /// Forwarded into `SpawnRequest.model` at the call site
    /// (`messaging/agent.rs::execute_spawn_blocking`). `None`
    /// means "inherit the parent's model".
    pub model_override: Option<String>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            announce_completion: true,
            max_depth: 1,
            model_override: None,
        }
    }
}

// ─── SubagentResult (lifted from src/extensions/framework/async_exec/executor/registry.rs)

/// Result of a subagent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentResult {
    /// Final status
    pub status: peko_extension_api::AsyncTaskStatus,
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
/// subagent domain model.
///
/// The `from_entry` projection method stayed in root because it
/// references `AsyncTaskEntry` / `TaskMetadata` — root-only types.
#[derive(Debug, Clone)]
pub struct SubagentRunView {
    pub run_id: String,
    pub child_session_key: String,
    pub parent_session_key: String,
    pub task: String,
    pub status: peko_extension_api::AsyncTaskStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub cleanup: SpawnCleanupPolicy,
    pub label: Option<String>,
    pub result: Option<SubagentResult>,
    pub depth: u32,
    pub announce_completion: bool,
}
