//! `AsyncExecutorRuntime` — implements the
//! `crate::tools::builtin::async_control::AsyncRuntime` port by wrapping
//! the per-agent `AsyncExecutor` + `Weak<ExtensionCore>` +
//! `principal_id` + `capabilities` snapshot.
//!
//! This is the bridge between peko-tools-builtin (which only sees the
//! trait) and the framework-host (which owns `AsyncExecutor` +
//! `ExtensionCore`). Agents construct one `AsyncExecutorRuntime` per
//! agent process and pass it to `AsyncSpawnTool::new`,
//! `AsyncOutputTool::new`, etc. via the factory closure
//! (`Arc<AsyncExecutorRuntime>` → `Arc<dyn AsyncRuntime>`).
//!
//! ## F37 alignment
//!
//! The `spawn` method builds the canonical funnel closure:
//! `execute_tool_via_hook(...)`, with
//! `ToolDispatchContext::for_principal(principal_id, capabilities)` set
//! so the capability gate at `registry.rs:260-277` evaluates against
//! the spawning principal's grants. Pre-F37, the spawned tool call
//! bypassed the gate entirely; this adapter preserves the F37 routing.
//!
//! ## F38 alignment
//!
//! `spawn` delegates to `AsyncExecutor::dispatch_tool` (no signal),
//! which internally calls `dispatch_tool_with_signal(core, ctx,
//! config, None)`. The `None` cancellation token is the cron-spawn
//! path's default; agents that need a cancel token can plumb one via
//! `dispatch_tool_with_signal` from a peer crate (not used by the
//! built-in `AsyncSpawnTool`).
//!
//! ## Per-agent scope (with global-registry fallback)
//!
//! `lookup`, `list`, and `cancel` operate on this runtime's own
//! `AsyncExecutor::registry()` first — the tasks spawned by THIS run.
//! The production executor registry is per-call (built fresh in
//! `Agent::build_agentic_loop`), so tasks registered elsewhere in the
//! process would otherwise be invisible: background `Bash` tasks live
//! in the process-global registry keyed by the synthetic `"Bash"`
//! agent, subagent runs live in the per-agent-name global registry,
//! and `AsyncSpawn` tasks from an earlier run are gone from the fresh
//! registry. To keep receipts resolvable across turns and producers,
//! each method falls back to the global per-agent registry cache
//! (`super::registry::find_task_across_all_registries` & friends) when
//! the own-registry lookup misses — restoring the intent the
//! pre-Phase-10c helper encoded as its "cross-registry global" mode.

use super::dispatch::ToolDispatchContext;
use super::executor::AsyncExecutor;
use super::types::AsyncToolConfig;
use crate::extensions::framework::core::ExtensionCore;
use crate::tools::builtin::async_control::{
    AsyncRuntime, CancelResult as PortCancelResult, SharedAsyncRuntime, SpawnReceipt, SpawnRequest,
    TaskView, WaitResult,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use peko_subject::PrincipalId;
// Phase 8c.1.A: gated on `test-utils` so external root tests can construct
// `TestAsyncRuntime` via the host's `test-utils` feature flag.
#[cfg(any(test, feature = "test-utils"))]
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

/// Per-agent runtime adapter that speaks the `AsyncRuntime` port to
/// peko-tools-builtin.
pub struct AsyncExecutorRuntime {
    executor: Arc<AsyncExecutor>,
    extension_core: Weak<ExtensionCore>,
    /// Agent identity (DID) used to look up this agent's session key on
    /// the shared `ExtensionCore` for `parent_session_key` stamping.
    agent_id: Option<String>,
    /// F37: snapshot of the spawning principal's ID — flows into
    /// `ToolDispatchContext::for_principal` at spawn time so the
    /// capability gate evaluates against the spawning principal.
    principal_id: PrincipalId,
    /// F37: snapshot of the spawning principal's capability grants.
    capabilities: Arc<Vec<String>>,
    /// Snapshot of the spawning principal's active extensions. Extension-owned
    /// tools must remain inside this scope when dispatched asynchronously.
    active_extensions: Arc<Vec<String>>,
}

impl AsyncExecutorRuntime {
    /// Construct with the per-agent wiring the F37 funnel needs.
    #[must_use]
    pub fn new(
        executor: Arc<AsyncExecutor>,
        extension_core: Weak<ExtensionCore>,
        agent_id: Option<String>,
        principal_id: PrincipalId,
        capabilities: Arc<Vec<String>>,
        active_extensions: Arc<Vec<String>>,
    ) -> Self {
        Self {
            executor,
            extension_core,
            agent_id,
            principal_id,
            capabilities,
            active_extensions,
        }
    }

    /// Convert into a shared trait handle for the built-in tools.
    #[must_use]
    pub fn as_shared(self: Arc<Self>) -> SharedAsyncRuntime {
        self as Arc<dyn AsyncRuntime>
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
    }
}

#[async_trait]
impl AsyncRuntime for AsyncExecutorRuntime {
    async fn spawn(&self, request: SpawnRequest) -> Result<SpawnReceipt> {
        let core = self
            .extension_core
            .upgrade()
            .ok_or_else(|| anyhow!("ExtensionCore has been dropped; cannot spawn"))?;

        // Look up the session key for *this* tool's owning agent
        // (issue #68 — concurrent agents must not stamp each other's
        // `parent_session_key`).
        let session_key = match self.agent_id.as_deref() {
            Some(agent_id) => core
                .current_session_key(agent_id)
                .unwrap_or_else(|| "unknown".to_string()),
            None => "unknown".to_string(),
        };

        let config = AsyncToolConfig {
            timeout_secs: request.timeout_secs,
            label: request.label,
            wake_on_completion: request.wake_on_completion,
            ..Default::default()
        };

        // F37: the runtime overlays its snapshot
        // `principal_id` + `capabilities` on the request, so the
        // closure fires the capability gate against the spawning
        // principal's grants.
        let context =
            ToolDispatchContext::builder(request.tool_name, request.params, session_key.clone())
                .for_principal(self.principal_id.0.clone(), (*self.capabilities).clone())
                .with_active_extensions((*self.active_extensions).clone());

        // F38: `dispatch_tool` internally calls
        // `dispatch_tool_with_signal(core, ctx, config, None)`. No
        // cancel token for natural agent spawns — the spawned task
        // reaches terminal status naturally or via `AsyncStop`.
        let receipt = self.executor.dispatch_tool(&core, context, config).await?;
        Ok(SpawnReceipt {
            task_id: receipt.task_id,
        })
    }

    async fn lookup(&self, task_id: &str) -> Option<TaskView> {
        {
            let registry = self.executor.registry();
            let reg = registry.read().await;
            if let Some(entry) = reg.get(&task_id.to_string()) {
                return Some(Self::project_taskview(entry));
            }
        }
        // Fallback: the task may live in a process-global per-agent
        // registry (background `Bash` tasks, subagent runs, tasks from
        // an earlier run of this agent) — see the module doc.
        super::registry::find_task_across_all_registries(task_id)
            .await
            .map(|entry| Self::project_taskview(&entry))
    }

    async fn list(&self, status_filter: Option<&str>, tool_filter: Option<&str>) -> Vec<TaskView> {
        // Own-registry entries first, then any tasks from the global
        // per-agent registries (deduped by task_id, own wins). The
        // own registry is per-call in production, so without the
        // global merge `AsyncList` would never show background `Bash`
        // tasks or previous runs' spawns.
        let mut seen = std::collections::HashSet::new();
        let mut tasks: Vec<TaskView> = Vec::new();
        {
            let registry = self.executor.registry();
            let reg = registry.read().await;
            for entry in reg.list_tasks(None) {
                if seen.insert(entry.task_id.clone()) {
                    tasks.push(Self::project_taskview(&entry));
                }
            }
        }
        for entry in super::registry::list_all_tasks_across_all_registries().await {
            if seen.insert(entry.task_id.clone()) {
                tasks.push(Self::project_taskview(&entry));
            }
        }
        tasks
            .into_iter()
            .filter(|t| {
                status_filter.map_or(true, |f| t.status == f)
                    && tool_filter.map_or(true, |f| t.tool_name == f)
            })
            .collect()
    }

    async fn cancel(&self, task_id: &str) -> PortCancelResult {
        {
            let registry = self.executor.registry();
            let mut reg = registry.write().await;
            match reg.cancel(&task_id.to_string()) {
                super::registry::CancelResult::Success { previous } => {
                    return PortCancelResult::Success { previous };
                }
                super::registry::CancelResult::AlreadyTerminal { previous } => {
                    return PortCancelResult::AlreadyTerminal { previous };
                }
                super::registry::CancelResult::NotFound => {} // fall through to global registries
            }
        }
        match super::registry::cancel_task_across_all_registries(task_id).await {
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
        let in_own_registry = {
            let reg = self.executor.registry().read().await;
            reg.get(&task_id_string).is_some()
        };
        // When the task lives in a global per-agent registry (see
        // `lookup`), wait on THAT registry — the own executor's
        // registry would immediately error with "not found".
        let wait_result = if !in_own_registry {
            match super::registry::find_owning_registry_for_task(task_id).await {
                Some(owning) => {
                    super::registry::wait_for_completion_polled(&owning, &task_id_string, timeout)
                        .await?
                }
                None => {
                    self.executor
                        .wait_for_completion(&task_id_string, timeout)
                        .await?
                }
            }
        } else {
            self.executor
                .wait_for_completion(&task_id_string, timeout)
                .await?
        };
        Ok(match wait_result {
            super::types::WaitResult::Completed { result } => WaitResult::Completed { result },
            super::types::WaitResult::Failed { error } => WaitResult::Failed { error },
            super::types::WaitResult::Cancelled => WaitResult::Cancelled,
            super::types::WaitResult::Timeout => WaitResult::Timeout,
        })
    }
}

// ─── Test helper ──────────────────────────────────────────────────
//
// In-tree tests of `AsyncListTool`, `AsyncStatusTool`, `AsyncStopTool`,
// and `AsyncOutputTool` need to construct an `AsyncRuntime` to plug
// into the tool under test. The real `AsyncExecutor` machinery is
// framework-internal, so for tests we provide a small in-memory
// runtime here in root that the tests can reach via the shim.
//
// Used by `tests::async_tool_test_runtime` re-export.

/// In-memory `AsyncRuntime` for testing the Async* tools.
///
/// Backed by a `Mutex<HashMap<String, TaskEntry>>` instead of the
/// framework's `AsyncTaskRegistry`. Supports manual status flips so
/// tests can simulate terminal states (`completed`, `failed`, etc.)
/// without running a real `AsyncExecutor`.
///
/// Gated `#[cfg(any(test, feature = "test-utils"))]` so external test
/// crates (root `src/tools/builtin/async_*.rs`) can construct it via
/// the host's `test-utils` feature, not just host-internal tests.
/// (Phase 8c.1.A)
#[cfg(any(test, feature = "test-utils"))]
pub struct TestAsyncRuntime {
    map: std::sync::Mutex<HashMap<String, TestTaskEntry>>,
}

/// In-memory task entry for `TestAsyncRuntime` tests.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Clone)]
pub struct TestTaskEntry {
    pub task_id: String,
    pub tool_name: String,
    pub status: String,
    pub parent_session_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result: Option<serde_json::Value>,
    pub label: Option<String>,
    pub metadata_type: String,
}

#[cfg(any(test, feature = "test-utils"))]
impl TestAsyncRuntime {
    /// Build an empty test runtime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Insert a task entry directly.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn insert(&self, entry: TestTaskEntry) {
        let mut map = self.map.lock().unwrap();
        map.insert(entry.task_id.clone(), entry);
    }

    /// Convert into a shared trait handle for built-in tools.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn as_shared(self: Arc<Self>) -> SharedAsyncRuntime {
        self as Arc<dyn AsyncRuntime>
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl Default for TestAsyncRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait]
impl AsyncRuntime for TestAsyncRuntime {
    async fn spawn(&self, _request: SpawnRequest) -> Result<SpawnReceipt> {
        Err(anyhow!("TestAsyncRuntime::spawn is not supported in tests"))
    }

    async fn lookup(&self, task_id: &str) -> Option<TaskView> {
        let map = self.map.lock().unwrap();
        map.get(task_id).map(|e| {
            TaskView::new(
                e.task_id.clone(),
                e.tool_name.clone(),
                e.status.clone(),
                e.parent_session_key.clone(),
                e.created_at,
                e.completed_at,
                e.result.clone(),
                e.label.clone(),
                e.metadata_type.clone(),
            )
        })
    }

    async fn list(&self, status_filter: Option<&str>, tool_filter: Option<&str>) -> Vec<TaskView> {
        let map = self.map.lock().unwrap();
        map.values()
            .map(|e| {
                TaskView::new(
                    e.task_id.clone(),
                    e.tool_name.clone(),
                    e.status.clone(),
                    e.parent_session_key.clone(),
                    e.created_at,
                    e.completed_at,
                    e.result.clone(),
                    e.label.clone(),
                    e.metadata_type.clone(),
                )
            })
            .filter(|t| {
                status_filter.map_or(true, |f| t.status == f)
                    && tool_filter.map_or(true, |f| t.tool_name == f)
            })
            .collect()
    }

    async fn cancel(&self, task_id: &str) -> PortCancelResult {
        let mut map = self.map.lock().unwrap();
        let Some(entry) = map.get_mut(task_id) else {
            return PortCancelResult::NotFound;
        };
        let previous = entry.status.clone();
        if matches!(
            previous.as_str(),
            "completed" | "failed" | "cancelled" | "timed_out"
        ) {
            return PortCancelResult::AlreadyTerminal { previous };
        }
        entry.status = "cancelled".to_string();
        entry.completed_at = Some(chrono::Utc::now());
        PortCancelResult::Success { previous }
    }

    async fn wait_for_completion(&self, _task_id: &str, _timeout: Duration) -> Result<WaitResult> {
        Ok(WaitResult::Timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::async_exec::executor::{
        standalone_inbox_registry, AsyncExecutor,
    };
    use peko_tools_core::Tool;

    /// Build a runtime whose own executor registry is empty — any
    /// resolution of a task id must come from the global-registry
    /// fallback.
    fn make_runtime() -> Arc<AsyncExecutorRuntime> {
        let core = Arc::new(ExtensionCore::new());
        Arc::new(AsyncExecutorRuntime::new(
            Arc::new(AsyncExecutor::new(standalone_inbox_registry())),
            Arc::downgrade(&core),
            None,
            PrincipalId::system().clone(),
            Arc::new(Vec::new()),
            Arc::new(Vec::new()),
        ))
    }

    /// Bug pin: `Bash { run_in_background: true }` receipts are
    /// registered in the process-global registry keyed by the
    /// synthetic `"Bash"` agent, so a runtime bound to a fresh
    /// per-call executor could not resolve them ("Task not found").
    /// lookup / list / wait_for_completion / cancel must all resolve
    /// the receipt through the global-registry fallback.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_background_receipt_resolves_via_global_fallback() {
        let bash = crate::tools::builtin::BashTool::new();
        let receipt = bash
            .execute(serde_json::json!({
                "command": "echo bg-done",
                "run_in_background": true,
            }))
            .await
            .unwrap();
        let task_id = receipt["task_id"].as_str().unwrap().to_string();
        assert!(task_id.starts_with("Bash:"));

        let runtime = make_runtime();

        let view = runtime
            .lookup(&task_id)
            .await
            .expect("lookup must resolve the Bash receipt id");
        assert_eq!(view.tool_name, "Bash");

        let list = runtime.list(None, Some("Bash")).await;
        assert!(
            list.iter().any(|t| t.task_id == task_id),
            "list must include the Bash background task"
        );

        let wait = runtime
            .wait_for_completion(&task_id, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(
            matches!(wait, WaitResult::Completed { .. }),
            "blocking wait must observe completion: {wait:?}"
        );

        let view = runtime.lookup(&task_id).await.unwrap();
        assert!(view.is_terminal(), "task should be terminal after wait");

        // Cancel on the terminal task must report AlreadyTerminal —
        // NotFound would mean the fallback still can't see the task.
        let cancel = runtime.cancel(&task_id).await;
        assert!(
            matches!(cancel, PortCancelResult::AlreadyTerminal { .. }),
            "cancel on completed task: {cancel:?}"
        );
    }

    /// `AsyncStop` on a still-running background Bash task must find it
    /// through the fallback and return `Success`, not `NotFound`.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_background_cancel_resolves_via_global_fallback() {
        let bash = crate::tools::builtin::BashTool::new();
        let receipt = bash
            .execute(serde_json::json!({
                "command": "sleep 30",
                "run_in_background": true,
            }))
            .await
            .unwrap();
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        let runtime = make_runtime();
        let cancel = runtime.cancel(&task_id).await;
        assert!(
            matches!(cancel, PortCancelResult::Success { .. }),
            "cancel on running task: {cancel:?}"
        );
    }

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
}
