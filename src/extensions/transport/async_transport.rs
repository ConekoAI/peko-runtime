//! Async Task Transport Abstraction (ADR-020 Phase 3)
//!
//! Provides a trait-based abstraction over async task execution so that the
//! `AsyncExecutionRouter` can work identically whether it is running inside the
//! daemon (local execution) or inside the CLI (HTTP submission to daemon).

use crate::extensions::async_exec::executor::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig,
};
use anyhow::Result;
use serde_json::Value;

/// Boxed async execution closure type
///
/// Returns `Value` directly — tool-specific formatting is handled at delivery time.
pub type BoxedExecutionFn = Box<
    dyn FnOnce() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Value>> + Send>,
        > + Send,
>;

/// Transport abstraction for async task execution
///
/// Implementations:
/// - `LocalAsyncTransport` — runs tasks in-process via `AsyncExecutor` (daemon mode)
/// - `DaemonHttpTransport` — submits tasks to daemon via HTTP API (CLI mode)
#[async_trait::async_trait]
pub trait AsyncTaskTransport: Send + Sync {
    /// Spawn a new async task
    ///
    /// For `LocalAsyncTransport`, this creates a placeholder task; use
    /// `spawn_task_boxed` for actual tool execution with a closure.
    /// For `DaemonHttpTransport`, this submits the task to the daemon via HTTP.
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
    /// don't need the closure (e.g. HTTP transport where the daemon executes
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
        // HTTP transport uses this path because the daemon has its own ToolRuntime.
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

use crate::extensions::async_exec::executor::AsyncExecutor;
use std::sync::Arc;

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

use crate::ipc::{DaemonClient, ResponsePacket};

/// IPC transport that submits tasks to the daemon via UDP/Unix socket
#[derive(Debug)]
pub struct DaemonIpcTransport {
    client: DaemonClient,
}

impl DaemonIpcTransport {
    /// Create a new IPC transport (connects to daemon, fails if not running)
    pub async fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: DaemonClient::connect().await?,
        })
    }

    /// Check if the daemon is reachable
    pub async fn is_daemon_reachable(&self) -> bool {
        self.client.is_running().await
    }
}

#[async_trait::async_trait]
impl AsyncTaskTransport for DaemonIpcTransport {
    async fn spawn_task(
        &self,
        task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        session_key: String,
        workspace: std::path::PathBuf,
        _config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        let mut stream = self
            .client
            .spawn_async_task(tool_name, params, session_key, workspace)
            .await?;

        // Wait for the async receipt response
        while let Some(packet) = stream.next().await {
            match packet {
                ResponsePacket::AsyncReceipt { receipt, .. } => return Ok(receipt),
                ResponsePacket::Error { message, .. } => anyhow::bail!(message),
                ResponsePacket::Done { success, error, .. } => {
                    if !success {
                        anyhow::bail!(error.unwrap_or_else(|| "Async spawn failed".to_string()));
                    }
                }
                _ => {}
            }
        }

        anyhow::bail!("Stream closed without receipt for task {}", task_id)
    }

    async fn get_status(&self, _task_id: &AsyncTaskId) -> Result<Option<AsyncTaskStatus>> {
        // TODO: Implement status check via IPC
        // For now, return None to trigger fallback behavior
        Ok(None)
    }

    async fn cancel_task(&self, task_id: &AsyncTaskId) -> Result<bool> {
        let mut stream = self.client.cancel_async_task(task_id).await?;

        while let Some(packet) = stream.next().await {
            match packet {
                ResponsePacket::Done { success, .. } => return Ok(success),
                ResponsePacket::Error { message, .. } => anyhow::bail!(message),
                _ => {}
            }
        }

        anyhow::bail!(
            "Stream closed without cancel confirmation for task {}",
            task_id
        )
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
// Transport factory — detects daemon and chooses transport
// ================================================================================

/// Create the appropriate transport for CLI mode
///
/// - If the daemon is reachable via IPC, returns `DaemonIpcTransport`
/// - Otherwise, returns an error — async tool execution requires the daemon
///
/// # Why no fallback?
///
/// `LocalAsyncTransport` spawns tasks via `tokio::spawn`. When the CLI exits,
/// the tokio runtime shuts down and any spawned tasks are dropped — they never
/// complete. This produces "phantom success": the agent receives a valid receipt,
/// but the task was never executed. Failing fast with a clear error is safer.
pub async fn create_transport() -> anyhow::Result<std::sync::Arc<dyn AsyncTaskTransport>> {
    // Use try_connect_quick (200ms timeout, no auto-start) to avoid hanging
    // on daemon-unreachable commands like `agent list`.
    let client = match crate::ipc::ConnectionManager::try_connect_quick().await {
        Ok(conn) => crate::ipc::DaemonClient::with_connection(conn).await?,
        Err(e) => {
            anyhow::bail!(
                "Pekobot daemon is not running. Async tool execution requires the daemon.\n\
                 Start it with: pekobot daemon start\n\
                 Or use sync mode (remove _async: true from the tool call).\n\
                 Details: {e}"
            )
        }
    };

    let ipc = DaemonIpcTransport { client };

    if ipc.is_daemon_reachable().await {
        tracing::info!("Using DaemonIpcTransport for async tasks (daemon is running)");
        Ok(std::sync::Arc::new(ipc))
    } else {
        anyhow::bail!(
            "Pekobot daemon is not running. Async tool execution requires the daemon.\n\
             Start it with: pekobot daemon start\n\
             Or use sync mode (remove _async: true from the tool call)."
        )
    }
}

/// Create a local transport (for daemon mode where HTTP is not needed)
pub fn create_local_transport() -> std::sync::Arc<dyn AsyncTaskTransport> {
    // Use a shared registry from the global cache so that the `task` tool can
    // find async tasks created by the router.
    let registry = crate::extensions::async_exec::executor::get_or_create_registry_for_agent("_global");
    let queue_manager =
        std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::extensions::async_exec::executor::AsyncResultQueueManager::new(),
        ));
    let executor =
        crate::extensions::async_exec::executor::AsyncExecutor::with_registries(registry, queue_manager);
    std::sync::Arc::new(LocalAsyncTransport::from_executor(executor))
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
