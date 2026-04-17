//! Extension Async Adapter
//!
//! Bridges `ExtensionCore` async hooks to the `UnifiedAsyncExecutor` framework.
//!
//! This adapter enables extensions (MCP, Universal Tools, etc.) to participate
//! in the async tool execution ecosystem with the same capabilities as built-in tools:
//! - Return receipts for immediate non-blocking response
//! - Background task execution
//! - Status polling
//! - Cancellation support
//! - Result queuing and delivery
//!
//! # Architecture
//!
//! ```
//! ToolExecutor ──▶ ExtensionAsyncAdapter ──▶ ExtensionCore (hooks)
//!       │                                          │
//!       └──────────▶ UnifiedAsyncExecutor ◄────────┘
//!                         │
//!                         ▼
//!              ┌─────────────────────┐
//!              │ AsyncTaskRegistry   │
//!              │ ResultQueueManager  │
//!              │ DeliveryMechanisms  │
//!              └─────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! let adapter = ExtensionAsyncAdapter::new(extension_core);
//!
//! // Execute tool asynchronously via extension
//! let receipt = adapter.execute_async("my_tool", params, session_key).await?;
//!
//! // Check status later
//! let status = adapter.check_status("my_tool", &receipt.task_id).await?;
//!
//! // Cancel if needed
//! let cancelled = adapter.cancel("my_tool", &receipt.task_id).await?;
//! ```

use crate::agent::async_tool_framework::{
    AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus, AsyncToolConfig, UnifiedAsyncExecutor,
    WaitResult,
};
use crate::extensions::core::{ExtensionCore, HookPointBuilder};
use crate::extensions::types::{AsyncReceipt, HookInput, HookOutput, HookResult};
use anyhow::{anyhow, Result};

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Bridges `ExtensionCore` hooks to `UnifiedAsyncExecutor`
///
/// This adapter provides a unified interface for async tool execution that works
/// with both extensions that implement async hooks and those that don't.
#[derive(Clone)]
pub struct ExtensionAsyncAdapter {
    /// Extension core for invoking hooks
    core: Arc<ExtensionCore>,

    /// Unified async executor for background task management
    executor: UnifiedAsyncExecutor,

    /// Cache of extension capabilities (which tools support async)
    capability_cache: Arc<RwLock<HashMap<String, AsyncCapability>>>,
}

/// Capability information for a tool's async support
#[derive(Debug, Clone)]
struct AsyncCapability {
    /// Whether the tool supports native async via hooks
    supports_native_async: bool,

    /// Whether the tool supports status checking
    supports_status_check: bool,

    /// Whether the tool supports cancellation
    supports_cancel: bool,

    /// Name of the status check tool (if different from default)
    status_tool_name: Option<String>,
}

impl ExtensionAsyncAdapter {
    /// Create a new async adapter with default executor
    pub fn new(core: Arc<ExtensionCore>) -> Self {
        Self {
            core,
            executor: UnifiedAsyncExecutor::new(),
            capability_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create with a custom executor (for sharing registries)
    pub fn with_executor(core: Arc<ExtensionCore>, executor: UnifiedAsyncExecutor) -> Self {
        Self {
            core,
            executor,
            capability_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get the underlying `ExtensionCore`
    #[must_use] 
    pub fn core(&self) -> &Arc<ExtensionCore> {
        &self.core
    }

    /// Execute a tool asynchronously
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool to execute
    /// * `params` - Tool parameters
    /// * `session_key` - Session key for result routing
    ///
    /// # Returns
    /// Receipt with `task_id` and status information
    pub async fn execute_async(
        &self,
        tool_name: &str,
        params: Value,
        session_key: impl Into<String>,
    ) -> Result<AsyncTaskReceipt> {
        let session_key = session_key.into();

        debug!(tool_name, session_key, "Executing tool asynchronously");

        // 1. Try native async hook first
        match self
            .try_native_async(tool_name, &params, &session_key)
            .await
        {
            Ok(receipt) => {
                info!(tool_name, task_id = %receipt.task_id, "Native async execution started");
                return Ok(receipt);
            }
            Err(e) => {
                debug!(tool_name, error = %e, "Native async not available, using fallback");
            }
        }

        // 2. Fallback: execute sync tool in background
        self.fallback_async(tool_name, params, session_key).await
    }

    /// Try native async execution via `ToolExecuteAsync` hook
    async fn try_native_async(
        &self,
        tool_name: &str,
        params: &Value,
        session_key: &str,
    ) -> Result<AsyncTaskReceipt> {
        let hook_point = HookPointBuilder::tool_execute_async(tool_name);

        let result = self
            .core
            .invoke_hook(
                hook_point,
                HookInput::ToolCall {
                    tool_name: tool_name.to_string(),
                    params: params.clone(),
                    workspace: None,
                },
            )
            .await;

        match result {
            HookResult::Continue(HookOutput::Receipt(receipt)) => {
                // Extension handles async - register with our executor for status tracking
                self.register_extension_receipt(tool_name, &receipt, session_key)
                    .await?;

                Ok(AsyncTaskReceipt {
                    task_id: receipt.task_id,
                    status: AsyncTaskStatus::Pending,
                    estimated_duration_secs: receipt.estimated_duration_secs,
                    task_file: receipt.task_file,
                })
            }
            HookResult::PassThrough => Err(anyhow!(
                "No async handler registered for tool {tool_name}"
            )),
            HookResult::Error(e) => Err(anyhow!("Async execution error: {e}")),
            _ => Err(anyhow!("Unexpected hook result for async execution")),
        }
    }

    /// Fallback: Execute sync tool in background via executor
    async fn fallback_async(
        &self,
        tool_name: &str,
        params: Value,
        session_key: String,
    ) -> Result<AsyncTaskReceipt> {
        let task_id = format!("{}_{}", tool_name, Uuid::new_v4().simple());
        let core = self.core.clone();
        let tool_name_clone = tool_name.to_string();

        info!(tool_name, task_id, "Starting fallback async execution");

        // Execute using UnifiedAsyncExecutor
        let receipt = self
            .executor
            .execute(
                task_id.clone(),
                tool_name,
                params.clone(),
                session_key,
                AsyncToolConfig::default(),
                move || async move {
                    // Invoke sync ToolExecute hook
                    let result = core
                        .invoke_hook(
                            HookPointBuilder::tool_execute(&tool_name_clone),
                            HookInput::ToolCall {
                                tool_name: tool_name_clone,
                                params,
                                workspace: None,
                            },
                        )
                        .await;

                    // Convert HookResult to AsyncTaskResult
                    match result {
                        HookResult::Continue(HookOutput::Json(json)) => {
                            Ok(AsyncTaskResult::Generic { data: json })
                        }
                        HookResult::Continue(HookOutput::Text(text)) => {
                            Ok(AsyncTaskResult::Generic {
                                data: json!({"result": text}),
                            })
                        }
                        HookResult::Error(e) => Err(anyhow!("Tool execution failed: {e}")),
                        _ => Err(anyhow!("Unexpected result from sync tool execution")),
                    }
                },
            )
            .await?;

        Ok(receipt)
    }

    /// Check status of an async task
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool
    /// * `task_id` - Task identifier from receipt
    ///
    /// # Returns
    /// Current status of the task
    pub async fn check_status(&self, tool_name: &str, task_id: &str) -> Result<AsyncTaskStatus> {
        // 1. Try native status check hook
        let hook_point = HookPointBuilder::tool_check_status(tool_name);

        let result = self
            .core
            .invoke_hook(
                hook_point,
                HookInput::TaskStatus {
                    task_id: task_id.to_string(),
                    tool_name: tool_name.to_string(),
                },
            )
            .await;

        match result {
            HookResult::Continue(HookOutput::TaskStatus(status)) => {
                return Ok(status);
            }
            HookResult::PassThrough => {
                // No native status hook, try executor
            }
            HookResult::Error(e) => {
                warn!(tool_name, task_id, error = %e, "Status check hook failed");
            }
            _ => {}
        }

        // 2. Fallback: Check executor registry
        self.executor
            .check_status(&task_id.to_string())
            .await
            .ok_or_else(|| anyhow!("Task not found: {task_id}"))
    }

    /// Cancel an async task
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool
    /// * `task_id` - Task identifier from receipt
    ///
    /// # Returns
    /// true if cancellation succeeded, false otherwise
    pub async fn cancel(&self, tool_name: &str, task_id: &str) -> Result<bool> {
        // 1. Try native cancel hook
        let hook_point = HookPointBuilder::tool_cancel(tool_name);

        let result = self
            .core
            .invoke_hook(
                hook_point,
                HookInput::TaskCancel {
                    task_id: task_id.to_string(),
                    tool_name: tool_name.to_string(),
                },
            )
            .await;

        match result {
            HookResult::Continue(HookOutput::Bool(success)) => {
                return Ok(success);
            }
            HookResult::PassThrough => {
                // No native cancel hook, try executor
            }
            HookResult::Error(e) => {
                warn!(tool_name, task_id, error = %e, "Cancel hook failed");
            }
            _ => {}
        }

        // 2. Fallback: Cancel via executor
        self.executor.cancel(&task_id.to_string()).await
    }

    /// Wait for task completion with timeout
    ///
    /// # Arguments
    /// * `task_id` - Task identifier
    /// * `timeout` - Maximum time to wait
    ///
    /// # Returns
    /// Result of the wait operation
    pub async fn wait_for_completion(
        &self,
        task_id: &str,
        timeout: std::time::Duration,
    ) -> Result<WaitResult> {
        self.executor
            .wait_for_completion(&task_id.to_string(), timeout)
            .await
    }

    /// Get a reference to the underlying executor
    #[must_use] 
    pub fn executor(&self) -> &UnifiedAsyncExecutor {
        &self.executor
    }

    /// Register an extension receipt for tracking
    async fn register_extension_receipt(
        &self,
        tool_name: &str,
        receipt: &AsyncReceipt,
        session_key: &str,
    ) -> Result<()> {
        let mut cache = self.capability_cache.write().await;
        cache.insert(
            tool_name.to_string(),
            AsyncCapability {
                supports_native_async: true,
                supports_status_check: true,
                supports_cancel: true,
                status_tool_name: None,
            },
        );

        debug!(
            tool_name,
            task_id = %receipt.task_id,
            session_key,
            "Registered extension async receipt"
        );

        Ok(())
    }

    /// Check if a tool supports native async execution
    pub async fn supports_native_async(&self, tool_name: &str) -> bool {
        // Check cache first
        let cache = self.capability_cache.read().await;
        if let Some(cap) = cache.get(tool_name) {
            return cap.supports_native_async;
        }
        drop(cache);

        // Probe by invoking the hook
        let hook_point = HookPointBuilder::tool_execute_async(tool_name);
        let result = self.core.invoke_hook(hook_point, HookInput::Unit).await;

        // Check if any handler is registered (not PassThrough)
        !matches!(result, HookResult::PassThrough)
    }
}

impl std::fmt::Debug for ExtensionAsyncAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionAsyncAdapter")
            .field("core", &"<ExtensionCore>")
            .field("executor", &self.executor)
            .field("capability_cache", &"<HashMap>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::core::context::{HookContext, HookHandler};
    use crate::extensions::core::HookPoint;
    use crate::extensions::ExtensionId;
    use async_trait::async_trait;

    /// Mock async handler for testing
    #[derive(Debug)]
    struct MockAsyncHandler {
        tool_name: String,
    }

    #[async_trait]
    impl HookHandler for MockAsyncHandler {
        async fn handle(&self, ctx: HookContext) -> HookResult {
            if let HookInput::ToolCall { tool_name, .. } = ctx.input() {
                if tool_name == self.tool_name.as_str() {
                    let receipt = AsyncReceipt::new(
                        format!("task_{}", Uuid::new_v4().simple()),
                    )
                    .with_duration(60);

                    return HookResult::Continue(HookOutput::Receipt(receipt));
                }
            }
            HookResult::PassThrough
        }

        fn hook_point(&self) -> HookPoint {
            HookPointBuilder::tool_execute_async(&self.tool_name)
        }
    }

    #[tokio::test]
    async fn test_extension_async_adapter_creation() {
        let core = Arc::new(ExtensionCore::new());
        let adapter = ExtensionAsyncAdapter::new(core);

        assert!(!(adapter.supports_native_async("unknown_tool").await));
    }

    #[tokio::test]
    async fn test_native_async_execution() {
        let core = Arc::new(ExtensionCore::new());

        // Register mock async handler
        let handler = Arc::new(MockAsyncHandler {
            tool_name: "test_tool".to_string(),
        });
        core.register_hook(
            HookPointBuilder::tool_execute_async("test_tool"),
            handler,
            &ExtensionId::new("test"),
        )
        .await
        .unwrap();

        let adapter = ExtensionAsyncAdapter::new(core);

        // Execute async
        let receipt = adapter
            .execute_async("test_tool", json!({"param": "value"}), "session:abc")
            .await
            .unwrap();

        assert!(receipt.task_id.starts_with("task_"));
        assert!(receipt.task_file.is_none());
        assert_eq!(receipt.estimated_duration_secs, Some(60));
        assert!(matches!(receipt.status, AsyncTaskStatus::Pending));
    }

    #[tokio::test]
    async fn test_fallback_async_execution() {
        let core = Arc::new(ExtensionCore::new());

        // Register sync handler only (no async handler)
        #[derive(Debug)]
        struct SyncHandler;

        #[async_trait]
        impl HookHandler for SyncHandler {
            async fn handle(&self, _ctx: HookContext) -> HookResult {
                HookResult::Continue(HookOutput::json(json!({"result": "success"})))
            }

            fn hook_point(&self) -> HookPoint {
                HookPointBuilder::tool_execute("sync_only_tool")
            }
        }

        core.register_hook(
            HookPointBuilder::tool_execute("sync_only_tool"),
            Arc::new(SyncHandler),
            &ExtensionId::new("test"),
        )
        .await
        .unwrap();

        let adapter = ExtensionAsyncAdapter::new(core);

        // Execute async - should use fallback
        let receipt = adapter
            .execute_async("sync_only_tool", json!({}), "session:abc")
            .await
            .unwrap();

        assert!(receipt.task_id.contains("sync_only_tool"));
        assert!(matches!(receipt.status, AsyncTaskStatus::Pending));
    }
}
