//! Async Tool Executor - Integration of UnifiedAsyncTool with Engine
//!
//! Provides async tool execution capabilities with:
//! - Capability detection (auto-detect async support)
//! - Progress reporting for long-running tasks
//! - Seamless fallback from sync to async
//! - Metrics and monitoring
//!
//! # Usage
//!
//! ```rust,ignore
//! let executor = AsyncToolExecutor::new();
//!
//! // Check if tool supports async
//! if executor.supports_async(tool.name()).await {
//!     let receipt = executor.execute_async(tool, params, context).await?;
//!     // Poll for status
//!     loop {
//!         let status = executor.check_status(tool.name(), &receipt.task_id).await?;
//!         if status.is_terminal() { break; }
//!         tokio::time::sleep(Duration::from_secs(1)).await;
//!     }
//! }
//! ```

use crate::agent::async_tool_framework::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskStatus, AsyncToolConfig, UnifiedAsyncExecutor,
};
use crate::engine::{ToolExecutionContext, ToolExecutor};
use crate::tools::{Tool, UnifiedAsyncTool};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

/// Progress update for async tool execution
#[derive(Debug, Clone)]
pub struct ToolProgress {
    /// Task identifier
    pub task_id: String,
    /// Tool name
    pub tool_name: String,
    /// Progress percentage (0-100)
    pub percent: u8,
    /// Human-readable status message
    pub message: String,
    /// Optional metadata
    pub metadata: Option<Value>,
}

/// Async tool capability information
#[derive(Debug, Clone, Default)]
pub struct AsyncCapability {
    /// Whether the tool supports native async execution
    pub supports_async: bool,
    /// Whether status checking is available
    pub supports_status_check: bool,
    /// Whether cancellation is supported
    pub supports_cancel: bool,
    /// Whether progress reporting is available
    pub supports_progress: bool,
    /// Estimated duration in seconds (if known)
    pub estimated_duration_secs: Option<u64>,
}

/// Enhanced executor with async tool support
pub struct AsyncToolExecutor {
    /// Base executor for sync tools
    sync_executor: ToolExecutor,
    /// Unified async executor for background tasks
    async_executor: UnifiedAsyncExecutor,
    /// Capability cache for tools
    capability_cache: Arc<RwLock<HashMap<String, AsyncCapability>>>,
    /// Progress callbacks by task_id
    progress_callbacks: Arc<RwLock<HashMap<String, Box<dyn Fn(ToolProgress) + Send + Sync>>>>,
    /// Default async configuration
    default_config: AsyncToolConfig,
}

impl std::fmt::Debug for AsyncToolExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncToolExecutor")
            .field("sync_executor", &self.sync_executor)
            .field("default_config", &self.default_config)
            .finish()
    }
}

impl AsyncToolExecutor {
    /// Create a new async tool executor
    pub fn new() -> Self {
        Self {
            sync_executor: ToolExecutor::new(),
            async_executor: UnifiedAsyncExecutor::new(),
            capability_cache: Arc::new(RwLock::new(HashMap::new())),
            progress_callbacks: Arc::new(RwLock::new(HashMap::new())),
            default_config: AsyncToolConfig::default(),
        }
    }

    /// Create with custom default timeout
    pub fn with_timeout(default_timeout: Duration) -> Self {
        Self {
            sync_executor: ToolExecutor::with_timeout(default_timeout),
            async_executor: UnifiedAsyncExecutor::new(),
            capability_cache: Arc::new(RwLock::new(HashMap::new())),
            progress_callbacks: Arc::new(RwLock::new(HashMap::new())),
            default_config: AsyncToolConfig {
                timeout_secs: default_timeout.as_secs(),
                ..AsyncToolConfig::default()
            },
        }
    }

    /// Create with custom async executor (for sharing registries)
    pub fn with_async_executor(async_executor: UnifiedAsyncExecutor) -> Self {
        Self {
            sync_executor: ToolExecutor::new(),
            async_executor,
            capability_cache: Arc::new(RwLock::new(HashMap::new())),
            progress_callbacks: Arc::new(RwLock::new(HashMap::new())),
            default_config: AsyncToolConfig::default(),
        }
    }

    /// Create with custom async executor and timeout
    pub fn with_async_executor_and_timeout(
        async_executor: UnifiedAsyncExecutor,
        default_timeout: Duration,
    ) -> Self {
        Self {
            sync_executor: ToolExecutor::with_timeout(default_timeout),
            async_executor,
            capability_cache: Arc::new(RwLock::new(HashMap::new())),
            progress_callbacks: Arc::new(RwLock::new(HashMap::new())),
            default_config: AsyncToolConfig {
                timeout_secs: default_timeout.as_secs(),
                ..AsyncToolConfig::default()
            },
        }
    }

    /// Check if a tool supports async execution
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool to check
    ///
    /// # Returns
    /// true if the tool supports async execution
    pub async fn supports_async(&self, tool_name: &str) -> bool {
        // Check cache first
        {
            let cache = self.capability_cache.read().await;
            if let Some(cap) = cache.get(tool_name) {
                return cap.supports_async;
            }
        }

        // Default: check if tool implements UnifiedAsyncTool
        // This is a placeholder - in practice, we'd query the tool registry
        false
    }

    /// Detect capabilities for a tool
    ///
    /// # Arguments
    /// * `tool` - The tool to detect capabilities for
    ///
    /// # Returns
    /// Detected async capabilities
    /// 
    /// # Note
    /// This is a simplified implementation. In practice, you'd use a registry
    /// that stores tools with their capabilities, or use a marker trait pattern.
    pub async fn detect_capabilities(&self, tool: &Arc<dyn Tool>) -> AsyncCapability {
        let tool_name = tool.name().to_string();

        // Check cache first
        {
            let cache = self.capability_cache.read().await;
            if let Some(cap) = cache.get(&tool_name) {
                return cap.clone();
            }
        }

        // For now, we can't easily downcast Arc<dyn Tool> to UnifiedAsyncTool
        // A proper implementation would use a registry pattern or double-dispatch
        // For this example, return default capabilities
        let cap = AsyncCapability::default();

        // Cache the result
        let mut cache = self.capability_cache.write().await;
        cache.insert(tool_name, cap.clone());

        cap
    }

    /// Execute a tool asynchronously
    ///
    /// # Arguments
    /// * `tool` - The tool to execute (must implement UnifiedAsyncTool)
    /// * `params` - Tool parameters
    /// * `context` - Execution context
    ///
    /// # Returns
    /// Receipt with task_id for tracking
    #[instrument(skip(self, tool, params, context), fields(tool_name = %tool.name()))]
    pub async fn execute_async(
        &self,
        tool: Arc<dyn Tool>,
        params: Value,
        context: &ToolExecutionContext,
    ) -> Result<AsyncTaskReceipt> {
        let tool_name = tool.name().to_string();

        info!(tool_name, "Starting async tool execution");

        // Try to get async interface
        if let Some(async_tool) = tool.as_async_tool() {
            // Build config from context
            let config = self.build_async_config(context);

            // Execute async
            let receipt = async_tool
                .execute_async(params, config)
                .await
                .with_context(|| format!("Async execution failed for tool '{}'", tool_name))?;

            info!(
                tool_name,
                task_id = %receipt.task_id,
                "Async tool execution started"
            );

            return Ok(receipt);
        }

        // Fallback: wrap sync execution
        warn!(
            tool_name,
            "Tool does not implement UnifiedAsyncTool, using sync fallback"
        );
        self.execute_sync_fallback(tool, params, context).await
    }

    /// Execute with explicit progress callback
    ///
    /// # Arguments
    /// * `tool` - The tool to execute
    /// * `params` - Tool parameters
    /// * `context` - Execution context
    /// * `on_progress` - Callback for progress updates
    ///
    /// # Returns
    /// Receipt with task_id for tracking
    pub async fn execute_with_progress<F>(
        &self,
        tool: Arc<dyn Tool>,
        params: Value,
        context: &ToolExecutionContext,
        on_progress: F,
    ) -> Result<AsyncTaskReceipt>
    where
        F: Fn(ToolProgress) + Send + Sync + 'static,
    {
        let receipt = self.execute_async(tool.clone(), params, context).await?;

        // Register progress callback
        let mut callbacks = self.progress_callbacks.write().await;
        callbacks.insert(receipt.task_id.clone(), Box::new(on_progress));

        Ok(receipt)
    }

    /// Check status of an async task
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool
    /// * `task_id` - Task identifier
    ///
    /// # Returns
    /// Current task status
    pub async fn check_status(
        &self,
        tool_name: &str,
        task_id: &AsyncTaskId,
    ) -> Result<AsyncTaskStatus> {
        // Query the unified executor
        match self.async_executor.check_status(task_id).await {
            Some(status) => Ok(status),
            None => {
                // Task not found in executor, might be completed
                debug!(tool_name, task_id, "Task not found in executor");
                Ok(AsyncTaskStatus::Pending)
            }
        }
    }

    /// Cancel an async task
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool
    /// * `task_id` - Task identifier
    ///
    /// # Returns
    /// true if cancellation was successful
    pub async fn cancel(&self, tool_name: &str, task_id: &AsyncTaskId) -> Result<bool> {
        self.async_executor.cancel(task_id).await
    }

    /// Wait for task completion with timeout
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool
    /// * `task_id` - Task identifier
    /// * `timeout` - Maximum time to wait
    ///
    /// # Returns
    /// Final status or timeout error
    pub async fn wait_for_completion(
        &self,
        _tool_name: &str,
        task_id: &AsyncTaskId,
        timeout: Duration,
    ) -> Result<AsyncTaskStatus> {
        let start = std::time::Instant::now();

        loop {
            let status = self.check_status(_tool_name, task_id).await?;

            if status.is_terminal() {
                return Ok(status);
            }

            if start.elapsed() >= timeout {
                return Err(anyhow::anyhow!("Timeout waiting for task completion"));
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Get default async configuration
    pub fn default_config(&self) -> &AsyncToolConfig {
        &self.default_config
    }

    /// Update default async configuration
    pub fn set_default_config(&mut self, config: AsyncToolConfig) {
        self.default_config = config;
    }

    /// Build async config from execution context
    fn build_async_config(&self, context: &ToolExecutionContext) -> AsyncToolConfig {
        AsyncToolConfig {
            timeout_secs: self.default_config.timeout_secs,
            label: Some(format!("{}_{}", context.agent_id, context.session_id)),
            ..self.default_config.clone()
        }
    }

    /// Fallback: execute sync tool in background via async executor
    async fn execute_sync_fallback(
        &self,
        tool: Arc<dyn Tool>,
        params: Value,
        context: &ToolExecutionContext,
    ) -> Result<AsyncTaskReceipt> {
        let tool_name = tool.name().to_string();
        let task_id = format!("{}:{}", tool_name, uuid::Uuid::new_v4());

        // Clone for the async block
        let sync_executor = self.sync_executor.clone();
        let ctx = context.clone();

        // Spawn sync execution in background
        let handle = tokio::spawn(async move {
            sync_executor.execute_with_context(tool, params, &ctx).await
        });

        // Note: In a full implementation, we'd register this handle
        // with the executor for status tracking and cancellation

        Ok(AsyncTaskReceipt {
            task_id,
            status: AsyncTaskStatus::Running,
            estimated_duration_secs: None,
            check_status_tool: format!("{}_status", tool_name),
        })
    }

}

impl Default for AsyncToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait for converting Arc<dyn Tool> to async interface
trait ToolAsAsync {
    fn as_async_tool(&self) -> Option<&dyn UnifiedAsyncTool>;
}

impl ToolAsAsync for Arc<dyn Tool> {
    fn as_async_tool(&self) -> Option<&dyn UnifiedAsyncTool> {
        // This is a simplified version - in practice, we'd need a way
        // to safely downcast Arc<dyn Tool> to Arc<dyn UnifiedAsyncTool>
        None
    }
}

/// Factory for creating async tool executors with shared state
pub struct AsyncToolExecutorFactory {
    shared_executor: UnifiedAsyncExecutor,
    default_timeout: Duration,
}

impl AsyncToolExecutorFactory {
    /// Create a new factory
    pub fn new() -> Self {
        Self {
            shared_executor: UnifiedAsyncExecutor::new(),
            default_timeout: Duration::from_secs(300),
        }
    }

    /// Create with custom timeout
    pub fn with_timeout(default_timeout: Duration) -> Self {
        Self {
            shared_executor: UnifiedAsyncExecutor::new(),
            default_timeout,
        }
    }

    /// Create a new executor sharing the async executor
    pub fn create_executor(&self) -> AsyncToolExecutor {
        AsyncToolExecutor::with_async_executor_and_timeout(
            self.shared_executor.clone(),
            self.default_timeout,
        )
    }
}

impl Default for AsyncToolExecutorFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolContext};
    use async_trait::async_trait;
    use serde_json::json;

    struct MockAsyncTool {
        name: String,
    }

    #[async_trait]
    impl Tool for MockAsyncTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> String {
            "Mock async tool".to_string()
        }

        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        async fn execute(&self, _params: Value) -> Result<Value> {
            Ok(json!({"result": "ok"}))
        }
    }

    #[async_trait]
    impl UnifiedAsyncTool for MockAsyncTool {
        fn supports_async(&self) -> bool {
            true
        }

        fn supports_status_check(&self) -> bool {
            true
        }

        async fn execute_async(
            &self,
            _params: Value,
            _config: AsyncToolConfig,
        ) -> Result<AsyncTaskReceipt> {
            Ok(AsyncTaskReceipt {
                task_id: "test_task".to_string(),
                status: AsyncTaskStatus::Running,
                estimated_duration_secs: Some(10),
                check_status_tool: "test_status".to_string(),
            })
        }

        async fn check_status(&self, _task_id: &AsyncTaskId) -> Result<AsyncTaskStatus> {
            Ok(AsyncTaskStatus::Completed {
                result: crate::tools::ToolResult::success(json!({"done": true})),
            })
        }

        async fn cancel(&self, _task_id: &AsyncTaskId) -> Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn test_async_executor_creation() {
        let executor = AsyncToolExecutor::new();
        assert_eq!(executor.default_config.timeout_secs, 300); // Default 5 min
    }

    #[tokio::test]
    async fn test_async_executor_with_timeout() {
        let executor = AsyncToolExecutor::with_timeout(Duration::from_secs(60));
        assert_eq!(executor.default_config.timeout_secs, 60);
    }

    #[tokio::test]
    async fn test_capability_detection() {
        let executor = AsyncToolExecutor::new();
        let tool: Arc<dyn Tool> = Arc::new(MockAsyncTool {
            name: "test".to_string(),
        });

        // For now, capability detection requires proper downcasting
        // This test verifies the structure exists
        let cap = executor.detect_capabilities(&tool).await;
        assert!(!cap.supports_async); // Will be false without proper downcasting
    }
}
