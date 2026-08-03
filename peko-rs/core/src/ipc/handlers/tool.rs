//! `tool` domain request handler (F6 step 3 / F8 completion).
//!
//! Owns the daemon-side async tool execution IPC variants:
//! `AsyncSpawn`, `AsyncCancel`. The handler holds a narrow [`ToolHost`]
//! port; the daemon-side implementation (`AppState`) is reached only
//! through the trait, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::tool`) defines
//!   the [`ToolHost`] trait; the producer (`daemon::state`) implements
//!   it (same pattern as `SystemHost` and `AuthHost`).
//! - F6: this module must not import any other `ipc::handlers::*` module.
//!
//! Security (ADR-042 + F8 invariant): capability grants passed to
//! `ToolRuntime::execute_tool_with_workspace` are **always** derived
//! server-side from the session's owning Principal. They are never
//! accepted from the IPC packet itself — that would be privilege
//! escalation. The resolution chain is:
//!
//!   `parse_session_key()` → `principal_manager.get_by_name(parts.agent)`
//!   → `Principal.capabilities().to_strings()` (+ active extensions
//!   from the daemon-side `ExtensionStore`).
//!
//! If the principal cannot be resolved (e.g. an unknown session key
//! raced the principal reload, or the daemon hasn't finished warming
//! up), the handler **fails closed** — it falls back to `None, None`,
//! matching the system-wide invariant that an empty grant set denies
//! capability-gated tools (`engine::tool_runtime`,
//! `framework::core::tool_registry`, `agents::agent`,
//! `engine::agentic_loop` all treat `None`/empty as deny-all). The
//! `*_fail_closed_without_principal_id` gate tests pin this contract.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::warn;

use crate::engine::tool_runtime::ToolRuntime;
use peko_observability::Observability;
use crate::extensions::framework::async_exec::executor::{
    AsyncExecutor, AsyncTaskId, AsyncToolConfig,
};
use crate::extensions::framework::store::ExtensionStore;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;
use peko_auth::caller::CallerContext;
use peko_session::key::parse_session_key;

/// Narrow port the `tool` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. All methods are sync: they
/// return cheap references / `Arc` clones, so the trait is trivially
/// object-safe and the handler pays no `async_trait` overhead. The
/// per-request grant resolution (the `async` part of the F8 invariant)
/// lives in the handler and uses these accessors to do its own awaits.
pub(crate) trait ToolHost: Send + Sync {
    /// Principal manager used to resolve the session's owning
    /// principal (ADR-042).
    fn principal_manager(&self) -> &Arc<PrincipalManager>;

    /// Extension store used to source the principal's active extension
    /// IDs for capability-gated tools.
    fn extension_store(&self) -> &Arc<ExtensionStore>;

    /// Async tool runtime used to actually execute the spawned task.
    fn tool_runtime(&self) -> Arc<ToolRuntime>;

    /// Async task executor that owns the spawned task's lifecycle
    /// (cancellation, completion delivery).
    fn async_task_executor(&self) -> Arc<AsyncExecutor>;

    /// Observability facade — used to stamp `service_token.use_denied`
    /// Security audit rows when the caller's service-token cap set
    /// strips a principal-granted capability (ADR-045 PR #6 step 3).
    fn observability(&self) -> Arc<Observability>;
}

/// `tool` domain request handler. Constructed with an `Arc<dyn ToolHost>`
/// (typically `Arc::new(app_state.clone())` from the dispatcher).
pub(crate) struct ToolHandler {
    host: Arc<dyn ToolHost>,
}

impl ToolHandler {
    pub(crate) fn new(host: Arc<dyn ToolHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ToolHandler {
    fn domain(&self) -> &'static str {
        "tool"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::AsyncSpawn { .. } | RequestPacket::AsyncCancel { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::AsyncSpawn {
                request_id,
                tool_name,
                params,
                session_key,
                workspace,
            } => {
                self.handle_async_spawn(
                    request_id,
                    tool_name,
                    params,
                    session_key,
                    workspace,
                    sink,
                    caller,
                )
                .await?;
            }

            RequestPacket::AsyncCancel {
                request_id,
                task_id,
            } => {
                self.handle_async_cancel(request_id, task_id, sink).await?;
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ToolHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

impl ToolHandler {
    /// Spawn an async tool task.
    ///
    /// Capability grants are derived server-side from the session's
    /// owning Principal (ADR-042). See the module-level doc for the
    /// resolution chain. If the principal cannot be resolved, we fall
    /// back to `None, None` (fail-closed) and log a warning so the
    /// mismatch is visible in tracing without leaking data to the
    /// caller.
    async fn handle_async_spawn(
        &self,
        request_id: u64,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
        sink: &dyn ResponseSink,
        caller: &CallerContext,
    ) -> anyhow::Result<()> {
        let parts = parse_session_key(&session_key);
        let principal_agent = parts.agent;

        // Resolve the owning principal. The principal name is the
        // session's `agent` segment per ADR-042; `principal_manager`
        // never returns a stale entry — it holds `Arc<Principal>` and
        // reloads are coordinated by `PrincipalManager` itself.
        //
        // From the resolved principal we derive BOTH:
        //   - the granted capability set (`capabilities().to_strings()`)
        //   - the principal's active extension set, built by
        //     `ExtensionCatalog::build` from the principal's
        //     capabilities + `agent_prompts` and the daemon-wide
        //     `ExtensionStore::global_items()`. This is the same path
        //     `PrincipalManager::receive` uses when an agent session
        //     boots, so capability-gated tools see the identical
        //     enable set whether they were spawned by chat traffic or
        //     by this IPC path.
        //
        // On resolution failure we fall back to `None, None` (deny-all)
        // and warn — preserving the fail-closed invariant pinned by
        // the `*_fail_closed_without_principal_id` gate tests.
        let principal = self
            .host
            .principal_manager()
            .get_by_name(principal_agent)
            .await;

        let (resolved_capabilities, resolved_active_extensions) = match principal {
            Some(principal) => {
                let caps = principal.capabilities().await;
                let global_items = self.host.extension_store().global_items().await;
                let catalog = crate::principal::extension_store::ExtensionCatalog::build(
                    &caps,
                    &principal.agent_prompts,
                    &global_items,
                );
                (
                    Some(caps.to_strings()),
                    Some(catalog.active_extensions().to_vec()),
                )
            }
            None => {
                warn!(
                    "AsyncSpawn session_key={} resolved to unknown principal '{}'; \
                     falling back to fail-closed (no grants)",
                    session_key, principal_agent,
                );
                (None, None)
            }
        };

        // PR #6 step 3: intersect the principal's resolved caps with the
        // caller's service-token caps. Local/interactive callers
        // (`token_caps.is_empty()`) bypass entirely — preserves ADR-045's
        // "agents running enabled capabilities/extensions are
        // automatically approved for most cases" rule.
        //
        // Scope: only the IPC `tool` funnel consults service-token caps.
        // Cron tool execution derives caps from the job's own principal
        // (not a service token), and `extension_api::ToolFunnel` would
        // double-count against the extension-scoped principal that
        // service tokens cannot mint. Service tokens cannot grant
        // extensions, so `resolved_active_extensions` is untouched.
        let resolved_capabilities = if caller.service_token_caps.is_empty() {
            resolved_capabilities
        } else {
            let (kept, dropped) = intersect_caps_with_token(
                resolved_capabilities,
                &caller.service_token_caps,
            );
            // Emit one Security audit row per dropped cap so operators
            // can graph the restriction surface. `agent_did` is the
            // dropped cap itself (the failing capability); details
            // carry the granted token set so the row is self-describing.
            for cap in &dropped {
                let _ = self
                    .host
                    .observability()
                    .audit_security_with_caller(
                        Some(&caller.subject()),
                        "service_token.use_denied",
                        Some(cap.as_str()),
                        serde_json::json!({
                            "granted_caps": caller.service_token_caps,
                        }),
                    )
                    .await;
            }
            kept
        };

        let tool_runtime = self.host.tool_runtime();
        let executor = self.host.async_task_executor();

        let config = AsyncToolConfig::default();
        let task_id = AsyncTaskId::new();

        let receipt = executor
            .execute(
                task_id,
                tool_name.clone(),
                params.clone(),
                session_key,
                config,
                move || {
                    let runtime = tool_runtime.clone();
                    let ws = workspace.clone();
                    let name = tool_name.clone();
                    let p = params.clone();
                    // Move the resolved grants into the closure so the
                    // executor owns the only copy (`'static` + `Send`).
                    let grants = resolved_capabilities;
                    let exts = resolved_active_extensions;
                    Box::pin(async move {
                        // `grants`/`exts` are `Option<Vec<String>>` —
                        // server-derived, never packet-supplied. On
                        // resolution failure they are `None`, which the
                        // tool runtime treats as deny-all (fail-closed).
                        runtime
                            .execute_tool_with_workspace(&name, p, &ws, grants, exts)
                            .await
                    })
                },
            )
            .await?;

        let response = ResponsePacket::AsyncReceipt {
            request_id,
            receipt,
        };
        send_response(sink, response).await?;

        Ok(())
    }

    /// Cancel a running async task by id.
    async fn handle_async_cancel(
        &self,
        request_id: u64,
        task_id: String,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        let executor = self.host.async_task_executor();
        let cancelled = executor.cancel(&task_id).await.unwrap_or(false);

        let response = ResponsePacket::Done {
            request_id,
            success: cancelled,
            error: if cancelled {
                None
            } else {
                Some(format!("Task {task_id} not found or already completed"))
            },
        };
        send_response(sink, response).await?;

        Ok(())
    }
}

/// Intersect principal-derived caps with the caller's service-token
/// caps. Returns `(Option<Vec<String>>, Vec<String>)` — the intersected
/// set (preserving `None` = fail-closed) and the dropped caps (for
/// audit emission).
///
/// Callers must short-circuit when `token_caps.is_empty()` before
/// invoking this helper (the local-bypass path never needs intersection).
fn intersect_caps_with_token(
    principal_caps: Option<Vec<String>>,
    token_caps: &[String],
) -> (Option<Vec<String>>, Vec<String>) {
    let Some(caps) = principal_caps else {
        return (None, Vec::new());
    };
    let token_set: HashSet<&str> = token_caps.iter().map(String::as_str).collect();
    let mut kept = Vec::with_capacity(caps.len());
    let mut dropped = Vec::new();
    for cap in caps {
        if token_set.contains(cap.as_str()) {
            kept.push(cap);
        } else {
            dropped.push(cap);
        }
    }
    (Some(kept), dropped)
}

#[cfg(test)]
mod tests {
    use super::intersect_caps_with_token;

    /// Token set is a strict superset of the principal's caps → all
    /// caps retained, none dropped.
    #[test]
    fn intersect_retains_all_when_token_is_superset() {
        let principal = Some(vec!["fs:read".to_string(), "tool:Bash".to_string()]);
        let token = vec![
            "fs:read".to_string(),
            "fs:write".to_string(),
            "tool:Bash".to_string(),
        ];
        let (kept, dropped) = intersect_caps_with_token(principal, &token);
        assert_eq!(
            kept,
            Some(vec!["fs:read".to_string(), "tool:Bash".to_string()])
        );
        assert!(dropped.is_empty());
    }

    /// Token set is a strict subset → only intersection retained;
    /// dropped caps reported for audit.
    #[test]
    fn intersect_strips_cap_not_in_token() {
        let principal = Some(vec![
            "fs:read".to_string(),
            "fs:write".to_string(),
            "tool:Bash".to_string(),
        ]);
        let token = vec!["fs:read".to_string(), "tool:Bash".to_string()];
        let (kept, dropped) = intersect_caps_with_token(principal, &token);
        assert_eq!(
            kept,
            Some(vec!["fs:read".to_string(), "tool:Bash".to_string()])
        );
        assert_eq!(dropped, vec!["fs:write".to_string()]);
    }

    /// `None` (fail-closed: principal unresolvable) propagates through
    /// unchanged. Service-token restriction cannot rescue a missing
    /// principal — the existing fail-closed invariant stands.
    #[test]
    fn intersect_preserves_none_when_principal_unresolved() {
        let principal: Option<Vec<String>> = None;
        let token = vec!["fs:read".to_string()];
        let (kept, dropped) = intersect_caps_with_token(principal, &token);
        assert!(kept.is_none(), "fail-closed None must propagate");
        assert!(dropped.is_empty());
    }

    /// Token set has zero overlap with principal → every principal cap
    /// is dropped. Audit loop will fire for each. `kept` collapses to
    /// `Some(empty)`; downstream `tool_registry::is_tool_enabled`
    /// treats empty as deny-all (matching the fail-closed invariant).
    #[test]
    fn intersect_drops_all_caps_when_no_overlap() {
        let principal = Some(vec![
            "principal:create".to_string(),
            "runtime:install".to_string(),
        ]);
        let token = vec!["fs:read".to_string()];
        let (kept, dropped) = intersect_caps_with_token(principal, &token);
        assert_eq!(kept, Some(vec![]));
        assert_eq!(
            dropped,
            vec!["principal:create".to_string(), "runtime:install".to_string()]
        );
    }
}
