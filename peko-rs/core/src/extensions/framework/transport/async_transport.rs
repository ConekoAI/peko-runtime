//! Async Task Transport Abstraction (ADR-020 Phase 3)
//!
//! Provides a trait-based abstraction over async task execution so that the
//! `AsyncExecutionRouter` can work identically whether it is running inside the
//! daemon (local execution) or inside a test harness.
//!
//! 2026-09-27 consolidation (ADR-063 (dead IPC path)): the IPC transports
//! are deleted. ADR-021 made the daemon the central runtime — the CLI is a
//! pure IPC client that never executes tools, so `DaemonIpcTransport` (and
//! its `DaemonTransport` projection, the `UnavailableAsyncTransport`
//! fail-fast mode, and `ipc::create_transport`) had no live producer. What
//! remains is the single in-process [`LocalAsyncTransport`]; the trait stays
//! because the router stores `Arc<dyn AsyncTaskTransport>` and tests
//! substitute mocks.

use crate::async_exec::executor::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskStatus, AsyncToolConfig,
};
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;

/// Boxed async execution closure type
///
/// Returns `Value` directly — tool-specific formatting is handled at delivery time.
pub type BoxedExecutionFn = Box<
    dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send>>
        + Send,
>;

/// Transport abstraction for async task execution
///
/// The single production implementation is [`LocalAsyncTransport`], which
/// runs tasks in-process via `AsyncExecutor` (daemon mode).
#[async_trait::async_trait]
pub trait AsyncTaskTransport: Send + Sync {
    /// Spawn a new async task
    ///
    /// For `LocalAsyncTransport`, this creates a placeholder task; use
    /// `spawn_task_boxed` for actual tool execution with a closure.
    async fn spawn_task(
        &self,
        task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        session_key: String,
        workspace: std::path::PathBuf,
        config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt>;

    /// Spawn a task with a boxed execution closure (non-generic)
    ///
    /// This is the primary method used by `AsyncExecutionRouter::execute_async`
    /// because the router has already built the execution closure.
    async fn spawn_task_boxed(
        &self,
        task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        session_key: String,
        workspace: std::path::PathBuf,
        config: AsyncToolConfig,
        _execution_fn: BoxedExecutionFn,
    ) -> Result<AsyncTaskReceipt> {
        // Default: ignore the closure and delegate to spawn_task.
        self.spawn_task(task_id, tool_name, params, session_key, workspace, config)
            .await
    }

    /// Get the current status of a task
    async fn get_status(&self, task_id: &AsyncTaskId) -> Result<Option<AsyncTaskStatus>>;

    /// Cancel a running or pending task
    ///
    /// Returns `true` if the task was found and cancelled.
    async fn cancel_task(&self, task_id: &AsyncTaskId) -> Result<bool>;

    /// Flip the task's `deliver_completion` flag on so its terminal
    /// outcome is pushed to the session inbox. The router calls this
    /// when a call detaches past its timeout and the agent receives a
    /// `queued` receipt. Default: `Ok(false)` (unsupported).
    async fn deliver_on_completion(&self, _task_id: &AsyncTaskId) -> Result<bool> {
        Ok(false)
    }

    /// Drop finished tasks past their retention window; returns how many.
    async fn purge_finished(&self) -> usize {
        0
    }
}

// ================================================================================
// LocalAsyncTransport — used inside the daemon
// ================================================================================

use crate::async_exec::executor::AsyncExecutor;

/// Local transport that executes tasks in-process via `AsyncExecutor`
#[derive(Debug, Clone)]
pub struct LocalAsyncTransport {
    executor: Arc<AsyncExecutor>,
}

impl LocalAsyncTransport {
    /// Create a new local transport wrapping the given executor
    pub fn new(executor: Arc<AsyncExecutor>) -> Self {
        Self { executor }
    }

    /// Create from a bare `AsyncExecutor`
    pub fn from_executor(executor: AsyncExecutor) -> Self {
        Self::new(Arc::new(executor))
    }
}

#[async_trait::async_trait]
impl AsyncTaskTransport for LocalAsyncTransport {
    async fn spawn_task(
        &self,
        _task_id: AsyncTaskId,
        _tool_name: String,
        _params: Value,
        _session_key: String,
        _workspace: std::path::PathBuf,
        _config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        // This method should not be called directly for local execution.
        // Use spawn_task_boxed instead, which accepts the execution closure.
        anyhow::bail!(
            "LocalAsyncTransport::spawn_task is not supported. Use spawn_task_boxed instead."
        )
    }

    async fn spawn_task_boxed(
        &self,
        task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        session_key: String,
        _workspace: std::path::PathBuf,
        config: AsyncToolConfig,
        execution_fn: BoxedExecutionFn,
    ) -> Result<AsyncTaskReceipt> {
        self.executor
            .execute_boxed(
                task_id,
                tool_name,
                params,
                session_key,
                config,
                execution_fn,
            )
            .await
    }

    async fn get_status(&self, task_id: &AsyncTaskId) -> Result<Option<AsyncTaskStatus>> {
        Ok(self.executor.check_status(task_id).await)
    }

    async fn cancel_task(&self, task_id: &AsyncTaskId) -> Result<bool> {
        self.executor.cancel(task_id).await
    }

    async fn deliver_on_completion(&self, task_id: &AsyncTaskId) -> Result<bool> {
        Ok(self.executor.enable_completion_delivery(task_id).await)
    }

    async fn purge_finished(&self) -> usize {
        self.executor.registry().write().await.cleanup_completed()
    }
}

impl LocalAsyncTransport {
    /// Get a reference to the underlying executor
    pub fn executor(&self) -> &AsyncExecutor {
        &self.executor
    }
}

// ================================================================================
// Transport factory
// ================================================================================

/// Create a local transport wired to the process-shared inbox registry
/// ([`shared_inbox_registry`](crate::async_exec::executor::shared_inbox_registry)),
/// which defers to the daemon-installed registry once `AppState` installs
/// it. Used for the pre-`AppState` `ToolingRuntime` the CLI installs for
/// `peko daemon start --foreground`; the daemon's composition root prefers
/// [`create_local_transport_with_inbox`] with the hoisted registry.
pub fn create_local_transport() -> Arc<dyn AsyncTaskTransport> {
    create_local_transport_with_inbox(crate::async_exec::executor::shared_inbox_registry())
}

/// WS3 (implicit session management, 2026-08-11): same as
/// [`create_local_transport`] but threads the daemon-shared inbox
/// registry into the executor so completions pushed by tools
/// dispatched through this transport actually reach the parent
/// agentic loop's per-iteration drain. Without this, the
/// `AsyncExecutionRouter` creates a private registry disconnected
/// from the loop's drain and WS3's `persist_subagent_completions`
/// never fires in production.
pub fn create_local_transport_with_inbox(
    inbox_registry: Arc<peko_session::InboxRegistry>,
) -> Arc<dyn AsyncTaskTransport> {
    // Calls from a bound principal route through that principal's
    // executor; this transport only carries calls without one (system
    // work, visible to no principal), so its registry is private.
    let executor = crate::async_exec::executor::AsyncExecutor::new(inbox_registry);
    Arc::new(LocalAsyncTransport::from_executor(executor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_transport_new() {
        let executor = AsyncExecutor::new(crate::async_exec::executor::standalone_inbox_registry());
        let transport = LocalAsyncTransport::from_executor(executor);
        let _ = transport.executor();
    }
}
