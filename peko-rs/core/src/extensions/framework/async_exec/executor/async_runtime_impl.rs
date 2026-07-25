//! `AsyncExecutorRuntime` — implements the
//! `peko_tools_builtin::async_control::AsyncRuntime` port by wrapping
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
//! ## Per-agent scope
//!
//! `lookup`, `list`, and `cancel` operate on this runtime's own
//! `AsyncExecutor::registry()` — the tasks spawned by THIS agent. The
//! legacy "all agents' tasks" cross-cutting helper functions are
//! intentionally not exposed; the per-agent scope is what every
//! current caller actually uses.

use super::dispatch::ToolDispatchContext;
use super::executor::AsyncExecutor;
use super::types::AsyncToolConfig;
use crate::extensions::framework::core::ExtensionCore;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use peko_subject::PrincipalId;
use peko_tools_builtin::async_control::{
    AsyncRuntime, CancelResult as PortCancelResult, SharedAsyncRuntime, SpawnReceipt, SpawnRequest,
    TaskView, WaitResult,
};
#[cfg(test)]
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
    ) -> Self {
        Self {
            executor,
            extension_core,
            agent_id,
            principal_id,
            capabilities,
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
                .for_principal(self.principal_id.0.clone(), (*self.capabilities).clone());

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
        let registry = self.executor.registry();
        let reg = registry.read().await;
        reg.get(&task_id.to_string()).map(Self::project_taskview)
    }

    async fn list(&self, status_filter: Option<&str>, tool_filter: Option<&str>) -> Vec<TaskView> {
        let registry = self.executor.registry();
        let reg = registry.read().await;
        reg.list_tasks(None)
            .into_iter()
            .map(|e| Self::project_taskview(&e))
            .filter(|t| {
                status_filter.map_or(true, |f| t.status == f)
                    && tool_filter.map_or(true, |f| t.tool_name == f)
            })
            .collect()
    }

    async fn cancel(&self, task_id: &str) -> PortCancelResult {
        let registry = self.executor.registry();
        let mut reg = registry.write().await;
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
        Ok(
            match self
                .executor
                .wait_for_completion(&task_id.to_string(), timeout)
                .await?
            {
                super::types::WaitResult::Completed { result } => WaitResult::Completed { result },
                super::types::WaitResult::Failed { error } => WaitResult::Failed { error },
                super::types::WaitResult::Cancelled => WaitResult::Cancelled,
                super::types::WaitResult::Timeout => WaitResult::Timeout,
            },
        )
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
#[cfg(test)]
pub struct TestAsyncRuntime {
    map: std::sync::Mutex<HashMap<String, TestTaskEntry>>,
}

/// In-memory task entry for `TestAsyncRuntime` tests.
#[cfg(test)]
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

#[cfg(test)]
impl TestAsyncRuntime {
    /// Build an empty test runtime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Insert a task entry directly.
    #[cfg(test)]
    pub fn insert(&self, entry: TestTaskEntry) {
        let mut map = self.map.lock().unwrap();
        map.insert(entry.task_id.clone(), entry);
    }

    /// Convert into a shared trait handle for built-in tools.
    #[cfg(test)]
    #[must_use]
    pub fn as_shared(self: Arc<Self>) -> SharedAsyncRuntime {
        self as Arc<dyn AsyncRuntime>
    }
}

#[cfg(test)]
impl Default for TestAsyncRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
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
