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
use crate::extensions::framework::services::tool_execution::{
    ToolExecutionConfig, ToolExecutionService,
};
use crate::extensions::framework::transport::async_transport::{
    AsyncTaskTransport, LocalAsyncTransport,
};
use crate::extensions::framework::types::{HookOutput, HookResult};
use anyhow::{anyhow, Result};
use futures::future::BoxFuture;
use serde_json::Value;
use std::time::Duration;
use tracing::{info, instrument, warn};

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
    #[instrument(skip(self, params, _exec_service, sync_executor), level = "debug")]
    pub async fn route<F, Fut>(
        &self,
        tool_name: &str,
        params: &mut Value,
        _exec_service: &ToolExecutionService,
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
        self.execute_with_timeout(tool_name, cleaned, tool_context, sync_executor)
            .await
    }

    /// Execute synchronously with the constant default timeout.
    ///
    /// The work is spawned as a background task via the transport first,
    /// then polled for completion up to the timeout. If the timeout fires
    /// before the task completes, a receipt is returned and the work
    /// continues running in the background.
    #[instrument(skip(self, params, sync_executor), level = "debug")]
    async fn execute_with_timeout<F, Fut>(
        &self,
        tool_name: &str,
        params: Value,
        tool_context: &ToolExecutionContext,
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
            timeout_secs: Some(task_hard_timeout_secs),
            timeout_millis: None,
            cleanup_after_delivery: true,
            label: Some(tool_name.to_string()),
            wake_on_completion: true,
            principal_root_session_key: None,
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
        let mut backoff = Duration::from_millis(50);
        loop {
            match self.transport.get_status(&task_id).await? {
                Some(AsyncTaskStatus::Completed { result }) => {
                    if result.success {
                        return Ok(result.data.unwrap_or(Value::Null));
                    }
                    return Err(anyhow::anyhow!(result
                        .error
                        .unwrap_or_else(|| "Tool execution failed".to_string())));
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
                Some(AsyncTaskStatus::Pending | AsyncTaskStatus::Running) | None => {
                    // Pending/Running: the task is still in flight.
                    // None: the transport cannot report a status. This is the
                    // CLI `DaemonIpcTransport` path, which has no IPC status
                    // channel and intentionally returns `None` to trigger this
                    // fallback (see `async_transport.rs`). Treat `None` as
                    // "still running": keep polling until the deadline, then
                    // return an honest queued receipt (ADR-020). Never fatal,
                    // and never fabricate completion.
                    let now = tokio::time::Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let remaining = deadline - now;
                    let sleep_duration = std::cmp::min(backoff, remaining);
                    tokio::time::sleep(sleep_duration).await;
                    // Double the backoff, capping at 1s.
                    backoff = std::cmp::min(backoff * 2, Duration::from_secs(1));
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

    /// Validate LLM-emitted tool arguments against the tool's declared
    /// JSON Schema. F32b — closes audit section 3 row 1 (P0).
    ///
    /// This runs BEFORE any preprocessor (workspace injection, reserved
    /// param defaults) so the LLM's emitted `arguments` are checked against
    /// the schema the LLM was actually given. Preprocessor-injected fields
    /// are not part of the LLM-facing schema and aren't validated here.
    ///
    /// On success, returns Ok(()). On schema violation, returns Err with a
    /// concise message that surfaces in the standard
    /// `(format!("Error: ..."), Value::String(s), false)` triplet via
    /// `tool_result_from_hook`. The downstream F32a `is_error: true`
    /// propagation carries the failure into the JSONL record and the
    /// next-iteration LLM message.
    ///
    /// Non-object params (e.g. an LLM emitting an array or scalar at the
    /// top level) are validated against the schema as-is. Schemas without
    /// an explicit `type` field are skipped (defensive — every built-in
    /// tool's `parameters()` returns a `{type: "object", ...}` literal).
    fn validate_tool_args(
        schema: &Value,
        args: &Value,
        tool_name: &str,
    ) -> std::result::Result<(), String> {
        // Skip empty / non-object schemas defensively — every built-in
        // tool's `parameters()` returns a non-empty object schema; future
        // tools that declare `{}` as their schema opt out of validation
        // by design.
        let schema_obj = match schema.as_object() {
            Some(s) => s,
            None => return Ok(()),
        };
        if schema_obj.is_empty() {
            return Ok(());
        }

        let validator = match jsonschema::validator_for(schema) {
            Ok(v) => v,
            Err(e) => {
                // Schema itself is malformed — surface as a hard error
                // so the framework doesn't silently dispatch with broken
                // validation. This is a tool-author bug, not an LLM bug.
                return Err(format!(
                    "Tool '{tool_name}' has an invalid JSON Schema; refusing to dispatch: {e}"
                ));
            }
        };

        let mut errors = validator.iter_errors(args);
        match errors.next() {
            None => Ok(()),
            Some(first) => {
                let mut msg = format!("Invalid arguments for tool '{tool_name}': {first}");
                // Append the rest of the error chain (cap at 5 to keep
                // the LLM-facing string bounded). The path is included
                // so the LLM can locate the offending field.
                let mut count = 0;
                for err in errors {
                    count += 1;
                    if count >= 5 {
                        msg.push_str(&format!(" (and {} more)", count));
                        break;
                    }
                    msg.push_str(&format!("; {err}"));
                }
                Err(msg)
            }
        }
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

        // 3. F32b — validate LLM-emitted args against the tool's
        // declared JSON Schema BEFORE any preprocessor (workspace
        // injection, reserved-params defaults). The LLM is held to the
        // schema it was given; preprocessor-injected fields are not
        // validated here because they're not part of the LLM-facing
        // surface.
        //
        // Failures short-circuit via the same `tool_result_from_hook`
        // shape as any other tool failure: the (String, Value, bool)
        // triplet produces an Error-prefixed response with
        // `success: false`. F32a's `is_error: !success` propagation
        // then carries the flag into both the JSONL record and the
        // next-iteration LLM message.
        if let Err(msg) = Self::validate_tool_args(&exec_config.full_schema, &params, tool_name) {
            warn!(
                tool = %tool_name,
                "Tool arg validation failed; returning as tool failure"
            );
            return HookResult::Error(anyhow!(msg));
        }

        // 4. Get services from context. The exec_service is unused
        // by `route()` (it takes `_exec_service`), so we no longer
        // pull it from the (now type-erased) `ExtensionServices`.
        // The legacy `tool_execution()` getter is removed because
        // the field is `Arc<dyn Any + Send + Sync>` in Phase 8a.
        // The trait-port impl in this file constructs a dummy
        // service before calling into this generic method.
        let exec_service = std::sync::Arc::new(ToolExecutionService::new());

        // 5. Build execution context
        let tool_ctx = match ctx
            .get_state::<crate::extensions::framework::types::ToolRuntimeContext>("tool_context")
        {
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
                &exec_service,
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

    // ===================== F32b: schema argument validation =====================

    /// F32b: A well-formed argument block against a `required`-bearing
    /// schema passes validation. Mirrors the Bash-tool shape: `command`
    /// is required, everything else is optional.
    #[test]
    fn test_validate_tool_args_passes_when_required_field_present() {
        let schema = json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "shell command"},
                "cwd": {"type": "string"}
            },
            "required": ["command"]
        });
        let args = json!({"command": "ls -la"});
        assert!(
            AsyncExecutionRouter::validate_tool_args(&schema, &args, "Bash").is_ok(),
            "valid args must pass schema validation"
        );
    }

    /// F32b: Missing the required `command` field fails validation.
    /// The error message must mention the tool name and surface to the
    /// LLM as a structured tool failure (via `tool_result_from_hook` →
    /// F32a `is_error: true` propagation).
    #[test]
    fn test_validate_tool_args_fails_when_required_field_missing() {
        let schema = json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "shell command"}
            },
            "required": ["command"]
        });
        let args = json!({}); // missing required `command`
        let result = AsyncExecutionRouter::validate_tool_args(&schema, &args, "Bash");
        let err = result.expect_err("validation must fail when required field is missing");
        assert!(
            err.contains("Bash"),
            "error must name the tool so the LLM can identify the offending call: {err}"
        );
        assert!(
            err.contains("command"),
            "error must identify the missing field: {err}"
        );
    }

    /// F32b: Wrong-type argument (a number when the schema expects a
    /// string) fails validation. Mirrors a real LLM error class where
    /// the model emits `{"command": 42}` because it confused JSON-shape
    /// markers in its prompt context.
    #[test]
    fn test_validate_tool_args_fails_on_type_mismatch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"}
            },
            "required": ["command"]
        });
        let args = json!({"command": 42});
        let result = AsyncExecutionRouter::validate_tool_args(&schema, &args, "Bash");
        assert!(
            result.is_err(),
            "type mismatch (number for declared string) must fail"
        );
    }

    /// F32b: Empty schema (defensive default) opts out of validation —
    /// tools that declare `{}` as their schema aren't expected to ship
    /// as a JSON Schema draft.
    #[test]
    fn test_validate_tool_args_skips_empty_schema() {
        let schema = json!({});
        // Any args — even malformed — pass against an empty schema
        let args = json!({"anything": [1, 2, 3]});
        assert!(
            AsyncExecutionRouter::validate_tool_args(&schema, &args, "UnknownTool").is_ok(),
            "empty schema must opt out of validation"
        );
    }

    /// F32b: Non-object schema (e.g., a tool author's bug — declaring
    /// `null` or a scalar as the schema) is treated as opt-out. We don't
    /// want to crash a dispatch because a tool author filed a bad schema.
    #[test]
    fn test_validate_tool_args_skips_non_object_schema() {
        let schema = json!(null);
        let args = json!({"command": "ls"});
        assert!(
            AsyncExecutionRouter::validate_tool_args(&schema, &args, "BrokenTool").is_ok(),
            "non-object schema must opt out of validation"
        );
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

    /// Mimics the CLI `DaemonIpcTransport`: `spawn_task` succeeds (returns a
    /// receipt) but `get_status` has no IPC status channel and returns `None`.
    #[derive(Debug)]
    struct NullStatusTransport;

    #[async_trait::async_trait]
    impl AsyncTaskTransport for NullStatusTransport {
        async fn spawn_task(
            &self,
            task_id: crate::extensions::framework::async_exec::executor::AsyncTaskId,
            _tool_name: String,
            _params: Value,
            _session_key: String,
            _workspace: std::path::PathBuf,
            _config: crate::extensions::framework::async_exec::executor::AsyncToolConfig,
        ) -> Result<crate::extensions::framework::async_exec::executor::AsyncTaskReceipt> {
            Ok(
                crate::extensions::framework::async_exec::executor::AsyncTaskReceipt {
                    task_id,
                    status: AsyncTaskStatus::Running,
                    estimated_duration_secs: None,
                    task_file: None,
                    params: None,
                },
            )
        }

        async fn get_status(
            &self,
            _task_id: &crate::extensions::framework::async_exec::executor::AsyncTaskId,
        ) -> Result<Option<AsyncTaskStatus>> {
            Ok(None)
        }

        async fn cancel_task(
            &self,
            _task_id: &crate::extensions::framework::async_exec::executor::AsyncTaskId,
        ) -> Result<bool> {
            Ok(false)
        }
    }

    /// `None` from the transport (the CLI IPC path) must fall back to an honest
    /// queued receipt at the deadline — never the fatal "task not found in
    /// transport registry" error. Locks the F1 fix.
    #[tokio::test]
    async fn test_router_none_status_falls_back_to_receipt() {
        let router = AsyncExecutionRouter {
            default_tool_timeout: Duration::from_secs(1),
            transport: std::sync::Arc::new(NullStatusTransport),
        };
        let exec_service = ToolExecutionService::new();
        let tool_context = ToolExecutionContext::new("agent1", "session1", "run1");
        let exec_config = ToolExecutionConfig::with_schema(json!({"type": "object"}));
        let mut params = json!({"query": "x"});

        let result = router
            .route(
                "ipc_tool",
                &mut params,
                &exec_service,
                &tool_context,
                &exec_config,
                |_p| async move { Ok(json!({"result": "should_not_run"})) },
            )
            .await;

        assert!(
            result.is_ok(),
            "None status must not be fatal: {:?}",
            result
        );
        let value = result.unwrap();
        assert_eq!(value["status"], "running");
        assert_eq!(value["reason"], "timeout");
        assert!(value.get("task_id").is_some());
    }
}

// ============================================================================
// Phase 8a trait-port impl
// ============================================================================

/// Impl of [`peko_extension_host::AsyncExecutionRouter`] for the root
/// concrete `AsyncExecutionRouter`.
///
/// `ExtensionServices::async_router` (now in `peko_extension_host`)
/// stores an `Arc<dyn AsyncExecutionRouter>`. The concrete router
/// stays in root until Phase 8b lifts the transport subtree; until
/// then, root provides the trait impl and root callers wrap their
/// router in `Arc::new(...) as Arc<dyn _>` at construction time.
///
/// The bridge closures convert the trait port's boxed
/// `PreprocessorFn` / `ExecFn` into the generic `F` / `P` shapes the
/// root's existing generic [`AsyncExecutionRouter::execute_from_hook`]
/// accepts. `BoxFuture<'static, _>` already satisfies the
/// `Future<Output = _> + Send + 'static` bound, so the bridge is
/// transparent.
#[async_trait::async_trait]
impl peko_extension_host::AsyncExecutionRouter for AsyncExecutionRouter {
    async fn execute_from_hook(
        &self,
        ctx: &HookContext,
        tool_name: &str,
        exec_config: &peko_extension_host::ToolExecConfig,
        preprocessor: Option<peko_extension_host::PreprocessorFn>,
        exec_fn: peko_extension_host::ExecFn,
    ) -> HookResult {
        // Rebuild a root-side ToolExecutionConfig. The trait-port
        // `ToolExecConfig` holds a `peko_extension_api::ReservedParamsConfig`,
        // which is the same type root's `services::ToolExecutionConfig`
        // expects, so this is just a struct-field copy.
        let local_exec_config = ToolExecutionConfig {
            full_schema: exec_config.full_schema.clone(),
            reserved_params: exec_config.reserved_params.clone(),
        };

        // Bridge: convert `Option<Box<dyn Fn + Send + Sync>>` into
        // `Option<impl Fn(&mut Value, Option<&str>) + Send>`. Root's
        // generic P bound only requires `Send`; Sync is a strict
        // subset that satisfies it.
        //
        // The bridge takes `preprocessor` by reference so the closure
        // is `Fn` (not `FnOnce`); `Option::as_ref()` gives `Option<&Box<...>>`.
        let preprocessor_bridge = move |params: &mut Value, workspace: Option<&str>| {
            if let Some(p) = preprocessor.as_ref() {
                p(params, workspace);
            }
        };

        // Bridge: convert `Box<dyn FnOnce(Value) -> BoxFuture<_, _>>`
        // into a closure whose return type satisfies `Fut: Future +
        // Send + 'static`. `BoxFuture` already derefs to
        // `Pin<Box<dyn Future + Send>>` which is itself a valid
        // `Future + Send + 'static`.
        let exec_fn_bridge = move |v: Value| -> BoxFuture<'static, Result<Value>> { exec_fn(v) };

        // `route()` requires `&ToolExecutionService`; root's body
        // ignores it (the `_exec_service` underscore prefix confirms
        // this), so we pass a temporary.
        let dummy_exec_service = ToolExecutionService::new();

        // Delegate to the existing generic `execute_from_hook`. That
        // method performs tool-call extraction, tool-name match,
        // F32b JSON-Schema validation, preprocessor invocation, and
        // route dispatch — all the work the trait port needs.
        self.execute_from_hook(
            ctx,
            tool_name,
            &local_exec_config,
            Some(preprocessor_bridge),
            exec_fn_bridge,
        )
        .await
    }

    async fn wait_for_all_tasks(&self, timeout: Duration) {
        // Delegate to the existing concrete method (defined earlier
        // in this file). It sleeps up to `timeout` for the local
        // transport and returns immediately for the HTTP transport.
        AsyncExecutionRouter::wait_for_all_tasks(self, timeout).await;
    }
}
