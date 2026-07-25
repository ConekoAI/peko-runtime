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
//!   gate. Returns `true` when the per-principal capability snapshot
//!   grants `agent:<name>` (or when no snapshot is registered — the
//!   fail-open standalone/test path).
//! - [`resolve_agent_config`](SubagentRuntime::resolve_agent_config) —
//!   disk lookup. Workspace-scoped first
//!   (`<workspace>/agents/<name>/AGENT.md` or `<workspace>/agents/<name>.md`),
//!   then global (`{PEKO_HOME}/agents/<name>/config.toml`). Adapter
//!   owns the `PathResolver` and `principal::agent_prompt` calls —
//!   built-ins never touch root internals.
//! - [`audit_spawn`](SubagentRuntime::audit_spawn) — observability hub
//!   write. Adapter no-ops when no hub is attached.
//! - [`execute_and_wait`](SubagentRuntime::execute_and_wait) — the actual
//!   spawn. Builds `SubagentExecutor::execute_and_wait` from the lifted
//!   request shape; returns the projected `SubagentRunView`.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::messaging::dto::{AgentConfig, ExecutionConfig, SpawnCleanupPolicy, SubagentRunView};

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
    /// Returns `true` when:
    /// - the executor has no principal capability snapshot registered
    ///   (standalone / test path — fail-open per existing behavior), or
    /// - the snapshot grants `agent:<subagent_type>`.
    ///
    /// Returns `false` when the snapshot is registered and does not
    /// grant the requested subagent.
    fn is_subagent_enabled(&self, subagent_type: &str) -> bool;

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

    /// Execute a subagent spawn and wait for completion (or framework
    /// detach on timeout).
    async fn execute_and_wait(&self, request: SpawnRequest) -> anyhow::Result<SubagentRunView>;

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
    /// Subagent type identifier (passed for logging / observability
    /// only — `subagent_config` already carries the resolved name).
    pub subagent_type: String,
    /// Whether to create an isolated session without parent context.
    pub isolated: bool,
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
}

// ─── SpawnAuditEvent (port input DTO) ─────────────────────────────

/// A spawn event for the observability audit log.
///
/// Carries the structured details the production executor used to
/// log under `SubagentSpawn`. Tests no-op this path.
#[derive(Debug, Clone)]
pub struct SpawnAuditEvent {
    /// Subagent type identifier.
    pub subagent_type: String,
    /// The principal's runtime id (DID).
    pub principal_id: String,
    /// The principal's display name (for the audit row).
    pub principal_name: Option<String>,
    /// Whether the spawn was isolated (fresh session).
    pub isolated: bool,
    /// Whether the spawn should keep or delete the session on
    /// completion. Serialised as `"keep"` / `"delete"`.
    pub cleanup: SpawnCleanupPolicy,
    /// Optional description label.
    pub description: Option<String>,
    /// Parent session key.
    pub parent_session_key: String,
}
