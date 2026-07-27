//! Tool registration composite and auto-generated companion handlers
//!
//! This module provides the [`ToolRegistration`] composite that tracks all hooks
//! belonging to a single tool, enabling atomic registration/unregistration.
//!
//! It also provides default handler implementations for the companion hooks that
//! `ExtensionCore::register_tool()` auto-generates:
//! - [`AutoPromptHandler`] — injects tool description into system prompt
//! - [`AutoAsyncHandler`] — passes through to the executor-backed async fallback
//! - [`AutoStatusHandler`] — passes through to executor status tracking
//! - [`AutoCancelHandler`] — passes through to executor cancellation
//!
//! These handlers are intentionally simple and generic. With the exception of the
//! prompt handler, they all return [`HookResult::PassThrough`]: a tool with no custom
//! async support has no *native* async handler, so the async bridge must fall back to
//! its executor (which actually spawns, tracks, and cancels the background work).
//! Returning a concrete default here would shadow that fallback and strand the task.
//! Adapters that need real native async should register their own higher-priority
//! handler; the registry handles the rest.

use crate::extensions::framework::core::context::HookContext;
use crate::extensions::framework::core::handler::HookHandler;
use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::types::{
    Capabilities, Capability, ExtensionId, HookId, HookOutput, HookResult, ToolExposure,
    ToolMetadata, ToolRuntimeContext,
};
use async_trait::async_trait;
#[cfg(test)]
use std::sync::Arc;
use tracing::debug;

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
    /// F34 — the LLM exposure from the tool's metadata. Suppressed from
    /// the prompt section unless `visible_in_prompt_section()` is true.
    /// `DirectModelOnly`, `Deferred`, and `Hidden` tools skip the
    /// prompt section entirely.
    exposure: ToolExposure,
    extension_id: ExtensionId,
    priority: i32,
}

impl AutoPromptHandler {
    /// Create from [`ToolMetadata`].
    #[must_use]
    pub(crate) fn from_metadata(
        metadata: &ToolMetadata,
        extension_id: ExtensionId,
        priority: i32,
    ) -> Self {
        Self {
            tool_name: metadata.name.clone(),
            description: metadata.description.clone(),
            exposure: metadata.exposure,
            extension_id,
            priority,
        }
    }
}

#[async_trait]
impl HookHandler for AutoPromptHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // F34 — exposure gate. Suppress from the prompt section unless
        // the tool's exposure says it belongs here. Capability gate runs
        // second so a `Hidden` tool is dropped without consulting the
        // principal's grants.
        if !self.exposure.visible_in_prompt_section() {
            debug!(
                tool_name = %self.tool_name,
                exposure = ?self.exposure,
                "Auto-prompt suppressed: exposure hides tool from prompt section"
            );
            return HookResult::PassThrough;
        }

        // Capability gate: every registered tool fires this hook, so the
        // prompt must fail closed when principal authorization context is
        // absent or the matching tool grant is missing. Legacy callers that
        // raise the prompt section without seeding `tool_context` are treated
        // as having an empty grant set; that is consistent with the canonical
        // execution funnel.
        let (capabilities, active_extensions) =
            match ctx.get_state::<ToolRuntimeContext>("tool_context") {
                Some(tc) => (
                    tc.capabilities.clone().unwrap_or_default(),
                    tc.active_extensions.clone(),
                ),
                None => (Vec::new(), None),
            };
        if capabilities.is_empty() {
            return HookResult::PassThrough;
        }
        let granted = Capabilities::with_grants(capabilities);
        if !granted.is_granted(&Capability::new(format!("tool:{}", self.tool_name))) {
            debug!(
                tool_name = %self.tool_name,
                "Auto-prompt suppressed: capability not granted"
            );
            return HookResult::PassThrough;
        }

        // Match the native catalog's owner gate. Built-ins are always present
        // and need only their tool grant; extension-owned tools must also have
        // their owner in the principal's active-extension snapshot. Empty or
        // missing snapshots fail closed for non-builtin owners.
        if !self.extension_id.0.starts_with("builtin:tool:") {
            let owner_active = active_extensions
                .as_ref()
                .is_some_and(|active| active.iter().any(|id| id == &self.extension_id.0));
            if !owner_active {
                debug!(
                    tool_name = %self.tool_name,
                    extension_id = %self.extension_id,
                    "Auto-prompt suppressed: owning extension is inactive"
                );
                return HookResult::PassThrough;
            }
        }

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
/// Returns [`HookResult::PassThrough`]: a tool with no custom async support has no
/// *native* async execution, so the async bridge falls back to its executor, which
/// runs the synchronous `ToolExecute` handler in the background and issues a receipt
/// for a task that actually exists. Returning a fabricated receipt here would mark the
/// tool as natively-async, bypass that fallback, and strand the caller polling a task
/// that never runs. Adapters with real native async register a higher-priority handler.
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
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        // No native async support — defer to the executor-backed fallback in the
        // async bridge. See module docs for why a default receipt must not be returned.
        debug!(tool_name = %self.tool_name, "Auto-async: passing through to executor fallback");
        HookResult::PassThrough
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
/// Returns [`HookResult::PassThrough`] so the async bridge consults the executor,
/// which holds the real status of fallback-spawned tasks. Returning a hardcoded
/// `Pending` here would shadow the executor and report every task as perpetually
/// pending. Adapters with native async tracking register a higher-priority handler.
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
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        // No native status tracking — defer to the executor registry in the async bridge.
        debug!(tool_name = %self.tool_name, "Auto-status: passing through to executor fallback");
        HookResult::PassThrough
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
/// Returns [`HookResult::PassThrough`] so the async bridge delegates cancellation to
/// the executor, which can actually cancel fallback-spawned tasks. Returning a
/// hardcoded `false` here would shadow the executor and make every task uncancellable.
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
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        // No native cancellation — defer to the executor in the async bridge.
        debug!(tool_name = %self.tool_name, "Auto-cancel: passing through to executor fallback");
        HookResult::PassThrough
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
        let handler =
            AutoPromptHandler::from_metadata(&meta, ExtensionId::new("builtin:tool:test"), 100);

        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        ctx.set_state(
            "tool_context",
            ToolRuntimeContext::new().with_capabilities(["tool:test_tool"]),
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

    /// When the principal's grants do not include `tool:<name>`, the
    /// auto-prompt handler must omit the entry from the system prompt's
    /// "Available Tools" section. Without this gate the prompt section
    /// would lie about what the principal can invoke, and after a
    /// `peko capability revoke` the LLM would still try to call revoked
    /// tools — falling back to raw `<tool_call>` text since the native
    /// tool catalog is now empty.
    #[tokio::test]
    async fn auto_prompt_suppressed_when_capability_not_granted() {
        let meta = sample_metadata("Read");
        let handler =
            AutoPromptHandler::from_metadata(&meta, ExtensionId::new("builtin:tool:test"), 100);

        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        let tc = ToolRuntimeContext::new().with_capabilities(["tool:Write".to_string()]);
        ctx.set_state("tool_context", tc);

        let result = handler.handle(ctx).await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "expected PassThrough when tool:Read is not granted, got {result:?}"
        );
    }

    /// The complementary case: when the grant IS present the handler
    /// emits the tool description as before.
    #[tokio::test]
    async fn auto_prompt_emitted_when_capability_granted() {
        let meta = sample_metadata("Read");
        let handler =
            AutoPromptHandler::from_metadata(&meta, ExtensionId::new("builtin:tool:test"), 100);

        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        // Grant uses the registered tool-name casing; capability matching is
        // exact apart from explicit trailing-wildcard grants.
        let tc = ToolRuntimeContext::new().with_capabilities(["tool:Read".to_string()]);
        ctx.set_state("tool_context", tc);

        let result = handler.handle(ctx).await;
        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(text.contains("### Read"));
                assert!(text.contains("The Read tool"));
            }
            other => panic!("Expected Continue with Text, got {other:?}"),
        }
    }

    /// `tool:*` is a wildcard that satisfies the per-tool grant check.
    #[tokio::test]
    async fn auto_prompt_wildcard_capability_covers_tool() {
        let meta = sample_metadata("Bash");
        let handler =
            AutoPromptHandler::from_metadata(&meta, ExtensionId::new("builtin:tool:test"), 100);

        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        let tc = ToolRuntimeContext::new().with_capabilities(["tool:*".to_string()]);
        ctx.set_state("tool_context", tc);

        let result = handler.handle(ctx).await;
        assert!(
            matches!(result, HookResult::Continue(HookOutput::Text(_))),
            "wildcard grant should permit the prompt, got {result:?}"
        );
    }

    #[tokio::test]
    async fn auto_prompt_suppresses_inactive_extension_tool() {
        let meta = sample_metadata("Search");
        let handler =
            AutoPromptHandler::from_metadata(&meta, ExtensionId::new("extension:search"), 100);
        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        ctx.set_state(
            "tool_context",
            ToolRuntimeContext::new()
                .with_capabilities(["tool:Search"])
                .with_active_extensions(Vec::<String>::new()),
        );

        assert!(matches!(handler.handle(ctx).await, HookResult::PassThrough));
    }

    #[tokio::test]
    async fn auto_prompt_emits_active_extension_tool() {
        let meta = sample_metadata("Search");
        let handler =
            AutoPromptHandler::from_metadata(&meta, ExtensionId::new("extension:search"), 100);
        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        ctx.set_state(
            "tool_context",
            ToolRuntimeContext::new()
                .with_capabilities(["tool:Search"])
                .with_active_extensions(["extension:search"]),
        );

        assert!(matches!(
            handler.handle(ctx).await,
            HookResult::Continue(HookOutput::Text(_))
        ));
    }

    #[tokio::test]
    async fn auto_prompt_fails_closed_without_authorization_context() {
        let meta = sample_metadata("Read");
        let handler =
            AutoPromptHandler::from_metadata(&meta, ExtensionId::new("builtin:tool:Read"), 100);
        let ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );

        assert!(matches!(handler.handle(ctx).await, HookResult::PassThrough));
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
                principal_id: None,
                principal_name: None,
                capabilities: None,
                active_extensions: None,
                abort_signal: None,
            },
            Arc::new(ExtensionServices::new()),
        );

        // The default handler has no native async support; it must pass through so the
        // async bridge falls back to its executor (which actually spawns the work).
        let result = handler.handle(ctx).await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough so the executor fallback runs, got {result:?}"
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

        // Must pass through so the executor reports the real task status.
        let result = handler.handle(ctx).await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough so the executor reports real status, got {result:?}"
        );
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

        // Must pass through so the executor can actually cancel the task.
        let result = handler.handle(ctx).await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough so the executor handles cancellation, got {result:?}"
        );
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
