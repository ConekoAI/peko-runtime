//! Tool Wrapper - Reserved Parameters for Agent Control (DEPRECATED)
//!
//! ⚠️ DEPRECATED: This module is deprecated as of ADR-018a.
//!
//! Use the Extension Framework's AsyncExecutionRouter instead, which provides
//! unified reserved parameter handling, timeout management, and panic isolation
//! for all tool types (built-in, MCP, universal).
//!
//! # Migration Guide
//!
//! Instead of wrapping tools with ToolWrapper:
//! ```rust,ignore
//! // OLD - ToolWrapper approach
//! let tool = ToolWrapper::new(Arc::new(my_tool), WrapperConfig::default());
//! ```
//!
//! Use ExtensionCore hooks for tool execution:
//! ```rust,ignore
//! // NEW - ExtensionCore approach
//! let hook_point = HookPointBuilder::tool_execute("my_tool");
//! let result = extension_core.invoke_hook(hook_point, HookInput::ToolCall { ... }).await;
//! ```
//!
//! The Extension Framework automatically handles:
//! - Reserved parameter extraction (`_async`, `_timeout`, etc.)
//! - Timeout enforcement with panic isolation
//! - Async/sync mode routing
//!
//! # Deprecated Functionality
//!
//! This module provided a universal wrapper for all tools that adds reserved parameter support,
//! allowing agents to control execution mode without modifying tool schemas.
//!
//! # Reserved Parameters
//!
//! | Parameter | Type | Default | Description |
//! |-----------|------|---------|-------------|
//! | `_async` | bool | `false` | Execute asynchronously |
//! | `_timeout` | u64 | 120/300 | Timeout in seconds (sync/async) |
//! | `_callback` | string | "queue" | Result delivery mode |
//! | `_progress` | bool | `true` | Request progress updates |
//! | `_priority` | string | "normal" | Task priority |
//! | `_retry` | u32 | 0 | Number of retries |
//!
//! # Usage
//!
//! ```rust,ignore
//! let tool = ToolWrapper::new(
//!     Arc::new(my_tool),
//!     WrapperConfig::default(),
//! );
//!
//! // Agent calls with reserved params
//! let result = tool.execute(json!({
//!     "query": "*.rs",
//!     "_async": true,
//!     "_timeout": 300,
//! })).await?;
//! ```

use crate::agent::async_tool_framework::AsyncTaskStatus;
use crate::engine::async_tool_executor::AsyncToolExecutor;
use crate::engine::{ToolExecutionContext, ToolExecutor};
use crate::tools::Tool;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, instrument, warn};

/// Reserved parameters extracted from tool calls
/// 
/// # Deprecated
/// Use `AsyncReservedParams` from `crate::extensions::services` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[deprecated(
    since = "0.1.0",
    note = "Use AsyncReservedParams from crate::extensions::services instead"
)]
pub struct ReservedParams {
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

impl Default for ReservedParams {
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

impl ReservedParams {
    /// Extract reserved parameters from a JSON value
    pub fn extract(params: &mut Value) -> (Self, Vec<String>) {
        let mut reserved = Self::default();
        let conflicts = Vec::new();

        if let Some(obj) = params.as_object_mut() {
            // Extract _async
            if let Some(v) = obj.remove("_async") {
                reserved.async_mode = v.as_bool().unwrap_or(false);
            }

            // Extract _timeout
            if let Some(v) = obj.remove("_timeout") {
                reserved.timeout_secs = v.as_u64();
            }

            // Extract _callback
            if let Some(v) = obj.remove("_callback") {
                if let Some(s) = v.as_str() {
                    reserved.callback = s.to_string();
                }
            }

            // Extract _progress
            if let Some(v) = obj.remove("_progress") {
                reserved.progress = v.as_bool().unwrap_or(true);
            }

            // Extract _priority
            if let Some(v) = obj.remove("_priority") {
                if let Some(s) = v.as_str() {
                    reserved.priority = s.to_string();
                }
            }

            // Extract _retry
            if let Some(v) = obj.remove("_retry") {
                reserved.retry_count = v.as_u64().unwrap_or(0) as u32;
            }
        }

        (reserved, conflicts)
    }

    /// Get effective timeout (use reserved or default)
    pub fn effective_timeout(&self, is_async: bool) -> u64 {
        self.timeout_secs.unwrap_or(if is_async { 300 } else { 120 })
    }

    /// Validate callback mode
    pub fn is_valid_callback(&self) -> bool {
        matches!(self.callback.as_str(), "queue" | "stream" | "blocking")
    }

    /// Validate priority
    pub fn is_valid_priority(&self) -> bool {
        matches!(self.priority.as_str(), "low" | "normal" | "high")
    }
}

/// Configuration for tool wrapper
/// 
/// # Deprecated
/// ToolWrapper is deprecated. Configuration is now handled by AsyncExecutionRouter.
#[derive(Debug, Clone)]
#[deprecated(
    since = "0.1.0",
    note = "ToolWrapper is deprecated. Use ExtensionCore tool execution hooks instead"
)]
pub struct WrapperConfig {
    /// Default timeout for sync execution
    pub default_sync_timeout_secs: u64,
    /// Default timeout for async execution
    pub default_async_timeout_secs: u64,
    /// Whether async is allowed
    pub allow_async: bool,
    /// Async executor (required for async mode)
    pub async_executor: Option<Arc<AsyncToolExecutor>>,
    /// Sync executor (required for sync mode)
    pub sync_executor: Option<Arc<ToolExecutor>>,
    /// Log reserved param usage
    pub log_reserved_params: bool,
}

impl Default for WrapperConfig {
    fn default() -> Self {
        Self {
            default_sync_timeout_secs: 120,
            default_async_timeout_secs: 300,
            allow_async: true,
            async_executor: None,
            sync_executor: None,
            log_reserved_params: true,
        }
    }
}

impl WrapperConfig {
    /// Create with async executor
    pub fn with_async_executor(mut self, executor: Arc<AsyncToolExecutor>) -> Self {
        self.async_executor = Some(executor);
        self
    }

    /// Create with sync executor
    pub fn with_sync_executor(mut self, executor: Arc<ToolExecutor>) -> Self {
        self.sync_executor = Some(executor);
        self
    }

    /// Disable async
    pub fn sync_only(mut self) -> Self {
        self.allow_async = false;
        self
    }
}

/// Wrapper for any tool that handles reserved parameters
/// 
/// # Deprecated
/// Use ExtensionCore tool execution hooks with AsyncExecutionRouter instead.
/// All tool execution now routes through the Extension Framework for unified
/// handling of reserved parameters, timeouts, and panic isolation.
#[deprecated(
    since = "0.1.0",
    note = "Use ExtensionCore::invoke_hook with ToolExecute hook point instead"
)]
pub struct ToolWrapper {
    /// Inner tool implementation
    inner: Arc<dyn Tool>,
    /// Wrapper configuration
    config: WrapperConfig,
    /// Track reserved param usage for metrics
    usage_metrics: std::sync::Mutex<WrapperMetrics>,
}

/// Metrics for wrapper usage
#[derive(Debug, Default)]
struct WrapperMetrics {
    sync_calls: u64,
    async_calls: u64,
    reserved_param_usage: HashMap<String, u64>,
}

impl ToolWrapper {
    /// Create a new tool wrapper
    pub fn new(inner: Arc<dyn Tool>, config: WrapperConfig) -> Self {
        Self {
            inner,
            config,
            usage_metrics: std::sync::Mutex::new(WrapperMetrics::default()),
        }
    }

    /// Get the inner tool name
    pub fn tool_name(&self) -> &str {
        self.inner.name()
    }

    /// Check if the inner tool shadows reserved parameters
    fn check_param_conflicts(&self) -> Vec<String> {
        let schema = self.inner.parameters();
        let reserved_names = ["_async", "_timeout", "_callback", "_progress", "_priority", "_retry"];
        let mut conflicts = Vec::new();

        if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
            for name in &reserved_names {
                if props.contains_key(*name) {
                    conflicts.push(name.to_string());
                }
            }
        }

        conflicts
    }

    /// Update usage metrics
    fn record_usage(&self, reserved: &ReservedParams) {
        let Ok(mut metrics) = self.usage_metrics.lock() else {
            return;
        };

        // Track sync vs async calls
        if reserved.async_mode {
            metrics.async_calls += 1;
        } else {
            metrics.sync_calls += 1;
        }

        // Track which reserved params have non-default values
        let defaults = ReservedParams::default();
        let tracked_params = [
            ("_async", reserved.async_mode != defaults.async_mode),
            ("_timeout", reserved.timeout_secs != defaults.timeout_secs),
            ("_callback", reserved.callback != defaults.callback),
            ("_progress", reserved.progress != defaults.progress),
            ("_priority", reserved.priority != defaults.priority),
            ("_retry", reserved.retry_count != defaults.retry_count),
        ];

        for (name, used) in tracked_params {
            if used {
                *metrics.reserved_param_usage.entry(name.to_string()).or_insert(0) += 1;
            }
        }
    }

    /// Execute with retry logic
    async fn execute_with_retry<F, Fut>(
        &self,
        operation: F,
        retry_count: u32,
    ) -> Result<Value>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<Value>>,
    {
        let mut last_error = None;

        for attempt in 0..=retry_count {
            if attempt > 0 {
                debug!(
                    tool = %self.tool_name(),
                    attempt = attempt,
                    "Retrying tool execution"
                );
                tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
            }

            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    warn!(
                        tool = %self.tool_name(),
                        attempt = attempt,
                        error = %e,
                        "Tool execution failed"
                    );
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Unknown error")))
    }

    /// Execute synchronously
    #[instrument(skip(self, params), fields(tool_name = %self.tool_name()))]
    async fn execute_sync(
        &self,
        params: Value,
        reserved: &ReservedParams,
    ) -> Result<Value> {
        let timeout_secs = reserved.effective_timeout(false);

        info!(
            tool_name = %self.tool_name(),
            timeout = timeout_secs,
            "Executing tool synchronously"
        );

        let operation = || async {
            // Use sync executor if available, otherwise direct call
            if let Some(executor) = &self.config.sync_executor {
                // Create minimal context
                let ctx = ToolExecutionContext::new(
                    "unknown",
                    "unknown",
                    "unknown",
                );
                executor.execute_with_context(self.inner.clone(), params.clone(), &ctx).await
            } else {
                self.inner.execute(params.clone()).await
            }
        };

        // Apply timeout
        let timeout = Duration::from_secs(timeout_secs);
        match tokio::time::timeout(timeout, self.execute_with_retry(operation, reserved.retry_count)).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("Tool execution timed out after {}s", timeout_secs)),
        }
    }

    /// Execute asynchronously
    #[instrument(skip(self, params), fields(tool_name = %self.tool_name()))]
    async fn execute_async(
        &self,
        params: Value,
        reserved: &ReservedParams,
    ) -> Result<Value> {
        let executor = self.config.async_executor.as_ref()
            .context("Async executor not configured")?;

        let timeout_secs = reserved.effective_timeout(true);

        info!(
            tool_name = %self.tool_name(),
            timeout = timeout_secs,
            "Executing tool asynchronously"
        );

        // Create execution context
        let ctx = ToolExecutionContext::new(
            "unknown",
            "unknown",
            "unknown",
        );

        // Execute async
        let receipt = executor
            .execute_async(self.inner.clone(), params, &ctx)
            .await
            .context("Failed to start async execution")?;

        // For now, wait for completion (blocking mode)
        // TODO: Support non-blocking modes in future
        let timeout = Duration::from_secs(timeout_secs);
        let start = std::time::Instant::now();

        loop {
            let status = executor
                .check_status(self.tool_name(), &receipt.task_id)
                .await?;

            match status {
                AsyncTaskStatus::Completed { .. } => {
                    return Ok(serde_json::json!({
                        "status": "completed",
                        "task_id": receipt.task_id,
                    }));
                }
                AsyncTaskStatus::Failed { error } => {
                    return Err(anyhow::anyhow!("Async execution failed: {}", error));
                }
                AsyncTaskStatus::Cancelled => {
                    return Err(anyhow::anyhow!("Async execution was cancelled"));
                }
                _ => {
                    if start.elapsed() > timeout {
                        return Err(anyhow::anyhow!("Async execution timed out"));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for ToolWrapper {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> String {
        self.inner.description()
    }

    fn parameters(&self) -> Value {
        // Return ORIGINAL tool parameters (no reserved params)
        // This keeps tool schemas clean
        self.inner.parameters()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn execute(&self, params: Value) -> Result<Value> {
        // Check for conflicts on first execution
        let conflicts = self.check_param_conflicts();
        if !conflicts.is_empty() {
            warn!(
                tool = %self.tool_name(),
                conflicts = ?conflicts,
                "Tool shadows reserved parameters"
            );
        }

        // Extract reserved params (mutates params to remove them)
        let mut params = params;
        let (reserved, _extracted_conflicts) = ReservedParams::extract(&mut params);

        // Log reserved param usage if enabled
        if self.config.log_reserved_params && self.has_reserved_params(&reserved) {
            debug!(
                tool = %self.tool_name(),
                async_mode = reserved.async_mode,
                timeout = ?reserved.timeout_secs,
                "Reserved parameters detected"
            );
        }

        // Record metrics
        self.record_usage(&reserved);

        // Validate reserved params
        if !reserved.is_valid_callback() {
            return Err(anyhow::anyhow!(
                "Invalid callback mode: {}. Use 'queue', 'stream', or 'blocking'",
                reserved.callback
            ));
        }

        if !reserved.is_valid_priority() {
            return Err(anyhow::anyhow!(
                "Invalid priority: {}. Use 'low', 'normal', or 'high'",
                reserved.priority
            ));
        }

        // Route to appropriate execution mode
        if reserved.async_mode {
            if !self.config.allow_async {
                warn!(
                    tool = %self.tool_name(),
                    "Async requested but not allowed, falling back to sync"
                );
                self.execute_sync(params, &reserved).await
            } else {
                self.execute_async(params, &reserved).await
            }
        } else {
            self.execute_sync(params, &reserved).await
        }
    }
}

impl ToolWrapper {
    /// Check if any reserved params were specified (non-default values)
    fn has_reserved_params(&self, reserved: &ReservedParams) -> bool {
        // Compare against defaults - any difference means reserved params were used
        let defaults = ReservedParams::default();
        reserved.async_mode != defaults.async_mode
            || reserved.timeout_secs != defaults.timeout_secs
            || reserved.callback != defaults.callback
            || reserved.progress != defaults.progress
            || reserved.priority != defaults.priority
            || reserved.retry_count != defaults.retry_count
    }
}

/// Factory for creating wrapped tools
/// 
/// # Deprecated
/// Use ExtensionCore tool execution hooks instead of wrapping tools.
#[deprecated(
    since = "0.1.0",
    note = "Use ExtensionCore tool execution hooks instead"
)]
pub struct ToolWrapperFactory {
    config: WrapperConfig,
}

impl ToolWrapperFactory {
    /// Create a new factory with default config
    pub fn new() -> Self {
        Self {
            config: WrapperConfig::default(),
        }
    }

    /// Create with custom config
    pub fn with_config(config: WrapperConfig) -> Self {
        Self { config }
    }

    /// Wrap a tool
    pub fn wrap(&self, tool: Arc<dyn Tool>) -> ToolWrapper {
        ToolWrapper::new(tool, self.config.clone())
    }

    /// Wrap multiple tools
    pub fn wrap_many(&self, tools: Vec<Arc<dyn Tool>>) -> Vec<ToolWrapper> {
        tools.into_iter().map(|t| self.wrap(t)).collect()
    }
}

impl Default for ToolWrapperFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// System prompt section for reserved parameters
/// 
/// # Deprecated
/// This prompt section is no longer needed with the Extension Framework.
/// Reserved parameters are now documented in the system prompt by the
/// Extension Framework's ToolRegister hook handlers.
#[deprecated(
    since = "0.1.0",
    note = "Reserved parameters are now documented by the Extension Framework"
)]
pub fn get_reserved_params_prompt_section() -> String {
    r#"## Execution Control Parameters (All Tools)

These optional reserved parameters control how tools are executed. They are automatically handled by the runtime and removed before the tool receives its parameters.

**Note:** Parameter names start with underscore `_` to avoid conflicts with tool-specific parameters.

### Available Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `_async` | boolean | `false` | Execute asynchronously. Returns immediately with a task ID for status checking. |
| `_timeout` | integer | 120 (sync)<br>300 (async) | Maximum execution time in seconds. |
| `_callback` | string | `"queue"` | Result delivery: `"queue"` (default), `"stream"`, or `"blocking"`. |
| `_progress` | boolean | `true` | Request progress updates during async execution. |
| `_priority` | string | `"normal"` | Task scheduling priority: `"low"`, `"normal"`, or `"high"`. |
| `_retry` | integer | `0` | Number of automatic retries on failure. |

### Usage Examples

**Async execution with custom timeout:**
```json
{
  "name": "search_files",
  "arguments": {
    "query": "*.rs",
    "_async": true,
    "_timeout": 300
  }
}
```

**Sync execution with retry:**
```json
{
  "name": "api_call",
  "arguments": {
    "endpoint": "/data",
    "_retry": 3,
    "_timeout": 60
  }
}
```

**High priority async with streaming:**
```json
{
  "name": "process_large_file",
  "arguments": {
    "file": "data.csv",
    "_async": true,
    "_priority": "high",
    "_callback": "stream",
    "_progress": true
  }
}
```

### Important Notes

1. **Tool schemas don't include these parameters** - they are handled transparently by the runtime.

2. **If a tool defines a parameter with the same name**, the tool's parameter takes precedence and a warning is logged.

3. **Not all tools support async** - if `_async: true` is specified but the tool doesn't support it, the call falls back to sync execution.

4. **Timeouts are best-effort** - actual timeout may vary slightly due to scheduling.

"#.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use async_trait::async_trait;

    struct MockTool {
        name: String,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> String {
            "A mock tool for testing".to_string()
        }

        fn parameters(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                }
            })
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        async fn execute(&self, params: Value) -> Result<Value> {
            Ok(serde_json::json!({
                "received": params,
                "tool": self.name
            }))
        }
    }

    #[test]
    fn test_reserved_params_default() {
        let params = ReservedParams::default();
        assert!(!params.async_mode);
        assert_eq!(params.timeout_secs, None);
        assert_eq!(params.callback, "queue");
        assert!(params.progress);
        assert_eq!(params.priority, "normal");
        assert_eq!(params.retry_count, 0);
    }

    #[test]
    fn test_reserved_params_extract() {
        let mut params = serde_json::json!({
            "input": "test",
            "_async": true,
            "_timeout": 300,
            "_priority": "high",
        });

        let (reserved, _) = ReservedParams::extract(&mut params);

        assert!(reserved.async_mode);
        assert_eq!(reserved.timeout_secs, Some(300));
        assert_eq!(reserved.priority, "high");

        // Original param should remain
        assert_eq!(params["input"], "test");

        // Reserved params should be removed
        assert!(params.get("_async").is_none());
        assert!(params.get("_timeout").is_none());
    }

    #[test]
    fn test_reserved_params_effective_timeout() {
        let mut params = ReservedParams::default();

        // Default for sync
        assert_eq!(params.effective_timeout(false), 120);

        // Default for async
        assert_eq!(params.effective_timeout(true), 300);

        // Custom timeout
        params.timeout_secs = Some(600);
        assert_eq!(params.effective_timeout(false), 600);
        assert_eq!(params.effective_timeout(true), 600);
    }

    #[test]
    fn test_wrapper_creation() {
        let tool = Arc::new(MockTool {
            name: "test_tool".to_string(),
        });
        let wrapper = ToolWrapper::new(tool, WrapperConfig::default());

        assert_eq!(wrapper.name(), "test_tool");
    }

    #[test]
    fn test_wrapper_parameters_dont_include_reserved() {
        let tool = Arc::new(MockTool {
            name: "test_tool".to_string(),
        });
        let wrapper = ToolWrapper::new(tool, WrapperConfig::default());

        let params = wrapper.parameters();
        let props = params.get("properties").unwrap().as_object().unwrap();

        // Should only have the tool's original params
        assert!(props.contains_key("input"));

        // Should NOT have reserved params
        assert!(!props.contains_key("_async"));
        assert!(!props.contains_key("_timeout"));
    }

    #[test]
    fn test_check_param_conflicts() {
        struct ConflictingTool;

        #[async_trait]
        impl Tool for ConflictingTool {
            fn name(&self) -> &str {
                "conflicting"
            }

            fn description(&self) -> String {
                "Has _async param".to_string()
            }

            fn parameters(&self) -> Value {
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "_async": { "type": "boolean" },
                        "normal_param": { "type": "string" }
                    }
                })
            }

            async fn execute(&self, _params: Value) -> Result<Value> {
                Ok(Value::Null)
            }
        }

        let tool = Arc::new(ConflictingTool);
        let wrapper = ToolWrapper::new(tool, WrapperConfig::default());

        let conflicts = wrapper.check_param_conflicts();
        assert!(conflicts.contains(&"_async".to_string()));
        assert!(!conflicts.contains(&"normal_param".to_string()));
    }

    #[test]
    fn test_get_reserved_params_prompt_section() {
        let section = get_reserved_params_prompt_section();
        assert!(section.contains("_async"));
        assert!(section.contains("_timeout"));
        assert!(section.contains("Execution Control Parameters"));
    }

    #[tokio::test]
    async fn test_wrapper_execute_sync() {
        let tool = Arc::new(MockTool {
            name: "test_tool".to_string(),
        });
        let wrapper = ToolWrapper::new(tool, WrapperConfig::default().sync_only());

        let result = wrapper
            .execute(serde_json::json!({
                "input": "hello",
                "_async": false,
            }))
            .await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result["tool"], "test_tool");
        assert_eq!(result["received"]["input"], "hello");

        // Reserved params should be stripped
        assert!(result["received"].get("_async").is_none());
    }

    #[test]
    fn test_wrapper_factory() {
        let factory = ToolWrapperFactory::new();
        let tool = Arc::new(MockTool {
            name: "tool1".to_string(),
        });

        let wrapper = factory.wrap(tool);
        assert_eq!(wrapper.name(), "tool1");
    }
}
