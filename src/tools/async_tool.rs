//! Unified Async Tool trait for seamless async tool integration
//!
//! This trait extends the base Tool trait with async-specific capabilities,
//! allowing tools to implement native async execution, status checking,
//! and cancellation.
//!
//! # Architecture
//!
//! The async tool system has three layers:
//! 1. **`AsyncTool` trait** (this file) - Native async-capable tools implement this
//! 2. **`ExtensionAsyncAdapter`** - Bridges `ExtensionCore` hooks with `AsyncExecutor`
//! 3. **`AsyncExecutor`** - Manages task lifecycle and execution
//!
//! # Implementation Guide
//!
//! For native async support, implement `AsyncTool`:
//! ```rust,ignore
//! use async_trait::async_trait;
//! use pekobot::tools::async_tool::AsyncTool;
//! use pekobot::agent::async_tool_framework::{AsyncTaskReceipt, AsyncTaskId, AsyncTaskStatus, AsyncToolConfig};
//! use serde_json::Value;
//! use anyhow::Result;
//!
//! #[async_trait]
//! impl AsyncTool for MyTool {
//!     fn supports_async(&self) -> bool { true }
//!
//!     async fn execute_async(&self, params: Value, config: AsyncToolConfig)
//!         -> Result<AsyncTaskReceipt> { todo!() }
//!
//!     async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus> { todo!() }
//!
//!     async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool> { todo!() }
//! }
//! ```
//!
//! For sync-only tools, implement only the base `Tool` trait and use the
//! `SyncToAsyncAdapter` wrapper for async compatibility.

use crate::agent::async_tool_framework::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig,
};
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// Trait for tools that support native async execution
///
/// This trait extends the base Tool trait with async-specific capabilities.
/// Tools can implement this to provide native async support, or use the
/// `SyncToAsyncAdapter` for automatic sync-to-async wrapping.
#[async_trait]
pub trait AsyncTool: Tool {
    /// Check if this tool supports async execution
    ///
    /// Returns true if the tool has implemented native async support.
    /// Default implementation returns false for backward compatibility.
    fn supports_async(&self) -> bool {
        false
    }

    /// Check if this tool supports status checking
    ///
    /// Returns true if `check_status` is implemented and functional.
    fn supports_status_check(&self) -> bool {
        false
    }

    /// Check if this tool supports cancellation
    ///
    /// Returns true if cancel is implemented and functional.
    fn supports_cancel(&self) -> bool {
        false
    }

    /// Execute the tool asynchronously
    ///
    /// # Arguments
    /// * `params` - Tool parameters from the LLM
    /// * `config` - Async execution configuration (timeout, callbacks, etc.)
    ///
    /// # Returns
    /// Receipt containing `task_id` for tracking the async operation
    async fn execute_async(
        &self,
        params: Value,
        config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt>;

    /// Check the status of an async task
    ///
    /// # Arguments
    /// * `task_id` - The task identifier returned by `execute_async`
    ///
    /// # Returns
    /// Current status of the task (Pending, Running, Completed, Failed, Cancelled)
    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus>;

    /// Cancel an async task
    ///
    /// # Arguments
    /// * `task_id` - The task identifier returned by `execute_async`
    ///
    /// # Returns
    /// true if cancellation was successful, false otherwise
    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool>;

    /// Get the name of the tool to use for status checks
    ///
    /// This is used when the async execution spawns a different tool
    /// for checking status. Default returns `self.name()`.
    fn status_check_tool_name(&self) -> String {
        self.name().to_string()
    }

    /// Estimate async execution duration
    ///
    /// Returns an estimated duration in seconds for async execution.
    /// Used for scheduling and timeout configuration.
    fn estimated_async_duration_secs(&self, _params: &Value) -> Option<u64> {
        None
    }
}

/// Adapter that wraps a synchronous Tool to provide async compatibility
///
/// This adapter uses the `AsyncExecutor` to run sync tools in the background,
/// providing async semantics without requiring the tool to implement native async support.
pub struct SyncToAsyncAdapter<T: Tool> {
    inner: Arc<T>,
    executor: crate::agent::async_tool_framework::AsyncExecutor,
}

impl<T: Tool> SyncToAsyncAdapter<T> {
    /// Create a new adapter wrapping a sync tool
    pub fn new(inner: Arc<T>) -> Self {
        Self {
            inner,
            executor: crate::agent::async_tool_framework::AsyncExecutor::new(),
        }
    }

    /// Create with a custom executor
    pub fn with_executor(
        inner: Arc<T>,
        executor: crate::agent::async_tool_framework::AsyncExecutor,
    ) -> Self {
        Self { inner, executor }
    }

    /// Get a reference to the inner tool
    #[must_use]
    pub fn inner(&self) -> &T {
        &self.inner
    }
}

impl<T: Tool> std::fmt::Debug for SyncToAsyncAdapter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncToAsyncAdapter")
            .field("tool_name", &self.inner.name())
            .finish()
    }
}

#[async_trait]
impl<T: Tool + Send + Sync + 'static> Tool for SyncToAsyncAdapter<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> String {
        self.inner.description()
    }

    fn parameters(&self) -> Value {
        self.inner.parameters()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn execute(&self, params: Value) -> Result<Value> {
        self.inner.execute(params).await
    }

    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &crate::tools::context::ToolContext,
    ) -> Result<Value> {
        self.inner.execute_with_context(params, ctx).await
    }

    fn supports_progress(&self) -> bool {
        self.inner.supports_progress()
    }

    fn estimated_duration_ms(&self, params: &Value) -> u64 {
        self.inner.estimated_duration_ms(params)
    }
}

#[async_trait]
impl<T: Tool + Send + Sync + 'static> AsyncTool for SyncToAsyncAdapter<T> {
    fn supports_async(&self) -> bool {
        true // This adapter enables async for any sync tool
    }

    fn supports_status_check(&self) -> bool {
        true
    }

    fn supports_cancel(&self) -> bool {
        false // Cancel not supported for wrapped sync tools
    }

    async fn execute_async(
        &self,
        params: Value,
        config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        let tool_name = self.inner.name().to_string();
        let task_id = format!("{}:{}", tool_name, uuid::Uuid::new_v4());
        let inner = self.inner.clone();
        let params_clone = params.clone();

        // Execute the sync tool in the background
        let receipt = self
            .executor
            .execute(
                task_id.clone(),
                tool_name.clone(),
                params,
                "default_session", // Session key - should be configurable
                config,
                move || async move {
                    match inner.execute(params_clone).await {
                        Ok(result) => Ok(AsyncTaskResult::Generic { data: result }),
                        Err(e) => Ok(AsyncTaskResult::Generic {
                            data: serde_json::json!({"error": e.to_string()}),
                        }),
                    }
                },
            )
            .await?;

        Ok(receipt)
    }

    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus> {
        match self.executor.check_status(task_id).await {
            Some(status) => Ok(status),
            None => Ok(AsyncTaskStatus::Pending),
        }
    }

    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool> {
        self.executor.cancel(task_id).await
    }

    fn status_check_tool_name(&self) -> String {
        format!("{}_status", self.inner.name())
    }

    fn estimated_async_duration_secs(&self, params: &Value) -> Option<u64> {
        Some(self.inner.estimated_duration_ms(params) / 1000)
    }
}

/// Trait object type for async-capable tools
pub type BoxedAsyncTool = Box<dyn AsyncTool + Send + Sync>;

/// Convert a boxed Tool to a boxed `AsyncTool`
///
/// If the tool already implements `AsyncTool`, returns it directly.
/// Otherwise, wraps it in a `SyncToAsyncAdapter`.
pub fn into_async_tool<T>(tool: T) -> BoxedAsyncTool
where
    T: Tool + Send + Sync + 'static,
{
    // For now, we always wrap in SyncToAsyncAdapter
    // In the future, we could check if the tool already implements AsyncTool
    Box::new(SyncToAsyncAdapter::new(Arc::new(tool)))
}

/// Extension trait for Tool to add async capabilities
pub trait ToolAsyncExt: Tool {
    /// Wrap this tool in an async adapter
    fn into_async(self) -> SyncToAsyncAdapter<Self>
    where
        Self: Sized,
    {
        SyncToAsyncAdapter::new(Arc::new(self))
    }

    /// Wrap this tool in an async adapter with custom executor
    fn into_async_with_executor(
        self,
        executor: crate::agent::async_tool_framework::AsyncExecutor,
    ) -> SyncToAsyncAdapter<Self>
    where
        Self: Sized,
    {
        SyncToAsyncAdapter::with_executor(Arc::new(self), executor)
    }
}

// Blanket implementation for all Tools
impl<T: Tool> ToolAsyncExt for T {}

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

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        async fn execute(&self, params: Value) -> Result<Value> {
            // Simulate work
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            Ok(params)
        }
    }

    #[tokio::test]
    async fn test_sync_to_async_adapter_creation() {
        let tool = MockTool {
            name: "test_tool".to_string(),
        };
        let adapter = tool.into_async();

        assert_eq!(adapter.name(), "test_tool");
        assert!(adapter.supports_async());
        assert!(adapter.supports_status_check());
        assert!(!adapter.supports_cancel());
    }

    #[tokio::test]
    async fn test_sync_to_async_adapter_execution() {
        let tool = MockTool {
            name: "test_tool".to_string(),
        };
        let adapter = tool.into_async();

        // Test that sync execution still works
        let result = adapter.execute(serde_json::json!({"test": true})).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), serde_json::json!({"test": true}));
    }

    #[test]
    fn test_tool_async_ext() {
        let tool = MockTool {
            name: "ext_tool".to_string(),
        };
        let adapter = tool.into_async();

        assert_eq!(adapter.name(), "ext_tool");
    }
}
