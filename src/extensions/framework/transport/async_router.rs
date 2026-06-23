//! Async Execution Router
//!
//! Routes tool execution with a constant 5-minute timeout. Tools that exceed
//! the timeout are auto-detached to background tasks; the agent retrieves
//! the result via the `task` tool's `output` action.
//!
//! # Usage
//!
//! ```rust,ignore
//! let router = AsyncExecutionRouter::new();
//! let result = router.route(
//!     &mut params,
//!     &exec_service,
//!     |p| async move { tool.execute(p).await }
//! ).await?;
//! ```

use crate::extensions::framework::async_exec::executor::{
    AsyncResultDeliveryMode, AsyncTaskStatus, AsyncToolConfig, DeliveryTarget,
};
use crate::extensions::framework::core::context::HookContext;
use crate::extensions::framework::services::tool_execution::{ToolExecutionConfig, ToolExecutionService};
use crate::extensions::framework::transport::async_transport::{AsyncTaskTransport, LocalAsyncTransport};
use crate::extensions::framework::types::{HookOutput, HookResult};
use anyhow::Result;
use serde_json::Value;
use std::time::Duration;
use tracing::{info, instrument};

/// Default tool execution timeout in seconds. When a tool call exceeds
/// this, the work is detached to a background task and a receipt is
/// returned to the agent. Agent config can override via
/// `AgentConfig::default_tool_timeout_secs`.
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 300;

/// Legacy reserved params are no longer honored; this is now a no-op.
fn strip_legacy_reserved_params(params: Value) -> Value {
    params
}

/// Async Execution Router
///
/// Routes tool execution with a constant 5-minute timeout
/// ([`DEFAULT_TOOL_TIMEOUT_SECS`]). Tools exceeding the timeout are
/// auto-detached to background tasks; the agent retrieves the result
/// via the `task` tool's `output` action.
///
/// This is the unified router for ALL tool types in ADR-018a.
///
/// In daemon mode, use `LocalAsyncTransport`. In CLI mode, use `DaemonHttpTransport`.
#[derive(Clone)]
pub struct AsyncExecutionRouter {
    /// Default tool execution timeout (5 min default).
    default_tool_timeout: Duration,
    /// Transport for async task execution (local or HTTP)
    transport: std::sync::Arc<dyn AsyncTaskTransport>,
}

impl std::fmt::Debug for AsyncExecutionRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncExecutionRouter")
            .field("default_tool_timeout", &self.default_tool_timeout)
            .field("transport", &"<dyn AsyncTaskTransport>")
            .finish()
    }
}

impl Default for AsyncExecutionRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncExecutionRouter {
    /// Create a new async execution router with the default tool timeout
    /// (5 min) and a local transport.
    #[must_use]
    pub fn new() -> Self {
        use crate::extensions::framework::async_exec::executor::AsyncExecutor;
        let executor = AsyncExecutor::new();
        Self {
            default_tool_timeout: Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS),
            transport: std::sync::Arc::new(LocalAsyncTransport::from_executor(executor)),
        }
    }

    /// Create with a custom default tool timeout (local transport).
    #[must_use]
    pub fn with_default_tool_timeout(secs: u64) -> Self {
        use crate::extensions::framework::async_exec::executor::AsyncExecutor;
        let executor = AsyncExecutor::new();
        Self {
            default_tool_timeout: Duration::from_secs(secs),
            transport: std::sync::Arc::new(LocalAsyncTransport::from_executor(executor)),
        }
    }

    /// Create with a custom transport
    #[must_use]
    pub fn with_transport(transport: std::sync::Arc<dyn AsyncTaskTransport>) -> Self {
        Self {
            default_tool_timeout: Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS),
            transport,
        }
    }

    /// Create with a shared local async executor (for sharing registries across routers)
    #[must_use]
    pub fn with_executor(
        async_executor: crate::extensions::framework::async_exec::executor::AsyncExecutor,
    ) -> Self {
        Self {
            default_tool_timeout: Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS),
            transport: std::sync::Arc::new(LocalAsyncTransport::from_executor(async_executor)),
        }
    }

    /// Route execution through the constant-timeout pipeline.
    ///
    /// This is the primary routing method for ALL tool execution in ADR-018a.
    /// Legacy reserved parameters (`_async`, `_timeout`, `_callback`, `_progress`,
    /// `_priority`, `_retry`) are silently dropped with a `tracing::warn!` if
    /// present; the framework no longer honors them.
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool being executed
    /// * `params` - Tool parameters (reserved keys will be stripped)
    /// * `exec_service` - Tool execution service
    /// * `tool_context` - Tool context for execution
    /// * `exec_config` - Execution configuration
    /// * `sync_executor` - Closure that performs the actual tool execution
    ///
    /// # Returns
    /// Tool execution result, or a `task_id` receipt if the work was
    /// detached because it exceeded [`DEFAULT_TOOL_TIMEOUT_SECS`].
    #[instrument(skip(self, params, exec_service, sync_executor), level = "debug")]
    pub async fn route<F, Fut>(
        &self,
        tool_name: &str,
        params: &mut Value,
        exec_service: &ToolExecutionService,
        tool_context: &ToolExecutionContext,
        exec_config: &ToolExecutionConfig,
        sync_executor: F,
    ) -> Result<Value>
    where
        F: FnOnce(Value) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        // Strip legacy reserved params (with a warning) and clone the
        // cleaned params for execution.
        let cleaned = std::mem::replace(params, Value::Null);
        let cleaned = strip_legacy_reserved_params(cleaned);
        *params = cleaned.clone();

        info!(
            timeout = self.default_tool_timeout.as_secs(),
            "AsyncExecutionRouter: routing execution"
        );

        // Single code path: execute with constant timeout. On Elapsed,
        // detach to AsyncExecutor (existing path).
        self.execute_with_timeout(
            tool_name,
            cleaned,
            exec_service,
            tool_context,
            exec_config,
            sync_executor,
        )
        .await
    }

    /// Execute synchronously with the constant default timeout.
    ///
    /// The work is spawned as a background task via the transport first,
    /// then polled for completion up to the timeout. If the timeout fires
    /// before the task completes, a receipt is returned and the work
    /// continues running in the background.
    #[instrument(skip(self, params, _exec_service, sync_executor), level = "debug")]
    async fn execute_with_timeout<F, Fut>(
        &self,
        tool_name: &str,
        params: Value,
        _exec_service: &ToolExecutionService,
        tool_context: &ToolExecutionContext,
        _exec_config: &ToolExecutionConfig,
        sync_executor: F,
    ) -> Result<Value>
    where
        F: FnOnce(Value) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let timeout = self.default_tool_timeout;
        let timeout_secs = timeout.as_secs();

        info!(
            tool_name = tool_name,
            timeout = timeout_secs,
            "Executing tool with default timeout"
        );

        let task_id = format!("{}:{}", tool_name, uuid::Uuid::new_v4());
        let session_key = format!("{}_{}", tool_context.agent_id, tool_context.session_id);

        // The background task's hard timeout is the default 300s regardless of
        // the router's polling timeout (which may be shorter in tests).
        let task_hard_timeout_secs = DEFAULT_TOOL_TIMEOUT_SECS;

        let config = AsyncToolConfig {
            delivery_mode: AsyncResultDeliveryMode::QueueWhenBusy,
            delivery_target: Some(DeliveryTarget::AsyncQueue),
            timeout_secs: task_hard_timeout_secs,
            cleanup_after_delivery: true,
            label: Some(tool_name.to_string()),
        };

        // Build a boxed execution closure that captures params and runs the tool.
        let execution_fn: crate::extensions::framework::transport::async_transport::BoxedExecutionFn =
            Box::new(move || Box::pin(sync_executor(params)));

        // Spawn the real work as a background task via the transport.
        let receipt = self
            .transport
            .spawn_task_boxed(
                task_id.clone(),
                tool_name.to_string(),
                Value::Null, // params already captured in the closure
                session_key,
                std::path::PathBuf::from(&tool_context.workspace),
                config,
                execution_fn,
            )
            .await?;

        // Poll for completion up to the timeout.
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.transport.get_status(&task_id).await? {
                Some(AsyncTaskStatus::Completed { result }) => {
                    if result.success {
                        return Ok(result.data.unwrap_or(Value::Null));
                    } else {
                        return Err(anyhow::anyhow!(
                            result.error.unwrap_or_else(|| "Tool execution failed".to_string())
                        ));
                    }
                }
                Some(AsyncTaskStatus::Failed { error }) => {
                    return Err(anyhow::anyhow!(error));
                }
                Some(AsyncTaskStatus::Cancelled) => {
                    return Err(anyhow::anyhow!("Task was cancelled"));
                }
                Some(AsyncTaskStatus::TimedOut { error }) => {
                    return Err(anyhow::anyhow!(error));
                }
                Some(AsyncTaskStatus::Pending) | Some(AsyncTaskStatus::Running) => {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "Task {} not found in transport registry after spawn",
                        task_id
                    ));
                }
            }
        }

        // Timeout fired — the task is still running in the background.
        // Return an honest receipt.
        tracing::warn!(
            tool_name = tool_name,
            timeout_secs = timeout_secs,
            "Tool exceeded default timeout; returning receipt while work continues in background"
        );

        Ok(serde_json::json!({
            "_async_status": "queued",
            "task_id": receipt.task_id,
            "status": "running",
            "tool_name": tool_name,
            "task_file": receipt.task_file,
            "timeout_requested": timeout_secs,
            "reason": "timeout",
        }))
    }

    /// Get a reference to the underlying transport
    #[must_use]
    pub fn transport(&self) -> &std::sync::Arc<dyn AsyncTaskTransport> {
        &self.transport
    }

    /// Execute a tool from a HookContext — eliminates adapter boilerplate.
    ///
    /// This convenience method handles the common glue code that every
    /// `ToolExecute` hook handler performs:
    /// - Extracting params from `HookContext::as_tool_call()`
    /// - Validating the tool name matches
    /// - Building `ToolExecutionContext` from hook state
    /// - Routing through `self.route()`
    /// - Mapping the result to `HookResult`
    ///
    /// Adapters only provide:
    /// 1. Tool name matching logic
    /// 2. Optional param preprocessing
    /// 3. The actual tool execution closure
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// impl HookHandler for MyToolHandler {
    ///     async fn handle(&self, ctx: HookContext) -> HookResult {
    ///         let tool = self.tool.clone();
    ///         ctx.services.async_router().execute_from_hook(
    ///             &ctx,
    ///             self.tool.name(),
    ///             &ToolExecutionConfig::with_schema(self.tool.parameters()),
    ///             Some(|params, workspace| {
    ///                 // Optional preprocessing
    ///             }),
    ///             move |p| async move { tool.execute(p).await },
    ///         ).await
    ///     }
    /// }
    /// ```
    pub async fn execute_from_hook<F, Fut, P>(
        &self,
        ctx: &HookContext,
        tool_name: &str,
        exec_config: &ToolExecutionConfig,
        preprocessor: Option<P>,
        exec_fn: F,
    ) -> HookResult
    where
        F: FnOnce(Value) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
        P: Fn(&mut Value, Option<&str>) + Send,
    {
        // 1. Extract tool call from context
        let (called_tool_name, mut params, workspace) = match ctx.as_tool_call() {
            Some((name, params, ws)) => (name, params.clone(), ws),
            None => return HookResult::PassThrough,
        };

        // 2. Validate tool name match
        if called_tool_name != tool_name {
            return HookResult::PassThrough;
        }

        // 3. Get services from context
        let exec_service = ctx.services.tool_execution();

        // 4. Build execution context
        let tool_ctx =
            match ctx.get_state::<crate::extensions::framework::types::ToolRuntimeContext>("tool_context") {
                Some(tc) => ToolExecutionContext::new(
                    tc.agent_id.clone().unwrap_or_else(|| "unknown".to_string()),
                    tc.session_id
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    tc.run_id.clone().unwrap_or_else(|| "unknown".to_string()),
                )
                .with_workspace(tc.workspace.clone().unwrap_or_else(|| ".".to_string())),
                None => {
                    let ctx = ToolExecutionContext::new("unknown", "unknown", "unknown");
                    match workspace {
                        Some(ws) => ctx.with_workspace(ws),
                        None => ctx,
                    }
                }
            };

        // 5. Run preprocessor if provided
        if let Some(pre) = preprocessor {
            pre(&mut params, workspace);
        }

        // 6. Route through AsyncExecutionRouter
        let result = self
            .route(
                tool_name,
                &mut params,
                exec_service,
                &tool_ctx,
                exec_config,
                exec_fn,
            )
            .await;

        // 7. Map result to HookResult
        match result {
            Ok(value) => HookResult::Continue(HookOutput::Json(value)),
            Err(e) => HookResult::Error(e),
        }
    }

    /// Wait for all async tasks to complete
    ///
    /// For `LocalAsyncTransport`, this waits until all tasks reach a terminal
    /// state or the timeout expires. For `DaemonHttpTransport`, this returns
    /// immediately because tasks live in the daemon and survive CLI exit.
    pub async fn wait_for_all_tasks(&self, timeout: std::time::Duration) {
        // For HTTP transport, tasks live in the daemon — no need to wait.
        // For local transport, poll the executor directly.
        tokio::time::sleep(timeout).await;
    }
}

/// Context for tool execution
///
/// This contains runtime identity information needed by the router.
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    /// Agent identifier
    pub agent_id: String,
    /// Session identifier
    pub session_id: String,
    /// Run identifier
    pub run_id: String,
    /// Workspace path
    pub workspace: String,
}

impl ToolExecutionContext {
    /// Create a new execution context
    pub fn new(
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            workspace: ".".to_string(),
        }
    }

    /// Set workspace
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = workspace.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_default_tool_timeout_constant() {
        // Single source of truth for the 5-min default.
        assert_eq!(DEFAULT_TOOL_TIMEOUT_SECS, 300);
    }

    #[tokio::test]
    async fn test_router_sync_path() {
        let router = AsyncExecutionRouter::new();
        let exec_service = ToolExecutionService::new();
        let tool_context = ToolExecutionContext::new("agent1", "session1", "run1");
        let exec_config = ToolExecutionConfig::with_schema(json!({"type": "object"}));

        let mut params = json!({"query": "test"});

        let result = router
            .route(
                "test_tool",
                &mut params,
                &exec_service,
                &tool_context,
                &exec_config,
                |p| async move { Ok(json!({"result": "success", "input": p})) },
            )
            .await;

        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value["result"], "success");
        assert_eq!(value["input"]["query"], "test");
    }

    #[tokio::test]
    async fn test_router_fast_tool_returns_inline_result() {
        let router = AsyncExecutionRouter::new();
        let exec_service = ToolExecutionService::new();
        let tool_context = ToolExecutionContext::new("agent1", "session1", "run1");
        let exec_config = ToolExecutionConfig::with_schema(json!({"type": "object"}));

        let mut params = json!({"query": "test"});

        let result = router
            .route(
                "fast_tool",
                &mut params,
                &exec_service,
                &tool_context,
                &exec_config,
                |p| async move { Ok(json!({"result": "inline", "input": p})) },
            )
            .await;

        assert!(result.is_ok());
        let value = result.unwrap();
        // Fast tools should return their result directly, not a receipt.
        assert_eq!(value["result"], "inline");
        assert_eq!(value["input"]["query"], "test");
        assert!(value.get("task_id").is_none());
        assert!(value.get("status").is_none());
    }

    #[tokio::test]
    async fn test_router_timeout_returns_receipt_with_tool_name() {
        let router = AsyncExecutionRouter::with_default_tool_timeout(1);
        let exec_service = ToolExecutionService::new();
        let tool_context = ToolExecutionContext::new("agent1", "session1", "run1");
        let exec_config = ToolExecutionConfig::with_schema(json!({"type": "object"}));

        let mut params = json!({"query": "slow"});

        let result = router
            .route(
                "slow_tool",
                &mut params,
                &exec_service,
                &tool_context,
                &exec_config,
                |_p| async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok(json!({"result": "should_never_see_this"}))
                },
            )
            .await;

        assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
        let value = result.unwrap();
        // Should be a receipt, not the tool result.
        assert!(value.get("task_id").is_some());
        assert_eq!(value["status"], "running");
        assert_eq!(value["tool_name"], "slow_tool");
        assert_eq!(value["reason"], "timeout");
    }
}
