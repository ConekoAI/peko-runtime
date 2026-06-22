//! Tool registration composite and auto-generated companion handlers
//!
//! This module provides the [`ToolRegistration`] composite that tracks all hooks
//! belonging to a single tool, enabling atomic registration/unregistration.
//!
//! It also provides default handler implementations for the companion hooks that
//! `ExtensionCore::register_tool()` auto-generates:
//! - [`AutoPromptHandler`] — injects tool description into system prompt
//! - [`AutoAsyncHandler`] — returns an async receipt (delegates to sync execution)
//! - [`AutoStatusHandler`] — returns `Pending` status
//! - [`AutoCancelHandler`] — returns `false` (cancellation not supported by default)
//!
//! These handlers are intentionally simple and generic. Adapters that need custom
//! behaviour should provide their own execution handler; the registry handles the rest.

use crate::extensions::framework::core::context::HookContext;
use crate::extensions::framework::core::handler::HookHandler;
use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::types::{
    AsyncReceipt, AsyncTaskStatus, ExtensionId, HookId, HookOutput, HookResult, ToolMetadata,
};
use async_trait::async_trait;
#[cfg(test)]
use std::sync::Arc;
use tracing::debug;
use uuid::Uuid;

// ═══════════════════════════════════════════════════════════════════════════════
// ToolRegistration composite
// ═══════════════════════════════════════════════════════════════════════════════

/// Composite handle for all hooks belonging to a single tool.
///
/// When a tool is registered via [`ExtensionCore::register_tool`], this struct
/// captures every [`HookId`] that was created (execution, prompt, async, status,
/// cancel).  Unregistering the tool via [`ExtensionCore::unregister_tool`] uses
/// these IDs to remove **all** associated hooks atomically.
#[derive(Debug, Clone)]
pub struct ToolRegistration {
    /// Name of the registered tool
    pub tool_name: String,

    /// IDs of all hooks created for this tool (in registration order)
    pub hook_ids: Vec<HookId>,

    /// The primary execution hook ID (also the first element of `hook_ids`)
    pub primary_hook_id: HookId,

    /// Extension that owns this tool
    pub extension_id: ExtensionId,
}

impl ToolRegistration {
    /// Create a new tool registration composite.
    #[must_use]
    pub fn new(
        tool_name: impl Into<String>,
        hook_ids: Vec<HookId>,
        extension_id: ExtensionId,
    ) -> Self {
        let tool_name = tool_name.into();
        let primary_hook_id = hook_ids.first().copied().unwrap_or_else(HookId::new);
        Self {
            tool_name,
            hook_ids,
            primary_hook_id,
            extension_id,
        }
    }

    /// Total number of hooks registered for this tool.
    #[must_use]
    pub fn hook_count(&self) -> usize {
        self.hook_ids.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// AutoPromptHandler
// ═══════════════════════════════════════════════════════════════════════════════

/// Default handler for `PromptSystemSection { section: "tools" }`.
///
/// Injects a markdown-formatted tool description into the system prompt.
#[derive(Debug, Clone)]
pub(crate) struct AutoPromptHandler {
    tool_name: String,
    description: String,
    priority: i32,
}

impl AutoPromptHandler {
    /// Create from [`ToolMetadata`].
    #[must_use]
    pub(crate) fn from_metadata(metadata: &ToolMetadata, priority: i32) -> Self {
        Self {
            tool_name: metadata.name.clone(),
            description: metadata.description.clone(),
            priority,
        }
    }
}

#[async_trait]
impl HookHandler for AutoPromptHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        let text = format!("### {}\n\n{}", self.tool_name, self.description);
        HookResult::Continue(HookOutput::Text(text))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "tools".to_string(),
            priority: self.priority,
        }
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn name(&self) -> String {
        format!("AutoPrompt({})", self.tool_name)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// AutoAsyncHandler
// ═══════════════════════════════════════════════════════════════════════════════

/// Default handler for `ToolExecuteAsync`.
///
/// Returns an [`AsyncReceipt`] with a generated task ID.  The actual execution
/// is expected to be performed by the synchronous `ToolExecute` handler; this
/// handler merely provides the async plumbing.
#[derive(Debug, Clone)]
pub(crate) struct AutoAsyncHandler {
    tool_name: String,
    priority: i32,
}

impl AutoAsyncHandler {
    /// Create from [`ToolMetadata`].
    #[must_use]
    pub(crate) fn from_metadata(metadata: &ToolMetadata, priority: i32) -> Self {
        Self {
            tool_name: metadata.name.clone(),
            priority,
        }
    }
}

#[async_trait]
impl HookHandler for AutoAsyncHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Validate this is the right tool
        match ctx.as_tool_call() {
            Some((tool_name, _, _)) if tool_name != self.tool_name => {
                return HookResult::PassThrough;
            }
            None => return HookResult::PassThrough,
            _ => {}
        }

        debug!(tool_name = %self.tool_name, "Auto-async execution: returning receipt");

        let task_id = format!("auto:{}:{}", self.tool_name, Uuid::new_v4());

        let receipt = AsyncReceipt {
            task_id,
            estimated_duration_secs: None,
            task_file: None,
            metadata: Some(serde_json::json!({
                "tool_name": self.tool_name,
                "auto_generated": true,
            })),
        };

        HookResult::Continue(HookOutput::Receipt(receipt))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolExecuteAsync {
            tool_name: self.tool_name.clone(),
        }
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn name(&self) -> String {
        format!("AutoAsync({})", self.tool_name)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// AutoStatusHandler
// ═══════════════════════════════════════════════════════════════════════════════

/// Default handler for `ToolCheckStatus`.
///
/// Returns [`AsyncTaskStatus::Pending`] — adapters that support true async
/// tracking should override this via a custom handler.
#[derive(Debug, Clone)]
pub(crate) struct AutoStatusHandler {
    tool_name: String,
    priority: i32,
}

impl AutoStatusHandler {
    /// Create from [`ToolMetadata`].
    #[must_use]
    pub(crate) fn from_metadata(metadata: &ToolMetadata, priority: i32) -> Self {
        Self {
            tool_name: metadata.name.clone(),
            priority,
        }
    }
}

#[async_trait]
impl HookHandler for AutoStatusHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Validate this is the right tool
        match ctx.as_task_status() {
            Some((_, tool_name)) if tool_name != self.tool_name => {
                return HookResult::PassThrough;
            }
            None => return HookResult::PassThrough,
            _ => {}
        }

        debug!(tool_name = %self.tool_name, "Auto-status check: returning Pending");

        HookResult::Continue(HookOutput::TaskStatus(AsyncTaskStatus::Pending))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolCheckStatus {
            tool_name: self.tool_name.clone(),
        }
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn name(&self) -> String {
        format!("AutoStatus({})", self.tool_name)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// AutoCancelHandler
// ═══════════════════════════════════════════════════════════════════════════════

/// Default handler for `ToolCancel`.
///
/// Returns `false` — cancellation is not supported by default.
#[derive(Debug, Clone)]
pub(crate) struct AutoCancelHandler {
    tool_name: String,
    priority: i32,
}

impl AutoCancelHandler {
    /// Create from [`ToolMetadata`].
    #[must_use]
    pub(crate) fn from_metadata(metadata: &ToolMetadata, priority: i32) -> Self {
        Self {
            tool_name: metadata.name.clone(),
            priority,
        }
    }
}

#[async_trait]
impl HookHandler for AutoCancelHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Validate this is the right tool
        match ctx.as_task_cancel() {
            Some((_, tool_name)) if tool_name != self.tool_name => {
                return HookResult::PassThrough;
            }
            None => return HookResult::PassThrough,
            _ => {}
        }

        debug!(tool_name = %self.tool_name, "Auto-cancel: returning false");

        HookResult::Continue(HookOutput::Bool(false))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolCancel {
            tool_name: self.tool_name.clone(),
        }
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn name(&self) -> String {
        format!("AutoCancel({})", self.tool_name)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::core::config::ExtensionServices;

    fn sample_metadata(name: &str) -> ToolMetadata {
        ToolMetadata::new(
            name.to_string(),
            format!("The {name} tool"),
            serde_json::json!({"type": "object"}),
            crate::extensions::framework::types::ToolSource::BuiltIn,
        )
    }

    #[tokio::test]
    async fn test_auto_prompt_handler() {
        let meta = sample_metadata("test_tool");
        let handler = AutoPromptHandler::from_metadata(&meta, 100);

        let ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;
        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(text.contains("### test_tool"));
                assert!(text.contains("The test_tool tool"));
            }
            _ => panic!("Expected Continue with Text, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_auto_async_handler() {
        let meta = sample_metadata("test_tool");
        let handler = AutoAsyncHandler::from_metadata(&meta, 100);

        let ctx = HookContext::new(
            HookPoint::ToolExecuteAsync {
                tool_name: "test_tool".to_string(),
            },
            crate::extensions::framework::types::HookInput::ToolCall {
                tool_name: "test_tool".to_string(),
                params: serde_json::json!({}),
                workspace: None,
                agent_id: None,
                session_id: None,
                caller_id: None,
            },
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;
        match result {
            HookResult::Continue(HookOutput::Receipt(receipt)) => {
                assert!(receipt.task_id.starts_with("auto:test_tool:"));
            }
            _ => panic!("Expected Continue with Receipt, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_auto_async_handler_wrong_tool() {
        let meta = sample_metadata("test_tool");
        let handler = AutoAsyncHandler::from_metadata(&meta, 100);

        let ctx = HookContext::new(
            HookPoint::ToolExecuteAsync {
                tool_name: "other_tool".to_string(),
            },
            crate::extensions::framework::types::HookInput::ToolCall {
                tool_name: "other_tool".to_string(),
                params: serde_json::json!({}),
                workspace: None,
                agent_id: None,
                session_id: None,
                caller_id: None,
            },
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough for wrong tool, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_auto_status_handler() {
        let meta = sample_metadata("test_tool");
        let handler = AutoStatusHandler::from_metadata(&meta, 100);

        let ctx = HookContext::new(
            HookPoint::ToolCheckStatus {
                tool_name: "test_tool".to_string(),
            },
            crate::extensions::framework::types::HookInput::TaskStatus {
                task_id: "task-123".to_string(),
                tool_name: "test_tool".to_string(),
            },
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;
        match result {
            HookResult::Continue(HookOutput::TaskStatus(AsyncTaskStatus::Pending)) => {}
            _ => panic!("Expected Continue with Pending, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_auto_cancel_handler() {
        let meta = sample_metadata("test_tool");
        let handler = AutoCancelHandler::from_metadata(&meta, 100);

        let ctx = HookContext::new(
            HookPoint::ToolCancel {
                tool_name: "test_tool".to_string(),
            },
            crate::extensions::framework::types::HookInput::TaskCancel {
                task_id: "task-123".to_string(),
                tool_name: "test_tool".to_string(),
            },
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;
        match result {
            HookResult::Continue(HookOutput::Bool(false)) => {}
            _ => panic!("Expected Continue with Bool(false), got {result:?}"),
        }
    }

    #[test]
    fn test_tool_registration_composite() {
        let ids = vec![HookId::new(), HookId::new(), HookId::new()];
        let primary = ids[0];
        let reg = ToolRegistration::new("my_tool", ids.clone(), ExtensionId::new("test:ext"));

        assert_eq!(reg.tool_name, "my_tool");
        assert_eq!(reg.hook_count(), 3);
        assert_eq!(reg.primary_hook_id, primary);
        assert_eq!(reg.extension_id.0, "test:ext");
    }
}
