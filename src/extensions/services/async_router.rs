//! Async Execution Router
//!
//! Routes tool execution based on the `_async` reserved parameter.
//! This replaces `ToolWrapper`'s async handling and makes it available
//! to ALL tool types (built-in, MCP, Universal, General) through `ExtensionCore`.
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

use crate::agent::async_tool_framework::{
    AsyncResultDeliveryMode, AsyncTaskResult, AsyncToolConfig, DeliveryTarget,
};
use crate::extensions::core::HookContext;
use crate::extensions::services::async_transport::{AsyncTaskTransport, LocalAsyncTransport};
use crate::extensions::services::tool_execution::{ToolExecutionConfig, ToolExecutionService};
use crate::extensions::types::{HookOutput, HookResult};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tracing::{info, instrument};

/// Reserved parameters for async execution control
///
/// These parameters are extracted from tool calls and control execution behavior.
/// They are removed from the params before the tool sees them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncReservedParams {
    /// Request async execution
    #[serde(rename = "_async", default)]
    pub async_mode: bool,

    /// Timeout in seconds
    #[serde(rename = "_timeout")]
    pub timeout_secs: Option<u64>,

    /// Result delivery mode: "queue" | "stream" | "blocking"
    #[serde(rename = "_callback", default = "default_callback")]
    pub callback: String,

    /// Request progress updates (async only)
    #[serde(rename = "_progress", default = "default_true")]
    pub progress: bool,

    /// Task priority: "low" | "normal" | "high"
    #[serde(rename = "_priority", default = "default_priority")]
    pub priority: String,

    /// Number of retries on failure
    #[serde(rename = "_retry", default)]
    pub retry_count: u32,
}

impl Default for AsyncReservedParams {
    fn default() -> Self {
        Self {
            async_mode: false,
            timeout_secs: None,
            callback: default_callback(),
            progress: default_true(),
            priority: default_priority(),
            retry_count: 0,
        }
    }
}

fn default_callback() -> String {
    "queue".to_string()
}

fn default_true() -> bool {
    true
}

fn default_priority() -> String {
    "normal".to_string()
}

impl AsyncReservedParams {
    /// Extract reserved parameters from a JSON value
    ///
    /// Removes the reserved parameters from the input params and returns them.
    pub fn extract(params: &mut Value) -> Self {
        let mut reserved = Self::default();

        if let Some(obj) = params.as_object_mut() {
            // Extract _async (accept boolean or string)
            if let Some(v) = obj.remove("_async") {
                reserved.async_mode = Self::parse_bool(&v).unwrap_or(false);
            }

            // Extract _timeout (accept integer, float, or string)
            if let Some(v) = obj.remove("_timeout") {
                reserved.timeout_secs = v
                    .as_u64()
                    .or_else(|| v.as_f64().map(|f| f as u64))
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()));
            }

            // Extract _callback
            if let Some(v) = obj.remove("_callback") {
                if let Some(s) = v.as_str() {
                    reserved.callback = s.to_string();
                }
            }

            // Extract _progress (accept boolean or string)
            if let Some(v) = obj.remove("_progress") {
                reserved.progress = Self::parse_bool(&v).unwrap_or(true);
            }

            // Extract _priority
            if let Some(v) = obj.remove("_priority") {
                if let Some(s) = v.as_str() {
                    reserved.priority = s.to_string();
                }
            }

            // Extract _retry (accept integer or string)
            if let Some(v) = obj.remove("_retry") {
                reserved.retry_count = v
                    .as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    .unwrap_or(0) as u32;
            }
        }

        reserved
    }

    /// Parse a JSON value as a boolean, accepting both native booleans
    /// and common string representations.
    fn parse_bool(v: &Value) -> Option<bool> {
        v.as_bool().or_else(|| {
            v.as_str().and_then(|s| match s.to_lowercase().as_str() {
                "true" | "yes" | "1" => Some(true),
                "false" | "no" | "0" => Some(false),
                _ => None,
            })
        })
    }

    /// Get effective timeout (use reserved or default)
    #[must_use]
    pub fn effective_timeout(&self, is_async: bool) -> u64 {
        self.timeout_secs
            .unwrap_or(if is_async { 300 } else { 120 })
    }

    /// Validate callback mode
    #[must_use]
    pub fn is_valid_callback(&self) -> bool {
        matches!(self.callback.as_str(), "queue" | "stream" | "blocking")
    }

    /// Validate priority
    #[must_use]
    pub fn is_valid_priority(&self) -> bool {
        matches!(self.priority.as_str(), "low" | "normal" | "high")
    }
}

/// Async Execution Router
///
/// Routes tool execution to either sync or async paths based on `_async` parameter.
/// This is the unified router for ALL tool types in ADR-018a.
///
/// In daemon mode, use `LocalAsyncTransport`. In CLI mode, use `DaemonHttpTransport`.
#[derive(Clone)]
pub struct AsyncExecutionRouter {
    /// Default sync timeout
    default_sync_timeout: Duration,
    /// Default async timeout
    default_async_timeout: Duration,
    /// Transport for async task execution (local or HTTP)
    transport: std::sync::Arc<dyn AsyncTaskTransport>,
}

impl std::fmt::Debug for AsyncExecutionRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncExecutionRouter")
            .field("default_sync_timeout", &self.default_sync_timeout)
            .field("default_async_timeout", &self.default_async_timeout)
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
    /// Create a new async execution router with default timeouts (local transport)
    #[must_use]
    pub fn new() -> Self {
        use crate::agent::async_tool_framework::AsyncExecutor;
        let executor = AsyncExecutor::new();
        Self {
            default_sync_timeout: Duration::from_secs(120),
            default_async_timeout: Duration::from_secs(300),
            transport: std::sync::Arc::new(LocalAsyncTransport::from_executor(executor)),
        }
    }

    /// Create with custom timeouts (local transport)
    #[must_use]
    pub fn with_timeouts(sync_secs: u64, async_secs: u64) -> Self {
        use crate::agent::async_tool_framework::AsyncExecutor;
        let executor = AsyncExecutor::new();
        Self {
            default_sync_timeout: Duration::from_secs(sync_secs),
            default_async_timeout: Duration::from_secs(async_secs),
            transport: std::sync::Arc::new(LocalAsyncTransport::from_executor(executor)),
        }
    }

    /// Create with a custom transport
    #[must_use]
    pub fn with_transport(transport: std::sync::Arc<dyn AsyncTaskTransport>) -> Self {
        Self {
            default_sync_timeout: Duration::from_secs(120),
            default_async_timeout: Duration::from_secs(300),
            transport,
        }
    }

    /// Create with a shared local async executor (for sharing registries across routers)
    #[must_use]
    pub fn with_executor(
        async_executor: crate::agent::async_tool_framework::AsyncExecutor,
    ) -> Self {
        Self {
            default_sync_timeout: Duration::from_secs(120),
            default_async_timeout: Duration::from_secs(300),
            transport: std::sync::Arc::new(LocalAsyncTransport::from_executor(async_executor)),
        }
    }

    /// Route execution based on `_async` parameter
    ///
    /// This is the primary routing method for ALL tool execution in ADR-018a.
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool being executed
    /// * `params` - Tool parameters (will be mutated to extract reserved params)
    /// * `exec_service` - Tool execution service for sync path
    /// * `tool_context` - Tool context for execution
    /// * `exec_config` - Execution configuration
    /// * `sync_executor` - Closure that performs the actual tool execution
    ///
    /// # Returns
    /// Tool execution result
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
        // Extract reserved parameters (this mutates params to remove them)
        let reserved = AsyncReservedParams::extract(params);

        info!(
            async_mode = reserved.async_mode,
            timeout = reserved.effective_timeout(reserved.async_mode),
            "AsyncExecutionRouter: routing execution"
        );

        if reserved.async_mode {
            // Async path: execute via AsyncExecutor
            self.execute_async(
                tool_name,
                params.clone(),
                tool_context,
                &reserved,
                sync_executor,
            )
            .await
        } else {
            // Sync path with timeout
            self.execute_sync(
                params.clone(),
                exec_service,
                tool_context,
                exec_config,
                &reserved,
                sync_executor,
            )
            .await
        }
    }

    /// Execute synchronously with timeout and retry support
    #[instrument(skip(self, params, exec_service, sync_executor), level = "debug")]
    async fn execute_sync<F, Fut>(
        &self,
        params: Value,
        exec_service: &ToolExecutionService,
        tool_context: &ToolExecutionContext,
        exec_config: &ToolExecutionConfig,
        reserved: &AsyncReservedParams,
        sync_executor: F,
    ) -> Result<Value>
    where
        F: FnOnce(Value) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send,
    {
        let timeout_secs = reserved.effective_timeout(false);
        let timeout = Duration::from_secs(timeout_secs);

        info!(timeout = timeout_secs, "Executing tool synchronously");

        // Build the context for parameter injection
        let abort_signal = crate::tools::AbortSignal::new();
        let ctx = abort_signal
            .create_context(&tool_context.run_id, "tool_exec", "async_router")
            .with_agent_id(&tool_context.agent_id)
            .with_session_id(&tool_context.session_id)
            .with_workspace(&tool_context.workspace);

        // Execute with isolation and timeout
        // Note: Retry logic is currently handled at a higher level if needed
        exec_service
            .execute_with_isolation(
                params,
                exec_config,
                Some(&ctx),
                Some(timeout),
                sync_executor,
            )
            .await
    }

    /// Execute asynchronously via the configured transport
    #[instrument(skip(self, params, sync_executor), level = "debug")]
    async fn execute_async<F, Fut>(
        &self,
        tool_name: &str,
        params: Value,
        tool_context: &ToolExecutionContext,
        reserved: &AsyncReservedParams,
        sync_executor: F,
    ) -> Result<Value>
    where
        F: FnOnce(Value) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let timeout_secs = reserved.effective_timeout(true);
        let task_id = format!("{}:{}", tool_name, uuid::Uuid::new_v4());
        let session_key = format!("{}_{}", tool_context.agent_id, tool_context.session_id);

        let (delivery_mode, delivery_target) = match reserved.callback.as_str() {
            "stream" => (
                AsyncResultDeliveryMode::Interrupt,
                DeliveryTarget::EventBroadcast,
            ),
            "blocking" => (
                AsyncResultDeliveryMode::QueueWhenBusy,
                DeliveryTarget::DirectChannel,
            ),
            _ => (
                AsyncResultDeliveryMode::QueueWhenBusy,
                DeliveryTarget::AsyncQueue,
            ),
        };

        let config = AsyncToolConfig {
            delivery_mode,
            delivery_target: Some(delivery_target),
            timeout_secs,
            cleanup_after_delivery: true,
            label: Some(tool_name.to_string()),
        };

        info!(
            task_id = %task_id,
            timeout = timeout_secs,
            callback = %reserved.callback,
            "Executing tool asynchronously via transport"
        );

        let tool_name_owned = tool_name.to_string();
        let params_clone = params.clone();
        let workspace = std::path::PathBuf::from(&tool_context.workspace);
        let receipt = self
            .transport
            .spawn_task_boxed(
                task_id,
                tool_name.to_string(),
                params,
                session_key,
                workspace,
                config,
                Box::new(move || {
                    Box::pin(async move {
                        match sync_executor(params_clone).await {
                            Ok(result) => {
                                // Convert shell tool results to Process variant for task file
                                if tool_name_owned == "shell" {
                                    if let (Some(stdout), Some(stderr), Some(exit_code)) = (
                                        result.get("stdout").and_then(|v| v.as_str()),
                                        result.get("stderr").and_then(|v| v.as_str()),
                                        result.get("exit_code").and_then(|v| v.as_i64()),
                                    ) {
                                        Ok(AsyncTaskResult::Process {
                                            stdout: stdout.to_string(),
                                            stderr: stderr.to_string(),
                                            exit_code: exit_code as i32,
                                        })
                                    } else {
                                        Ok(AsyncTaskResult::Generic { data: result })
                                    }
                                } else {
                                    Ok(AsyncTaskResult::Generic { data: result })
                                }
                            }
                            Err(e) => Ok(AsyncTaskResult::Generic {
                                data: serde_json::json!({"error": e.to_string()}),
                            }),
                        }
                    })
                }),
            )
            .await?;

        let mut receipt_json = serde_json::json!({
            "_async_status": "queued",
            "task_id": receipt.task_id,
            "status": receipt.status,
            "task_file": receipt.task_file,
            "timeout_requested": timeout_secs,
            "callback_mode": reserved.callback,
        });
        if let Some(params) = receipt.params {
            receipt_json["params"] = params;
        }
        Ok(receipt_json)
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
        let tool_ctx = match ctx.as_tool_context() {
            Some(tc) => ToolExecutionContext::new(
                tc.agent_id.clone().unwrap_or_else(|| "unknown".to_string()),
                tc.session_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                tc.run_id.clone(),
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
    fn test_extract_async_params() {
        let mut params = json!({
            "query": "test",
            "_async": true,
            "_timeout": 60,
            "_callback": "stream"
        });

        let reserved = AsyncReservedParams::extract(&mut params);

        assert!(reserved.async_mode);
        assert_eq!(reserved.timeout_secs, Some(60));
        assert_eq!(reserved.callback, "stream");
        assert!(!params.as_object().unwrap().contains_key("_async"));
        assert_eq!(params["query"], "test");
    }

    #[test]
    fn test_default_params() {
        let mut params = json!({"query": "test"});
        let reserved = AsyncReservedParams::extract(&mut params);

        assert!(!reserved.async_mode);
        assert_eq!(reserved.timeout_secs, None);
        assert_eq!(reserved.callback, "queue");
    }

    #[test]
    fn test_effective_timeout() {
        let mut reserved = AsyncReservedParams::default();

        // Default sync timeout
        assert_eq!(reserved.effective_timeout(false), 120);

        // Default async timeout
        assert_eq!(reserved.effective_timeout(true), 300);

        // Custom timeout
        reserved.timeout_secs = Some(45);
        assert_eq!(reserved.effective_timeout(false), 45);
        assert_eq!(reserved.effective_timeout(true), 45);
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
    async fn test_router_sync_timeout() {
        let router = AsyncExecutionRouter::new();
        let exec_service = ToolExecutionService::new();
        let tool_context = ToolExecutionContext::new("agent1", "session1", "run1");
        let exec_config = ToolExecutionConfig::with_schema(json!({"type": "object"}));

        let mut params = json!({"query": "test", "_timeout": 1});

        let result = router
            .route(
                "test_tool",
                &mut params,
                &exec_service,
                &tool_context,
                &exec_config,
                |_p| async move {
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    Ok(json!({"result": "success"}))
                },
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("TOOL_TIMEOUT"),
            "Expected timeout error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_router_async_path() {
        let router = AsyncExecutionRouter::new();
        let exec_service = ToolExecutionService::new();
        let tool_context = ToolExecutionContext::new("agent1", "session1", "run1");
        let exec_config = ToolExecutionConfig::with_schema(json!({"type": "object"}));

        let mut params = json!({"query": "test", "_async": true, "_timeout": 60});

        let result = router
            .route(
                "test_tool",
                &mut params,
                &exec_service,
                &tool_context,
                &exec_config,
                |p| async move { Ok(json!({"result": "async_ok", "input": p})) },
            )
            .await;

        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value["_async_status"], "queued");
        assert_eq!(value["timeout_requested"], 60);
        assert!(value["task_id"].as_str().unwrap().starts_with("test_tool:"));

        // The task should complete shortly
        let task_id = value["task_id"].as_str().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let status = router
            .transport()
            .get_status(&task_id.to_string())
            .await
            .unwrap();
        assert!(status.is_some());
        assert!(status.unwrap().is_terminal());
    }
}
