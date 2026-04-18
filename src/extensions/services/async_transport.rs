//! Async Task Transport Abstraction (ADR-020 Phase 3)
//!
//! Provides a trait-based abstraction over async task execution so that the
//! `AsyncExecutionRouter` can work identically whether it is running inside the
//! daemon (local execution) or inside the CLI (HTTP submission to daemon).

use crate::agent::async_tool_framework::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig,
};
use anyhow::Result;
use serde_json::Value;

/// Boxed async execution closure type
pub type BoxedExecutionFn = Box<
    dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AsyncTaskResult>> + Send>>
        + Send,
>;

/// Transport abstraction for async task execution
///
/// Implementations:
/// - `LocalAsyncTransport` — runs tasks in-process via `UnifiedAsyncExecutor` (daemon mode)
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

use crate::agent::async_tool_framework::UnifiedAsyncExecutor;
use std::sync::Arc;

/// Local transport that executes tasks in-process via `UnifiedAsyncExecutor`
#[derive(Debug, Clone)]
pub struct LocalAsyncTransport {
    executor: Arc<UnifiedAsyncExecutor>,
}

impl LocalAsyncTransport {
    /// Create a new local transport wrapping the given executor
    pub fn new(executor: Arc<UnifiedAsyncExecutor>) -> Self {
        Self { executor }
    }

    /// Create from a bare `UnifiedAsyncExecutor`
    pub fn from_executor(executor: UnifiedAsyncExecutor) -> Self {
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
            .execute_boxed(task_id, tool_name, params, session_key, config, execution_fn)
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
    pub fn executor(&self) -> &UnifiedAsyncExecutor {
        &self.executor
    }
}

// ================================================================================
// DaemonHttpTransport — used inside the CLI
// ================================================================================

use crate::api::client::ApiClient;
use crate::api::routes::async_tasks::SpawnAsyncTaskRequest;

/// HTTP transport that submits tasks to the daemon via `ApiClient`
#[derive(Debug, Clone)]
pub struct DaemonHttpTransport {
    client: ApiClient,
}

impl DaemonHttpTransport {
    /// Create a new HTTP transport with the default daemon address
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: ApiClient::new()?,
        })
    }

    /// Create with a specific daemon address
    pub fn with_addr(addr: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: ApiClient::with_addr(addr)?,
        })
    }

    /// Check if the daemon is reachable
    pub async fn is_daemon_reachable(&self) -> bool {
        self.client.health_check().await.is_ok()
    }
}

#[async_trait::async_trait]
impl AsyncTaskTransport for DaemonHttpTransport {
    async fn spawn_task(
        &self,
        task_id: AsyncTaskId,
        tool_name: String,
        params: Value,
        session_key: String,
        workspace: std::path::PathBuf,
        config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        let req = SpawnAsyncTaskRequest {
            task_id,
            tool_name,
            params,
            session_key,
            workspace,
            config,
        };
        let receipt = self.client.spawn_async_task(&req).await?;
        Ok(receipt)
    }

    async fn get_status(&self, task_id: &AsyncTaskId) -> Result<Option<AsyncTaskStatus>> {
        let resp = self.client.get_async_task(task_id).await?;
        // Map the HTTP response status string back to AsyncTaskStatus
        let status = match resp.status.as_str() {
            "pending" => Some(AsyncTaskStatus::Pending),
            "running" => Some(AsyncTaskStatus::Running),
            "cancelled" => Some(AsyncTaskStatus::Cancelled),
            "completed" => {
                if let Some(result) = resp.result {
                    Some(AsyncTaskStatus::Completed {
                        result: crate::tools::traits::ToolResult::success(result),
                    })
                } else {
                    Some(AsyncTaskStatus::Completed {
                        result: crate::tools::traits::ToolResult::success(serde_json::json!({})),
                    })
                }
            }
            "failed" => Some(AsyncTaskStatus::Failed {
                error: resp.error.unwrap_or_else(|| "Unknown error".to_string()),
            }),
            "timed_out" => Some(AsyncTaskStatus::TimedOut {
                error: resp.error.unwrap_or_else(|| "Timed out".to_string()),
            }),
            _ => None,
        };
        Ok(status)
    }

    async fn cancel_task(&self, task_id: &AsyncTaskId) -> Result<bool> {
        let cancelled = self.client.cancel_async_task(task_id).await?;
        Ok(cancelled)
    }
}

impl Default for DaemonHttpTransport {
    fn default() -> Self {
        Self::new().expect("Failed to create DaemonHttpTransport")
    }
}

// ================================================================================
// Transport factory — detects daemon and chooses transport
// ================================================================================

/// Create the appropriate transport for CLI mode
///
/// - If the daemon is reachable, returns `DaemonHttpTransport`
/// - Otherwise, returns an error — async tool execution requires the daemon
///
/// # Why no fallback?
///
/// `LocalAsyncTransport` spawns tasks via `tokio::spawn`. When the CLI exits,
/// the tokio runtime shuts down and any spawned tasks are dropped — they never
/// complete. This produces "phantom success": the agent receives a valid receipt,
/// but the task was never executed. Failing fast with a clear error is safer.
pub async fn create_transport() -> anyhow::Result<std::sync::Arc<dyn AsyncTaskTransport>> {
    let http = DaemonHttpTransport::new()
        .map_err(|e| anyhow::anyhow!("Failed to create daemon HTTP client: {e}"))?;

    if http.is_daemon_reachable().await {
        tracing::info!("Using DaemonHttpTransport for async tasks (daemon is running)");
        Ok(std::sync::Arc::new(http))
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
    std::sync::Arc::new(LocalAsyncTransport::from_executor(UnifiedAsyncExecutor::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_transport_new() {
        let executor = UnifiedAsyncExecutor::new();
        let transport = LocalAsyncTransport::from_executor(executor);
        let _ = transport.executor();
    }

    #[test]
    fn test_daemon_http_transport_default_addr() {
        let transport = DaemonHttpTransport::new();
        assert!(transport.is_ok());
    }
}
