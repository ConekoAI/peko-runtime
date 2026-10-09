//! `AsyncExecutorRuntime` bridges the AsyncRuntime port to a
//! principal-owned AsyncExecutor and a weak ToolingRuntime handle.
//! Installation creates stable Async* tools; each call supplies its own
//! session, workspace, and principal name through ToolContext.
//!
//! Spawns enter the attributed ToolFunnel with the caller's live binding.
//!
//! ## F38 alignment
//!
//! `spawn` delegates to `AsyncExecutor::dispatch_tool` (no signal),
//! which internally calls `dispatch_tool_with_signal(core, ctx,
//! config, None)`. The `None` cancellation token is the cron-spawn
//! path's default; agents that need a cancel token can plumb one via
//! `dispatch_tool_with_signal` from a peer crate (not used by the
//! built-in `AsyncSpawnAction`).
//!
//! ## Principal-owned tasks with caller-scoped dispatch
//!
//! Production installs a stable executor per principal. Its registry preserves
//! Async action spawn receipts across turns. Invocation context carries the parent
//! session, workspace, and principal name, and a live caller binding is retained
//! for background execution. Lookup/list/cancel also search the global
//! registries (subagent runs, calls detached on the foreground timeout),
//! applying the principal ownership filter to every entry.

use super::dispatch::ToolDispatchContext;
use super::executor::AsyncExecutor;
use super::types::AsyncToolConfig;
use crate::tools::builtin::async_control::{
    AsyncRuntime, CancelResult as PortCancelResult, SharedAsyncRuntime, SpawnReceipt, SpawnRequest,
    TaskView, WaitResult,
};
use crate::tools::runtime::ToolingRuntime;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use peko_subject::PrincipalId;
use std::sync::{Arc, Weak};
use std::time::Duration;

/// Principal-owned runtime adapter for the AsyncRuntime tool port.
pub struct AsyncExecutorRuntime {
    executor: Arc<AsyncExecutor>,
    tooling: Weak<ToolingRuntime>,
    /// Agent identity (DID) used to look up this agent's session key on
    /// the shared `ToolingRuntime` for `parent_session_key` stamping.
    agent_id: Option<String>,
    /// F37: snapshot of the spawning principal's ID — flows into
    /// `ToolDispatchContext::for_principal` at spawn time.
    principal_id: PrincipalId,
}

impl AsyncExecutorRuntime {
    /// Construct with principal-owned executor and attributed dispatch wiring.
    #[must_use]
    pub fn new(
        executor: Arc<AsyncExecutor>,
        tooling: Weak<ToolingRuntime>,
        agent_id: Option<String>,
        principal_id: PrincipalId,
    ) -> Self {
        Self {
            executor,
            tooling,
            agent_id,
            principal_id,
        }
    }

    /// Convert into a shared trait handle for the built-in tools.
    #[must_use]
    pub fn as_shared(self: Arc<Self>) -> SharedAsyncRuntime {
        self as Arc<dyn AsyncRuntime>
    }

    async fn spawn_inner(
        &self,
        request: SpawnRequest,
        caller: Option<&peko_tools_core::ToolContext>,
    ) -> Result<SpawnReceipt> {
        let core = self
            .tooling
            .upgrade()
            .ok_or_else(|| anyhow!("ToolingRuntime has been dropped; cannot spawn"))?;

        // Parent-session stamping, per call (ADR-061 follow-up): the
        // request's `parent_session_id` — filled by `AsyncSpawnAction`
        // from `ToolContext.session_id` — wins. It is the run id on the
        // agent-loop path and the token-resolved node id (or the
        // session-key string when nodeless) on the `ExecuteTool` path.
        // The legacy session-key cell (keyed by this runtime's agent
        // DID on the shared core, issue #68) is now only the fallback
        // for ctx-less in-process dispatches — it is stale between
        // runs and was never correct on the workflow path.
        let session_key = request
            .parent_session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.agent_id
                    .as_deref()
                    .and_then(|agent_id| core.session_keys().get(agent_id))
            })
            .unwrap_or_else(|| "unknown".to_string());

        let config = AsyncToolConfig {
            // `None` (model omitted `timeout_secs`) must fall back to the
            // documented 7200s default — the struct-update syntax below would
            // otherwise overwrite `AsyncToolConfig::default().timeout_secs`
            // with `None`, giving the task no timeout at all.
            timeout_secs: request
                .timeout_secs
                .or_else(|| AsyncToolConfig::default().timeout_secs),
            timeout_millis: request.timeout_millis,
            label: request.label,
            wake_on_completion: request.wake_on_completion,
            // Ownership stamping (P1-4): the task belongs to the spawning
            // principal so other principals' Async* surfaces can't see it.
            principal_id: self.principal_id.clone(),
            ..Default::default()
        };

        // F37: the runtime stamps its snapshot `principal_id` on the
        // request so the dispatched task is attributed to the spawning
        // principal.
        let mut context =
            ToolDispatchContext::builder(request.tool_name, request.params, session_key.clone())
                .for_principal(self.principal_id.0.clone())
                .with_session_id(session_key.clone());
        if let Some(caller) = caller {
            context.workspace = caller.workspace.clone();
            context.agent_id = caller.agent_id.clone();
            context.principal_name = caller.principal_name.clone();
        }
        // Capture the live caller's binding before scheduling, so a task
        // keeps its executor/config even if the foreground turn ends.
        let execution = core
            .execution_binding(&self.principal_id, &session_key)
            .unwrap_or(core);

        // F38: `dispatch_tool` internally calls
        // `dispatch_tool_with_signal(core, ctx, config, None)`. No
        // cancel token for natural agent spawns — the spawned task
        // reaches terminal status naturally or via `Async action stop`.
        let receipt = self
            .executor
            .dispatch_tool(&execution, context, config)
            .await?;
        Ok(SpawnReceipt {
            task_id: receipt.task_id,
        })
    }

    /// Project an `AsyncTaskEntry` into the canonical `TaskView` shape
    /// the port exposes. Done here (root side) because the per-task
    /// `metadata` field is framework-internal — peko-tools-builtin does
    /// not know about `SubagentMetadata` etc.
    fn project_taskview(entry: &super::registry::AsyncTaskEntry) -> TaskView {
        let metadata_type = match &entry.metadata {
            super::registry::TaskMetadata::None => "none",
            super::registry::TaskMetadata::Subagent(_) => "subagent",
        };
        TaskView::new(
            entry.task_id.clone(),
            entry.tool_name.clone(),
            entry.status.as_str().to_string(),
            entry.parent_session_key.clone(),
            entry.created_at,
            entry.completed_at,
            entry.result.clone(),
            entry.config.label.clone(),
            metadata_type.to_string(),
        )
        .with_partial_output(if entry.status.is_terminal() {
            None
        } else {
            entry.partial_output(super::registry::PARTIAL_OUTPUT_PREVIEW_BYTES)
        })
    }

    /// Per-principal isolation (P1-4): an entry is visible to this
    /// runtime iff this principal owns it. Every task has an owner —
    /// system-owned tasks (calls without a principal) are visible to no
    /// principal. Anything else is treated as nonexistent — same
    /// `NotFound` the caller would get for a genuinely unknown id, so
    /// the cross-registry fallback no longer leaks other principals'
    /// tasks into `Async action list`/`Async action status`/`Async action stop`.
    ///
    /// The dispatcher registers every foreground call as a routing task
    /// (`deliver_completion: false`) so it can detach on timeout; such a
    /// task becomes background work — and visible — only once it detaches
    /// (which turns delivery on before the receipt is returned).
    fn is_visible(&self, entry: &super::registry::AsyncTaskEntry) -> bool {
        entry.config.principal_id == self.principal_id && entry.config.deliver_completion
    }
}

#[async_trait]
impl AsyncRuntime for AsyncExecutorRuntime {
    async fn spawn(&self, request: SpawnRequest) -> Result<SpawnReceipt> {
        self.spawn_inner(request, None).await
    }

    async fn spawn_with_context(
        &self,
        request: SpawnRequest,
        ctx: &peko_tools_core::ToolContext,
    ) -> Result<SpawnReceipt> {
        self.spawn_inner(request, Some(ctx)).await
    }

    async fn lookup(&self, task_id: &str) -> Option<TaskView> {
        // Another principal's task is indistinguishable from a missing one.
        let registry = self.executor.registry();
        let reg = registry.read().await;
        reg.get(&task_id.to_string())
            .filter(|entry| self.is_visible(entry))
            .map(Self::project_taskview)
    }

    async fn list(&self, status_filter: Option<&str>, tool_filter: Option<&str>) -> Vec<TaskView> {
        // The principal's registry holds all of its background work (Async
        // spawn, background Bash, detached calls, subagent runs, cron
        // jobs); ownership is still checked on every entry.
        let registry = self.executor.registry();
        let reg = registry.read().await;
        reg.list_tasks(None)
            .into_iter()
            .filter(|entry| self.is_visible(entry))
            .map(|entry| Self::project_taskview(&entry))
            .filter(|t| {
                status_filter.map_or(true, |f| t.status == f)
                    && tool_filter.map_or(true, |f| t.tool_name == f)
            })
            .collect()
    }

    async fn cancel(&self, task_id: &str) -> PortCancelResult {
        let registry = self.executor.registry();
        let mut reg = registry.write().await;
        if let Some(entry) = reg.get(&task_id.to_string()) {
            if !self.is_visible(entry) {
                return PortCancelResult::NotFound;
            }
        }
        match reg.cancel(&task_id.to_string()) {
            super::registry::CancelResult::Success { previous } => {
                PortCancelResult::Success { previous }
            }
            super::registry::CancelResult::AlreadyTerminal { previous } => {
                PortCancelResult::AlreadyTerminal { previous }
            }
            super::registry::CancelResult::NotFound => PortCancelResult::NotFound,
        }
    }

    async fn wait_for_completion(&self, task_id: &str, timeout: Duration) -> Result<WaitResult> {
        let task_id_string = task_id.to_string();
        // Ownership pre-check (P1-4): waiting on another principal's task
        // would leak its result into `Async action output`; invisible
        // tasks behave as not-found.
        let visible = {
            let reg = self.executor.registry().read().await;
            reg.get(&task_id_string).map(|entry| self.is_visible(entry))
        };
        if visible == Some(false) {
            return Err(anyhow!("Task {task_id} not found"));
        }
        let wait_result = self
            .executor
            .wait_for_completion(&task_id_string, timeout)
            .await?;
        Ok(match wait_result {
            super::types::WaitResult::Completed { result } => WaitResult::Completed { result },
            super::types::WaitResult::Failed { error } => WaitResult::Failed { error },
            super::types::WaitResult::Cancelled => WaitResult::Cancelled,
            super::types::WaitResult::Timeout => WaitResult::Timeout,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_exec::executor::{standalone_inbox_registry, AsyncExecutor};

    /// Regression pin: `AsyncExecutor::wait_for_completion` used to
    /// hold the registry read guard for the entire wait, starving the
    /// background task's write-lock status update until the timeout
    /// fired. The wait must observe completion promptly instead.
    #[tokio::test]
    async fn wait_for_completion_observes_completion_promptly() {
        let executor = Arc::new(AsyncExecutor::new(standalone_inbox_registry()));
        let task_id = "test:prompt-completion".to_string();
        executor
            .execute(
                task_id.clone(),
                "test",
                serde_json::json!({}),
                "session",
                super::super::types::AsyncToolConfig::default(),
                || async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok(serde_json::json!({"ok": true}))
                },
            )
            .await
            .unwrap();

        let start = std::time::Instant::now();
        let result = executor
            .wait_for_completion(&task_id, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(
            matches!(result, super::super::types::WaitResult::Completed { .. }),
            "expected Completed, got: {result:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "wait starved the registry writer until the timeout: {:?}",
            start.elapsed()
        );
    }

    /// P0-3: an `Async action spawn` that omits `timeout_secs` must inherit the
    /// documented 7200s default — the spawned task must not run
    /// timeout-free.
    #[tokio::test]
    async fn spawn_without_timeout_gets_default_7200() {
        let executor = Arc::new(AsyncExecutor::new(standalone_inbox_registry()));
        let core = crate::tools::runtime::ToolingRuntime::standalone();
        let runtime = Arc::new(AsyncExecutorRuntime::new(
            Arc::clone(&executor),
            Arc::downgrade(&core),
            None,
            PrincipalId::system().clone(),
        ));
        let receipt = runtime
            .spawn(crate::tools::builtin::async_control::SpawnRequest {
                tool_name: "whatever".to_string(),
                params: serde_json::json!({}),
                label: None,
                wake_on_completion: false,
                timeout_secs: None,
                timeout_millis: None,
                parent_session_id: Some("session_t".to_string()),
            })
            .await
            .unwrap();
        let entry = {
            let reg = executor.registry().read().await;
            reg.get(&receipt.task_id).cloned()
        }
        .expect("spawned task must be registered");
        assert_eq!(
            entry.config.timeout_secs,
            Some(7200),
            "omitted timeout_secs must inherit the documented 7200s default"
        );
    }

    /// P1-4 defense in depth: even over one shared registry, a task
    /// stamped with principal A is invisible to principal B's runtime
    /// across lookup / list / cancel / wait, and visible to A's. A
    /// system-owned task (a call that carried no principal) is visible to
    /// neither: unattributed work must never leak across principals.
    #[tokio::test]
    async fn cross_principal_tasks_are_invisible() {
        let registry: super::super::registry::SharedAsyncTaskRegistry = Arc::default();
        {
            let mut reg = registry.write().await;
            reg.register(super::super::registry::AsyncTaskEntry::new(
                "tool:a-task".to_string(),
                "tool".to_string(),
                serde_json::json!({}),
                "session_a".to_string(),
                super::super::types::AsyncToolConfig {
                    principal_id: PrincipalId("prin_a".to_string()),
                    ..Default::default()
                },
            ));
            reg.register(super::super::registry::AsyncTaskEntry::new(
                "tool:system-task".to_string(),
                "tool".to_string(),
                serde_json::json!({}),
                "session_s".to_string(),
                super::super::types::AsyncToolConfig::default(),
            ));
        }

        let make_runtime_for = |pid: &str| {
            let core = crate::tools::runtime::ToolingRuntime::standalone();
            Arc::new(AsyncExecutorRuntime::new(
                Arc::new(AsyncExecutor::with_registries(
                    Arc::clone(&registry),
                    standalone_inbox_registry(),
                )),
                Arc::downgrade(&core),
                None,
                PrincipalId(pid.to_string()),
            ))
        };
        let runtime_a = make_runtime_for("prin_a");
        let runtime_b = make_runtime_for("prin_b");

        assert!(
            runtime_a.lookup("tool:a-task").await.is_some(),
            "owner must see its own task"
        );
        assert!(
            runtime_b.lookup("tool:a-task").await.is_none(),
            "other principal must NOT see the task"
        );
        assert!(
            runtime_b
                .list(None, None)
                .await
                .iter()
                .all(|t| t.task_id != "tool:a-task"),
            "other principal's Async action list must filter the task out"
        );
        assert!(
            matches!(
                runtime_b.cancel("tool:a-task").await,
                PortCancelResult::NotFound
            ),
            "other principal's Async action stop must report NotFound"
        );
        assert!(
            runtime_b
                .wait_for_completion("tool:a-task", Duration::from_millis(50))
                .await
                .is_err(),
            "other principal's blocking wait must error as not-found"
        );

        for runtime in [&runtime_a, &runtime_b] {
            assert!(runtime.lookup("tool:system-task").await.is_none());
            assert!(runtime
                .list(None, None)
                .await
                .iter()
                .all(|t| t.task_id != "tool:system-task"));
            assert!(matches!(
                runtime.cancel("tool:system-task").await,
                PortCancelResult::NotFound
            ));
        }

        // The tasks themselves are untouched (every cancel was refused).
        let reg = registry.read().await;
        for id in ["tool:a-task", "tool:system-task"] {
            assert!(!reg.get(&id.to_string()).unwrap().status.is_terminal());
        }
    }
}
