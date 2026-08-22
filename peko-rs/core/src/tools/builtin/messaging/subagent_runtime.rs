//! `SubagentRuntime` port trait — the surface `AgentTool` uses to
//! spawn a subagent.
//!
//! Per the Phase 10 plan rule ("Built-ins must not import daemon
//! state"), this trait is the only way `peko_tools_builtin::messaging`
//! reaches into the runtime. The root-side adapter
//! (`src/agents/subagent_runtime_impl.rs::SubagentExecutorRuntime`)
//! wraps `crate::agents::subagent_executor::SubagentExecutor` and
//! bridges every method.
//!
//! ## Methods
//!
//! - [`is_subagent_enabled`](SubagentRuntime::is_subagent_enabled) — capability
//!   gate. Returns `true` only when the per-principal capability snapshot
//!   grants `agent:<name>`; missing authorization context is denied.
//! - [`resolve_agent_config`](SubagentRuntime::resolve_agent_config) —
//!   disk lookup. Workspace-scoped first
//!   (`<workspace>/agents/<name>/AGENT.md` or `<workspace>/agents/<name>.md`),
//!   then global (`{PEKO_HOME}/agents/<name>/config.toml`). Adapter
//!   owns the `PathResolver` and `principal::agent_prompt` calls —
//!   built-ins never touch root internals.
//!
//! Sprint 8: parameter names that took a `subagent_type: &str` are
//! renamed to `agent: &str` to match the LLM-facing `AgentArgs::agent`
//! field. The method names (`is_subagent_enabled`,
//! `resolve_agent_config`) and the trait name (`SubagentRuntime`)
//! keep their historical "subagent" framing — a subagent is what gets
//! spawned, the `agent` value is its template name.
//! - [`audit_spawn`](SubagentRuntime::audit_spawn) — observability hub
//!   write. Adapter no-ops when no hub is attached.
//! - [`execute_and_wait`](SubagentRuntime::execute_and_wait) — the actual
//!   spawn. Builds `SubagentExecutor::execute_and_wait` from the lifted
//!   request shape; returns the projected `SubagentRunView`.
//! - [`request_compaction`](SubagentRuntime::request_compaction) — the
//!   `Agent` tool's `compact` action. Flags a session for engine-driven
//!   summarization at its next run; returns immediately (no LLM call,
//!   no completion signal).

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;

use async_trait::async_trait;

use crate::tools::builtin::messaging::dto::{
    AgentConfig, ExecutionConfig, SubagentRunView,
};
use crate::tools::builtin::session::CompactRequestOutcome;

/// Runtime port the `AgentTool` uses to talk to the subagent executor.
///
/// The production wiring implements this with
/// `SubagentExecutorRuntime` (root's
/// `src/agents/subagent_runtime_impl.rs`) which wraps
/// `Arc<SubagentExecutor>`. Tests substitute a `TestSubagentRuntime`
/// fixture provided in this module.
#[async_trait]
pub trait SubagentRuntime: Send + Sync {
    /// Capability check.
    ///
    /// Returns `true` only when the registered principal capability snapshot
    /// grants `agent:<agent>`. Missing context and missing grants are
    /// both denied.
    fn is_subagent_enabled(&self, agent: &str) -> bool;

    /// Resolve a subagent config from disk.
    ///
    /// Resolution order:
    /// 1. If `workspace` is `Some`, look up
    ///    `<workspace>/agents/<name>/AGENT.md` (directory layout) or
    ///    `<workspace>/agents/<name>.md` (flat layout).
    /// 2. Fall back to the global `{PEKO_HOME}/agents/<name>/config.toml`.
    ///
    /// `model_override` is accepted to preserve the current call shape
    /// but is currently a no-op — the runtime applies it at agent
    /// construction time via `Agent::init_provider`'s
    /// `provider_hint`, not by mutating the resolved config.
    async fn resolve_agent_config(
        &self,
        name: &str,
        workspace: Option<&Path>,
        model_override: Option<&str>,
    ) -> anyhow::Result<AgentConfig>;

    /// Audit a spawn event under the parent principal.
    ///
    /// Adapter no-ops when no observability hub is attached (the
    /// standalone / test path). Failures are logged but never bubble.
    async fn audit_spawn(&self, event: SpawnAuditEvent);

    /// Phase 3 of `feature/multi-model-subagents`: the
    /// conservative cost estimate (USD) computed at spawn time
    /// from the chosen model's `PricingHint` and a 4K-in +
    /// 1K-out token projection. Returns `None` when no
    /// pre-flight applies (no `cost_per_call_max` configured or
    /// no model pricing hint available). Default impl returns
    /// `None` so existing test stubs don't need to grow a new
    /// method.
    fn spawn_cost_estimate_usd(&self) -> Option<f64> {
        None
    }

    /// Execute a subagent spawn and wait for completion (or framework
    /// detach on timeout).
    async fn execute_and_wait(&self, request: SpawnRequest) -> anyhow::Result<SubagentRunView>;

    /// Flag a session for engine-driven compaction at its next run
    /// (the `Agent` tool's `action = "compact"`).
    ///
    /// Returns immediately after setting the persisted
    /// `compact_requested` flag — no LLM call, no completion signal.
    /// `caller_session_key` is the calling run's own session id; the
    /// ownership guards (target exists, not the caller's own session or
    /// an ancestor, inside the caller's subtree, not archived, no
    /// active run) live behind the port in
    /// `SubagentExecutor::request_compaction` and surface as structured
    /// anyhow errors.
    async fn request_compaction(
        &self,
        target: &str,
        caller_session_key: &str,
    ) -> anyhow::Result<CompactRequestOutcome>;

    /// The spawning principal's runtime id (DID). Used for the audit
    /// event's `principal_id` field. Defaults to empty so simple test
    /// fixtures don't have to override it; the production
    /// `SubagentExecutorRuntime` adapter returns the active principal.
    fn principal_id(&self) -> String {
        String::new()
    }

    /// The spawning principal's display name (for the audit row).
    /// Defaults to `None`.
    fn principal_name(&self) -> Option<String> {
        None
    }

    /// Maximum spawn depth the runtime enforces on incoming
    /// `ExecutionConfig.max_depth` calls. The production adapter
    /// reads this from the executor's principal config; test
    /// fixtures override to assert the tool projects the value
    /// onto the spawn request. Default `3` (the round-7
    /// historical cap).
    fn max_depth(&self) -> u32 {
        3
    }

    /// The spawning principal's workspace (the `<workspace>/agents/`
    /// resolution root). `None` means global agents only
    /// (standalone / test paths). Default `None`; production
    /// adapter overrides.
    fn workspace(&self) -> Option<&Path> {
        None
    }

    /// The calling run's own session id (the engine-side caller
    /// of the `Agent` tool). The tool reads this in the
    /// `Tool::execute` (no-`ToolContext`) path so non-engine
    /// callers (tests, async_executor) can still supply a
    /// session id. On the production `execute_with_context`
    /// path, `ctx.session_id` is preferred and this accessor is
    /// the fallback. Default `None`; production adapter
    /// overrides.
    fn session_id(&self) -> Option<String> {
        None
    }
}

/// Type alias for the shared runtime handle threaded through every
/// `AgentTool` constructor.
pub type SharedSubagentRuntime = Arc<dyn SubagentRuntime>;

// (Principal-id/principal-name accessors live as default methods on
// the `SubagentRuntime` trait itself so that a single `Arc<dyn
// SubagentRuntime>` exposes everything `AgentTool` needs to populate
// a `SpawnAuditEvent`. See the trait definition above.)

// ─── SpawnRequest (port input DTO) ────────────────────────────────

/// A request to execute a subagent spawn.
///
/// Mirrors the parameters of `SubagentExecutor::execute_and_wait` plus
/// the resolved `subagent_config` and the optional parent cancel
/// token. The adapter translates this into the executor call.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Task description / prompt for the subagent.
    pub prompt: String,
    /// Agent template name (passed for logging / observability
    /// only — `subagent_config` already carries the resolved name).
    pub agent: String,
    /// Parent session key.
    pub parent_session_key: String,
    /// Per-run execution config (timeout, cleanup, label, etc.).
    pub config: ExecutionConfig,
    /// Per-run timeout in seconds (the framework auto-detaches on
    /// timeout). Mirrors the `timeout_secs` parameter of
    /// `SubagentExecutor::execute_and_wait`.
    pub timeout_seconds: u64,
    /// Optional parent cancel token — wired by the tool from the
    /// parent `ToolContext::abort_signal` (F38).
    pub parent_cancel: Option<tokio_util::sync::CancellationToken>,
    /// The resolved subagent config (from
    /// [`SubagentRuntime::resolve_agent_config`]).
    pub subagent_config: AgentConfig,
    /// Phase 1 of `feature/multi-model-subagents`: optional
    /// catalog model id the parent picked for this spawn. When
    /// `Some`, the subagent dispatches its LLM calls against this
    /// model instead of inheriting the parent's model verbatim.
    /// `None` falls back to the parent's model (the historical
    /// behavior).
    pub model: Option<String>,
    /// Phase 5b: re-attach the run to an existing spawned session
    /// (persistent subagents) instead of spawning a fresh one.
    pub resume_session: Option<String>,
    /// The caller's own current session id (auto-detected by the
    /// tool from `ToolContext` / the session-key provider). Used by
    /// the adapter to ownership-validate an explicit
    /// `parent_session_key`; `None` skips that check (test paths).
    pub caller_session_key: Option<String>,
    /// Agent tool `name` param: slug for the child's session
    /// metadata (the per-parent-unique path segment for `/a/b`
    /// addressing). `None` leaves the child slugless. Ignored on the
    /// resume path (the session already exists — rename it via the
    /// session tool instead).
    pub name: Option<String>,
}

// ─── SpawnAuditEvent (port input DTO) ─────────────────────────────

/// A spawn event for the observability audit log.
///
/// Carries the structured details the production executor used to
/// log under `SubagentSpawn`. Tests no-op this path.
///
/// Sprint 7 Commit 3: `isolated`, `cleanup`, and `description` were
/// dropped — the LLM-facing `Agent` tool surface no longer exposes
/// these knobs, so they were always the same value on every audit
/// row. The audit JSON no longer carries `"isolated"`, `"cleanup"`,
/// or `"description"` keys; operators using `peko audit tail` will
/// see fewer fields per `SubagentSpawn` row.
#[derive(Debug, Clone, Serialize)]
pub struct SpawnAuditEvent {
    /// Agent template name (the value the LLM passed as `agent` on
    /// the `Agent` tool call).
    pub agent: String,
    /// The principal's runtime id (DID).
    pub principal_id: String,
    /// The principal's display name (for the audit row).
    pub principal_name: Option<String>,
    /// Parent session key.
    pub parent_session_key: String,
    /// Phase 1: catalog model id the parent picked for this
    /// spawn (when a non-default model was chosen). `None` means
    /// the child inherited the parent's model verbatim. Surfaced
    /// in the audit row under `model_id` so `peko audit tail`
    /// shows the parent-driven model choice.
    pub model_id: Option<String>,
    /// Phase 3 of `feature/multi-model-subagents`: the
    /// conservative cost estimate (USD) computed at spawn time
    /// from the chosen model's `PricingHint` and a 4K-in +
    /// 1K-out token projection. `None` when the principal has
    /// no `cost_per_call_max` configured (so no pre-flight ran)
    /// or when the model carries no pricing hint (local /
    /// unpriced model — cost is `0.0` by convention).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_estimate_usd: Option<f64>,
}
