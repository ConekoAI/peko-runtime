//! Async Task Transport Abstraction (ADR-020 Phase 3)
//!
//! Provides a trait-based abstraction over async task execution so that the
//! `AsyncExecutionRouter` can work identically whether it is running inside the
//! daemon (local execution) or inside the CLI (IPC submission to daemon).
//!
//! Phase 8b: lifted into `peko-extension-host`. The `DaemonIpcTransport`
//! consumes the value-returning [`DaemonTransport`] trait (declared in
//! the parent `transport` module) — root owns the `DaemonClient`
//! implementation and the host stays free of any root IPC dep.

use crate::async_exec::executor::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskStatus, AsyncToolConfig,
};
use crate::transport::DaemonTransport;
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
/// Implementations:
/// - `LocalAsyncTransport` — runs tasks in-process via `AsyncExecutor` (daemon mode)
/// - `DaemonIpcTransport` — submits tasks to daemon via IPC (CLI mode)
/// - `UnavailableAsyncTransport` — fail-fast with a clear error (no daemon reachable)
#[async_trait::async_trait]
pub trait AsyncTaskTransport: Send + Sync {
    /// Spawn a new async task
    ///
    /// For `LocalAsyncTransport`, this creates a placeholder task; use
    /// `spawn_task_boxed` for actual tool execution with a closure.
    /// For `DaemonIpcTransport`, this submits the task to the daemon via IPC.
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
    ///
    /// The default implementation delegates to `spawn_task` for transports that
    /// don't need the closure (e.g. IPC transport where the daemon executes
    /// the tool itself).
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
        // IPC transport uses this path because the daemon has its own ToolRuntime.
        self.spawn_task(task_id, tool_name, params, session_key, workspace, config)
            .await
    }

    /// Get the current status of a task
    async fn get_status(&self, task_id: &AsyncTaskId) -> Result<Option<AsyncTaskStatus>>;

    /// Cancel a running or pending task
    ///
    /// Returns `true` if the task was found and cancelled.
    async fn cancel_task(&self, task_id: &AsyncTaskId) -> Result<bool>;
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
}

impl LocalAsyncTransport {
    /// Get a reference to the underlying executor
    pub fn executor(&self) -> &AsyncExecutor {
        &self.executor
    }
}

// ================================================================================
// DaemonIpcTransport — used inside the CLI
// ================================================================================

/// IPC transport that submits tasks to the daemon via the
/// [`DaemonTransport`] projection.
///
/// `DaemonTransport` is the value-returning IPC projection owned by
/// the host crate (see `transport.rs`). Root implements it for
/// `Arc<DaemonClient>`; tests can substitute a mock.
///
/// Note: `DaemonTransport` is `dyn`-compatible but does not require
/// `Debug` (the host stays free of any `DaemonClient` leak), so the
/// `#[derive(Debug)]` below is omitted and we use a manual impl that
/// prints `<dyn DaemonTransport>` for the opaque field.
pub struct DaemonIpcTransport {
    client: Arc<dyn DaemonTransport>,
}

impl std::fmt::Debug for DaemonIpcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonIpcTransport")
            .field("client", &"<dyn DaemonTransport>")
            .finish()
    }
}

impl DaemonIpcTransport {
    /// Create a new IPC transport from a pre-built `Arc<dyn DaemonTransport>`.
    /// Root's CLI factory builds the client and supplies it via
    /// [`create_transport_with`]; tests can hand in a mock.
    pub fn new(client: Arc<dyn DaemonTransport>) -> Self {
        Self { client }
    }

    /// Check if the daemon is reachable
    pub async fn is_daemon_reachable(&self) -> bool {
        self.client.is_reachable().await
    }
}

#[async_trait::async_trait]
impl AsyncTaskTransport for DaemonIpcTransport {
    async fn spawn_task(
        &self,
        _task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        session_key: String,
        workspace: std::path::PathBuf,
        _config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        self.client
            .spawn_async_task(tool_name, params, session_key, workspace)
            .await
    }

    async fn get_status(&self, _task_id: &AsyncTaskId) -> Result<Option<AsyncTaskStatus>> {
        // The IPC `DaemonTransport` projection only exposes
        // `spawn_async_task` + `cancel_async_task`; status polling is
        // intentionally absent (the daemon's status channel is local
        // to its own `AsyncTaskRegistry`). Callers fall back to
        // `pending`/`running` while polling the task file the daemon
        // returned in the spawn receipt.
        Ok(None)
    }

    async fn cancel_task(&self, task_id: &AsyncTaskId) -> Result<bool> {
        self.client.cancel_async_task(task_id).await
    }
}

// ================================================================================
// UnavailableAsyncTransport — used when daemon is unreachable in CLI mode
// ================================================================================

/// Transport that always returns an error, used when async execution is unavailable.
///
/// This is used in CLI mode when the daemon is not running. Sync tools continue
/// to work, but async tools fail fast with a clear error message instead of
/// silently falling back to in-process execution (which would be dropped on CLI exit).
#[derive(Debug, Clone)]
pub struct UnavailableAsyncTransport {
    message: String,
}

impl UnavailableAsyncTransport {
    /// Create with a custom error message
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait::async_trait]
impl AsyncTaskTransport for UnavailableAsyncTransport {
    async fn spawn_task(
        &self,
        _task_id: AsyncTaskId,
        _tool_name: String,
        _params: Value,
        _session_key: String,
        _workspace: std::path::PathBuf,
        _config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        anyhow::bail!("{}", self.message)
    }

    async fn spawn_task_boxed(
        &self,
        _task_id: AsyncTaskId,
        _tool_name: String,
        _params: Value,
        _session_key: String,
        _workspace: std::path::PathBuf,
        _config: AsyncToolConfig,
        _execution_fn: BoxedExecutionFn,
    ) -> Result<AsyncTaskReceipt> {
        anyhow::bail!("{}", self.message)
    }

    async fn get_status(&self, _task_id: &AsyncTaskId) -> Result<Option<AsyncTaskStatus>> {
        anyhow::bail!("{}", self.message)
    }

    async fn cancel_task(&self, _task_id: &AsyncTaskId) -> Result<bool> {
        anyhow::bail!("{}", self.message)
    }
}

// ================================================================================
// Transport factory — caller provides the daemon client
// ================================================================================

/// Build a `DaemonIpcTransport` from an already-constructed
/// `Arc<dyn DaemonTransport>`.
///
/// # Why this shape
///
/// `LocalAsyncTransport` spawns tasks via `tokio::spawn`. When the CLI
/// exits, the tokio runtime shuts down and any spawned tasks are
/// dropped — they never complete. This produces "phantom success": the
/// agent receives a valid receipt, but the task was never executed.
/// The CLI therefore refuses to fall back to local execution; the
/// caller must construct the IPC client (via `ipc::ConnectionManager`)
/// and hand it in. Failing fast with a clear error is safer than
/// silently dropping work.
pub fn create_transport_with(client: Arc<dyn DaemonTransport>) -> Arc<dyn AsyncTaskTransport> {
    Arc::new(DaemonIpcTransport::new(client))
}

/// Create a local transport (for daemon mode where IPC is not needed).
///
/// Uses a shared registry from the global cache so the `task` tool
/// can find async tasks created by the router.
pub fn create_local_transport() -> Arc<dyn AsyncTaskTransport> {
    let registry = crate::async_exec::executor::get_or_create_registry_for_agent("_global");
    let queue_manager = Arc::new(tokio::sync::RwLock::new(
        crate::async_exec::executor::AsyncResultQueueManager::new(),
    ));
    let executor =
        crate::async_exec::executor::AsyncExecutor::with_registries(registry, queue_manager);
    Arc::new(LocalAsyncTransport::from_executor(executor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_transport_new() {
        let executor = AsyncExecutor::new();
        let transport = LocalAsyncTransport::from_executor(executor);
        let _ = transport.executor();
    }
}
