//! `CallerAwareSessionTool` — the `session` builtin with per-call
//! caller resolution (ADR-061 caller-awareness).
//!
//! The stock `SessionTool` reads the caller from a construction-time
//! cell (`SessionManagerRuntime.current_session_id`, written at run
//! start). On the agentic-loop path that cell IS the running session,
//! so classification is correct; on the `ExecuteTool` (workflow) path
//! the cell is stale — the registering agent's *last* run — and
//! pre-boot principals have no `session` tool registered at all.
//!
//! This tool resolves the caller **per call** from
//! `ToolContext.session_id` (the token-resolved node id on the
//! `ExecuteTool` path; the live run id on the agent path):
//!
//! - [`Self::for_daemon`] — daemon-global registration (system scope,
//!   next to `ModelCall`/`Workflow`). Per call: `ctx.principal_name`
//!   → `PrincipalManager` → the principal's sessions dir +
//!   `InboxRegistry` + quota meter; a fresh `SessionManagerRuntime`
//!   whose cell is seeded from `ctx.session_id`. Tokenless / no-node
//!   calls thread the session-key string into the cell → the session
//!   layer's existing `caller_context` classifies them dangling
//!   (fail-closed — no new privilege logic).
//! - [`Self::for_agent`] — per-agent registration (drop-in for the
//!   stock `SessionTool`). When `ctx.session_id` is present it
//!   overrides via `SessionManagerRuntime::with_current_session`;
//!   when absent (in-process dispatches without session ctx) it
//!   delegates to the shared runtime — byte-identical to the stock
//!   tool on the agentic-loop path, where the ctx id and the cell
//!   content are the same run UUID.
//!
//! Neither mode changes the `SessionRuntime` port trait, and the
//! per-agent construction keeps its exact behavior. Capability gating
//! stays `tool:session`; the registered name stays `session`.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use serde_json::Value;

use peko_session::InboxRegistry;
use peko_tools_core::{Tool, ToolContext, ToolError};

use crate::principal::Principal;
use crate::session::session_runtime_impl::SessionManagerRuntime;
use crate::tools::builtin::session::{SessionCache, SessionTool, SharedSessionRuntime};

/// How the caller's session store + identity are resolved per call.
enum SessionCallerMode {
    /// Daemon-global: everything resolved from `ToolContext` +
    /// `PrincipalManager` per call.
    Daemon {
        principals: Weak<crate::principal::manager::PrincipalManager>,
        inbox_registry: Arc<InboxRegistry>,
    },
    /// Per-agent: per-call override over the agent's shared runtime;
    /// ctx-less calls fall back to the shared cell (stock behavior).
    Agent { runtime: SessionManagerRuntime },
}

/// `session` builtin with per-call caller resolution. See module docs.
pub struct CallerAwareSessionTool {
    mode: SessionCallerMode,
    /// Static-surface delegate: the tool's name/description/schema are
    /// identical to the stock `SessionTool`'s; this instance exists
    /// only to serve them (never executes).
    metadata_tool: SessionTool,
}

impl CallerAwareSessionTool {
    /// Daemon-global registration (ADR-061 `ExecuteTool` path).
    /// `inbox_registry` is the daemon-shared registry the run-permit
    /// guards acquire through.
    #[must_use]
    pub fn for_daemon(
        principals: Weak<crate::principal::manager::PrincipalManager>,
        inbox_registry: Arc<InboxRegistry>,
    ) -> Self {
        Self {
            mode: SessionCallerMode::Daemon {
                principals,
                inbox_registry,
            },
            metadata_tool: metadata_tool(),
        }
    }

    /// Per-agent registration — a drop-in for the stock
    /// `SessionTool::new(runtime)` construction. Behavior on the
    /// agentic-loop path is identical (the ctx-carried id IS the run
    /// id the shared cell already holds); ctx-less dispatches delegate
    /// to the shared runtime unchanged.
    #[must_use]
    pub fn for_agent(runtime: SessionManagerRuntime) -> Self {
        let metadata_tool = SessionTool::new(Arc::new(runtime.clone()) as SharedSessionRuntime);
        Self {
            mode: SessionCallerMode::Agent { runtime },
            metadata_tool,
        }
    }

    /// Daemon-mode caller resolution: principal from
    /// `ctx.principal_name`, fail-closed — same shape as
    /// `ModelCallTool`/`WorkflowTool` (an unattributed session op is
    /// never allowed).
    async fn resolve_principal(
        principals: &Weak<crate::principal::manager::PrincipalManager>,
        ctx: &ToolContext,
    ) -> anyhow::Result<Arc<Principal>> {
        let name = ctx
            .principal_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "session: requires a calling-principal context \
                     (ToolContext.principal_name is unset)"
                )
            })?;
        let manager = principals.upgrade().ok_or_else(|| {
            anyhow::anyhow!("session: PrincipalManager unavailable on this runtime")
        })?;
        manager
            .get_by_name(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("session: unknown principal '{name}'"))
    }

    /// Build a fresh per-call `SessionTool` over the principal's store
    /// with the caller cell seeded from `ctx.session_id` (the
    /// token-resolved node id, or the session-key string when no node
    /// is known — the session layer classifies the latter dangling).
    async fn per_call_tool(
        principal: &Principal,
        inbox_registry: &Arc<InboxRegistry>,
        session_id: Option<String>,
    ) -> SessionTool {
        let name = principal.name().await;
        let sessions_dir = principal.memory.sessions_dir().clone();
        let session_manager = peko_session::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir)
            .with_agent_name(name.as_str());
        let runtime = SessionManagerRuntime::new(
            Arc::new(tokio::sync::RwLock::new(session_manager)),
            Arc::new(tokio::sync::RwLock::new(session_id)),
            name,
            Some(Arc::clone(inbox_registry)),
            Some(Arc::clone(&principal.quota_meter)),
        );
        SessionTool::new(Arc::new(runtime) as SharedSessionRuntime)
    }
}

impl std::fmt::Debug for CallerAwareSessionTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallerAwareSessionTool")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for CallerAwareSessionTool {
    fn name(&self) -> &'static str {
        // Must match the stock tool's registered name — the registry
        // probe and the `tool:session` capability grant key off it.
        "session"
    }

    fn description(&self) -> String {
        self.metadata_tool.description()
    }

    fn parameters(&self) -> Value {
        self.metadata_tool.parameters()
    }

    fn parallelizable(&self) -> bool {
        // Read-mostly ops with per-call state; the stock tool uses the
        // trait default (true) — match it.
        true
    }

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        Err(ToolError::Other(
            "session requires a ToolContext (caller resolution); \
             invoke it through the extension funnel, not Tool::execute"
                .to_string(),
        )
        .into())
    }

    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }
        match &self.mode {
            SessionCallerMode::Daemon {
                principals,
                inbox_registry,
            } => {
                let principal = Self::resolve_principal(principals, ctx).await?;
                let tool = Self::per_call_tool(
                    &principal,
                    inbox_registry,
                    ctx.session_id.clone().filter(|s| !s.is_empty()),
                )
                .await;
                tool.execute(params).await
            }
            SessionCallerMode::Agent { runtime } => {
                match ctx.session_id.clone().filter(|s| !s.is_empty()) {
                    // Per-call override: identical content to the cell
                    // during a run; the token-resolved node on the
                    // ExecuteTool path.
                    Some(id) => {
                        let tool =
                            SessionTool::new(Arc::new(runtime.with_current_session(Some(id)))
                                as SharedSessionRuntime);
                        tool.execute(params).await
                    }
                    // In-process dispatches without session ctx
                    // (AsyncSpawn-internal, some cron paths): the
                    // stock tool's shared-cell behavior, unchanged.
                    None => self.metadata_tool.execute(params).await,
                }
            }
        }
    }
}

/// Stock-tool static surface for the daemon mode (a `SessionCache`
/// runtime is never executed — description/schema only).
fn metadata_tool() -> SessionTool {
    SessionTool::new(Arc::new(SessionCache::new("session")) as SharedSessionRuntime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Seed a tempdir store with trunk + child (slugs t/c); returns the
    /// runtime with the caller cell pre-seeded plus the canonical ids.
    async fn seeded_runtime(
        cell: Option<String>,
    ) -> (tempfile::TempDir, SessionManagerRuntime, String, String) {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut manager = peko_session::SessionManager::new()
            .with_sessions_dir_internal(temp.path())
            .with_agent_name("wf-agent");
        let peer = peko_subject::Subject::User("alice".to_string());
        manager
            .create_session(
                "wf-agent",
                &peer,
                peko_session::SessionCreateOptions::new().with_session_id("trunk"),
            )
            .await
            .expect("trunk");
        let trunk = peko_session::SessionId::from("trunk").to_string();
        manager
            .create_session(
                "wf-agent",
                &peer,
                peko_session::SessionCreateOptions::new()
                    .with_session_id("child")
                    .with_parent(trunk.clone()),
            )
            .await
            .expect("child");
        let child = peko_session::SessionId::from("child").to_string();
        let runtime = SessionManagerRuntime::new(
            Arc::new(tokio::sync::RwLock::new(manager)),
            Arc::new(tokio::sync::RwLock::new(cell)),
            "wf-agent".to_string(),
            None,
            None,
        );
        (temp, runtime, trunk, child)
    }

    fn ctx_with_session(id: Option<&str>) -> ToolContext {
        let ctx = ToolContext::default_for_tool("session");
        match id {
            Some(id) => ctx.with_session_id(id),
            None => ctx,
        }
    }

    /// Agent mode: `ctx.session_id` (the ExecuteTool token-resolved
    /// node) overrides the shared cell per call; without ctx the shared
    /// cell answers — stock behavior.
    #[tokio::test]
    async fn agent_mode_ctx_override_wins_and_absent_ctx_uses_cell() {
        let (_t, runtime, trunk, child) =
            seeded_runtime(Some(peko_session::SessionId::from("child").to_string())).await;
        let tool = CallerAwareSessionTool::for_agent(runtime);

        let out = tool
            .execute_with_context(json!({"action": "status"}), &ctx_with_session(Some(&trunk)))
            .await
            .expect("status");
        assert_eq!(out["session_id"], json!(trunk), "ctx override must win");

        let out = tool
            .execute_with_context(json!({"action": "status"}), &ctx_with_session(None))
            .await
            .expect("status");
        assert_eq!(out["session_id"], json!(child), "no ctx → shared cell");
    }

    /// Agent mode with no session ctx is byte-identical to the stock
    /// `SessionTool` (the loop-path invariant).
    #[tokio::test]
    async fn agent_mode_without_ctx_is_identical_to_stock_tool() {
        let (_t, runtime, _trunk, child) =
            seeded_runtime(Some(peko_session::SessionId::from("child").to_string())).await;
        let aware = CallerAwareSessionTool::for_agent(runtime.clone());
        let stock = SessionTool::new(Arc::new(runtime) as SharedSessionRuntime);
        let ctx = ctx_with_session(None);

        let a = aware
            .execute_with_context(json!({"action": "list"}), &ctx)
            .await
            .expect("list");
        let b = stock
            .execute(json!({"action": "list"}))
            .await
            .expect("list");
        assert_eq!(a, b, "ctx-less agent-mode calls must match the stock tool");

        // And on the loop path (ctx == cell) results are identical too.
        let a = aware
            .execute_with_context(json!({"action": "list"}), &ctx_with_session(Some(&child)))
            .await
            .expect("list");
        assert_eq!(a, b, "loop-path ctx (== cell) must match the stock tool");
    }

    /// Daemon mode fails closed without a resolvable principal.
    #[tokio::test]
    async fn daemon_mode_fails_closed_without_principal() {
        let tool = CallerAwareSessionTool::for_daemon(
            Weak::new(),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        );
        let ctx = ToolContext::default_for_tool("session").with_principal_name("ghost");
        let err = tool
            .execute_with_context(json!({"action": "list"}), &ctx)
            .await
            .expect_err("dangling Weak must error");
        assert!(
            format!("{err:#}").contains("PrincipalManager"),
            "got: {err:#}"
        );

        let err = tool
            .execute(json!({"action": "list"}))
            .await
            .expect_err("bare execute must refuse");
        assert!(format!("{err:#}").contains("ToolContext"), "got: {err:#}");
    }

    /// Static surface matches the stock tool: name, schema, description.
    #[tokio::test]
    async fn static_surface_matches_stock_tool() {
        let (_t, runtime, _trunk, _child) =
            seeded_runtime(Some(peko_session::SessionId::from("child").to_string())).await;
        let aware = CallerAwareSessionTool::for_agent(runtime.clone());
        let stock = SessionTool::new(Arc::new(runtime) as SharedSessionRuntime);
        assert_eq!(aware.name(), stock.name());
        assert_eq!(aware.parameters(), stock.parameters());
        assert_eq!(aware.description(), stock.description());

        let daemon = CallerAwareSessionTool::for_daemon(
            Weak::new(),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        );
        assert_eq!(daemon.name(), "session");
        assert_eq!(daemon.parameters(), stock.parameters());
    }
}
