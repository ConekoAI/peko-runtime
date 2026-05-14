//! Extension Core registry
//!
//! This module implements the central facade for extension hooks and tools.
//! It composes `HookRegistry` and `ToolRegistry` to provide a unified interface.

use crate::extension::core::config::ExtensionServices;
#[cfg(test)]
use crate::extension::core::context::HookContext;
use crate::extension::core::handler::HookHandler;
use crate::extension::core::hook_points::HookPoint;
use crate::extension::core::hook_registry::HookRegistry;
use crate::extension::core::tool_registry::ToolRegistry;
use crate::extension::types::{ExtensionId, HookId, HookInput, HookResult, ToolMetadata};
use anyhow::Result;
use std::sync::Arc;
use tracing::{debug, instrument, trace, warn};

// Re-export types from sub-registries for backward compatibility
pub use crate::extension::core::hook_registry::{BuiltinExtensionInfo, RegisteredHook};

/// Central facade for extension hooks and tools
///
/// `ExtensionCore` composes `HookRegistry` and `ToolRegistry` to provide
/// a unified interface for hook and tool management. All hook-related
/// operations are delegated to `HookRegistry`, and all tool index/policy
/// operations are delegated to `ToolRegistry`.
#[derive(Debug)]
pub struct ExtensionCore {
    /// Hook registry
    hook_registry: Arc<HookRegistry>,

    /// Tool registry
    tool_registry: Arc<ToolRegistry>,

    /// Extension services shared across all handlers
    services: Arc<ExtensionServices>,
}

impl ExtensionCore {
    /// Create a new Extension Core
    #[must_use]
    pub fn new() -> Self {
        let services = Arc::new(ExtensionServices::new());
        Self {
            hook_registry: Arc::new(HookRegistry::with_services(services.clone())),
            tool_registry: Arc::new(ToolRegistry::new()),
            services,
        }
    }

    /// Create a new Extension Core with custom services
    pub fn with_services(services: Arc<ExtensionServices>) -> Self {
        Self {
            hook_registry: Arc::new(HookRegistry::with_services(services.clone())),
            tool_registry: Arc::new(ToolRegistry::new()),
            services,
        }
    }

    /// Get the hook registry
    pub fn hook_registry(&self) -> Arc<HookRegistry> {
        self.hook_registry.clone()
    }

    /// Get the tool registry
    pub fn tool_registry(&self) -> Arc<ToolRegistry> {
        self.tool_registry.clone()
    }

    /// Get the extension services
    #[must_use]
    pub fn services(&self) -> Arc<ExtensionServices> {
        self.services.clone()
    }

    /// Set the tool configuration (whitelist, etc.)
    pub async fn set_tool_config(&self, config: crate::types::agent::ExtensionConfig) {
        self.tool_registry.set_tool_config(config).await;
    }

    /// Check if a tool is enabled according to whitelist
    pub async fn is_tool_enabled(&self, tool_name: &str) -> bool {
        self.tool_registry.is_tool_enabled(tool_name).await
    }

    /// Wait for background async tasks to complete
    pub async fn wait_for_async_tasks(&self, timeout: std::time::Duration) {
        self.services.wait_for_async_tasks(timeout).await;
    }

    /// Register a hook handler
    ///
    /// # Arguments
    /// * `point` - The hook point to register at
    /// * `handler` - The handler implementation
    /// * `extension_id` - ID of the extension that owns this handler
    ///
    /// # Returns
    /// The registration information
    #[instrument(skip(self, handler), fields(extension_id = %extension_id))]
    pub async fn register_hook(
        &self,
        point: HookPoint,
        handler: Arc<dyn super::handler::HookHandler>,
        extension_id: &ExtensionId,
    ) -> Result<RegisteredHook> {
        self.hook_registry
            .register_hook(point, handler, extension_id)
            .await
    }

    /// Unregister a hook handler
    ///
    /// # Arguments
    /// * `hook_id` - The ID of the hook to unregister
    #[instrument(skip(self), fields(hook_id = %hook_id))]
    pub async fn unregister_hook(&self, hook_id: &HookId) -> Result<()> {
        self.hook_registry.unregister_hook(hook_id).await
    }

    /// Enable a hook handler
    #[instrument(skip(self), fields(hook_id = %hook_id))]
    pub async fn enable_hook(&self, hook_id: &HookId) -> Result<()> {
        self.hook_registry.enable_hook(hook_id).await
    }

    /// Disable a hook handler
    #[instrument(skip(self), fields(hook_id = %hook_id))]
    pub async fn disable_hook(&self, hook_id: &HookId) -> Result<()> {
        self.hook_registry.disable_hook(hook_id).await
    }

    /// Globally enable/disable all hooks
    pub async fn set_globally_enabled(&self, enabled: bool) {
        self.hook_registry.set_globally_enabled(enabled).await;
    }

    /// Check if hooks are globally enabled
    pub async fn is_globally_enabled(&self) -> bool {
        self.hook_registry.is_globally_enabled().await
    }

    /// Get all hooks for a specific extension
    pub async fn get_hooks_for_extension(&self, extension_id: &ExtensionId) -> Vec<RegisteredHook> {
        self.hook_registry
            .get_hooks_for_extension(extension_id)
            .await
    }

    /// Get all registered hooks
    pub async fn get_all_hooks(&self) -> Vec<RegisteredHook> {
        self.hook_registry.get_all_hooks().await
    }

    /// List all built-in extensions registered with this core
    ///
    /// Built-in extensions have IDs in the form `builtin:{type}:{name}`.
    /// This is type-agnostic — it will return tools, gateways, skills, or any
    /// future built-in extension type that registers hooks.
    pub async fn list_builtin_extensions(&self) -> Vec<BuiltinExtensionInfo> {
        self.hook_registry.list_builtin_extensions().await
    }

    /// Get hooks for a specific hook point
    ///
    /// Supports wildcard matching for tool execution hooks. If an exact match
    /// is not found, checks for wildcard patterns (e.g., "mcp:identity:*" matches
    /// "`mcp:identity:echo_identity`").
    pub async fn get_hooks_for_point(&self, point: &HookPoint) -> Vec<RegisteredHook> {
        self.hook_registry.get_hooks_for_point(point).await
    }

    /// Invoke hooks for a specific point
    ///
    /// This calls all registered and enabled handlers for the given hook point
    /// in priority order (highest first).
    ///
    /// # Arguments
    /// * `point` - The hook point to invoke
    /// * `input` - The input data for handlers
    ///
    /// # Returns
    /// Combined output from all handlers
    #[instrument(skip(self, input), fields(point = %point))]
    pub async fn invoke_hook(&self, point: HookPoint, input: HookInput) -> HookResult {
        // Check global enable
        if !self.hook_registry.is_globally_enabled().await {
            trace!("Hooks globally disabled, returning PassThrough");
            return HookResult::PassThrough;
        }

        // ADR-019 Phase 1: Tool permission check at ExtensionCore layer
        // This ensures ALL tools (built-in, MCP, Universal) are checked consistently
        if let HookPoint::ToolExecute { ref tool_name } = point {
            if !self.tool_registry.is_tool_enabled(tool_name).await {
                warn!(tool_name = %tool_name, "Tool execution blocked: tool is not enabled");
                return HookResult::Error(anyhow::anyhow!(
                    "Tool '{tool_name}' is currently disabled. Enable it in agent config to use it."
                ));
            }
            trace!(tool_name = %tool_name, "Tool execution permitted");
        }

        self.hook_registry.invoke_hook(point, input).await
    }

    /// Invoke hooks and return text output (convenience for prompt hooks)
    pub async fn invoke_hook_text(&self, point: HookPoint, input: HookInput) -> Option<String> {
        self.hook_registry.invoke_hook_text(point, input).await
    }

    /// Invoke hooks and return JSON output (convenience for data hooks)
    pub async fn invoke_hook_json(
        &self,
        point: HookPoint,
        input: HookInput,
    ) -> Option<serde_json::Value> {
        self.hook_registry.invoke_hook_json(point, input).await
    }

    /// Get the number of registered hooks
    pub async fn hook_count(&self) -> usize {
        self.hook_registry.hook_count().await
    }

    /// Get the number of registered hooks for a specific point
    pub async fn hook_count_for_point(&self, point: &HookPoint) -> usize {
        self.hook_registry.hook_count_for_point(point).await
    }

    // ==================== UNIFIED TOOL REGISTRY (ADR-018b) ====================

    /// Register a tool with the unified registry.
    ///
    /// This is the **single canonical path** for tool registration.  It performs an
    /// atomic composite operation:
    ///
    /// 1. Registers the adapter-supplied **execution handler**.
    /// 2. Auto-generates companion hooks (prompt, async, status, cancel) from the
    ///    [`ToolMetadata`].
    /// 3. Indexes the tool in [`ToolRegistry`] for O(1) metadata lookup.
    ///
    /// If the tool was previously registered, it is unregistered first (idempotent).
    ///
    /// # Arguments
    /// * `metadata` - Tool metadata (name, description, parameters, source, etc.)
    /// * `handler` - The adapter's execution handler (`HookPoint::ToolExecute`)
    /// * `extension_id` - ID of the extension that owns this tool
    ///
    /// # Returns
    /// A [`ToolRegistration`] composite containing all hook IDs created.
    #[instrument(skip(self, handler, metadata), fields(extension_id = %extension_id, tool_name = %metadata.name))]
    pub async fn register_tool(
        &self,
        metadata: ToolMetadata,
        handler: Arc<dyn super::handler::HookHandler>,
        extension_id: &ExtensionId,
    ) -> Result<super::tool_registration::ToolRegistration> {
        use super::tool_registration::{
            AutoAsyncHandler, AutoCancelHandler, AutoPromptHandler, AutoStatusHandler,
            ToolRegistration,
        };

        let tool_name = metadata.name.clone();
        let priority = handler.priority();

        // ADR-019: Allow re-registration for dynamic enable/disable
        // If tool already registered, unregister it first (idempotent)
        if self.get_tool_metadata(&tool_name).await.is_some() {
            let _ = self.unregister_tool(&tool_name).await;
        }

        let mut hook_ids: Vec<HookId> = Vec::with_capacity(5);

        // ── 1. Execution hook (adapter-provided business logic) ─────────────────
        let exec_point = handler.hook_point();
        let exec_hook_id = HookId::new();

        let exec_registration = RegisteredHook::with_tool_metadata(
            exec_hook_id,
            extension_id.clone(),
            exec_point.clone(),
            handler.clone(),
            priority,
            metadata.clone(),
        );

        {
            let mut hooks = self.hook_registry.hooks.write().await;
            hooks.insert(exec_hook_id, exec_registration);
        }

        {
            let mut by_point = self.hook_registry.hooks_by_point.write().await;
            let exec_point_name = exec_point.name();
            let entry = by_point.entry(exec_point_name).or_insert_with(Vec::new);
            entry.push(exec_hook_id);
            let hooks = self.hook_registry.hooks.read().await;
            entry.sort_by_key(|id| hooks.get(id).map_or(0, |h| -h.priority));
        }

        hook_ids.push(exec_hook_id);

        // ── 2. Prompt section hook (auto-generated) ─────────────────────────────
        let prompt_handler = Arc::new(AutoPromptHandler::from_metadata(&metadata, priority));
        let prompt_point = prompt_handler.hook_point();
        let prompt_reg = self
            .register_hook(prompt_point, prompt_handler, extension_id)
            .await?;
        hook_ids.push(prompt_reg.id);

        // ── 3. Async execution hook (auto-generated) ────────────────────────────
        let async_handler = Arc::new(AutoAsyncHandler::from_metadata(&metadata, priority));
        let async_point = async_handler.hook_point();
        let async_reg = self
            .register_hook(async_point, async_handler, extension_id)
            .await?;
        hook_ids.push(async_reg.id);

        // ── 4. Check status hook (auto-generated) ───────────────────────────────
        let status_handler = Arc::new(AutoStatusHandler::from_metadata(&metadata, priority));
        let status_point = status_handler.hook_point();
        let status_reg = self
            .register_hook(status_point, status_handler, extension_id)
            .await?;
        hook_ids.push(status_reg.id);

        // ── 5. Cancel hook (auto-generated) ─────────────────────────────────────
        let cancel_handler = Arc::new(AutoCancelHandler::from_metadata(&metadata, priority));
        let cancel_point = cancel_handler.hook_point();
        let cancel_reg = self
            .register_hook(cancel_point, cancel_handler, extension_id)
            .await?;
        hook_ids.push(cancel_reg.id);

        // ── 6. Store companion hook IDs in the primary registration's metadata ──
        //        so that unregister_tool() can clean them up atomically.
        {
            let mut hooks = self.hook_registry.hooks.write().await;
            if let Some(primary) = hooks.get_mut(&exec_hook_id) {
                if let Some(ref mut meta) = primary.tool_metadata {
                    meta.companion_hook_ids = Some(hook_ids[1..].to_vec());
                }
            }
        }

        // ── 7. Index in ToolRegistry for O(1) lookup ────────────────────────────
        self.tool_registry
            .register_tool(&tool_name, exec_hook_id)
            .await?;

        debug!(
            tool_name = %tool_name,
            extension_id = %extension_id,
            hook_count = hook_ids.len(),
            "Registered tool with unified registry"
        );

        Ok(ToolRegistration::new(
            tool_name,
            hook_ids,
            extension_id.clone(),
        ))
    }

    /// Get tool metadata by name (O(1) lookup)
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    ///
    /// # Returns
    /// The tool metadata if found, None otherwise
    pub async fn get_tool_metadata(&self, tool_name: &str) -> Option<ToolMetadata> {
        let hook_id = self.tool_registry.get_tool_hook_id(tool_name).await?;
        let hooks = self.hook_registry.hooks.read().await;
        hooks.get(&hook_id)?.tool_metadata.clone()
    }

    /// Get the hook ID for a tool by name
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    ///
    /// # Returns
    /// The hook ID if found, None otherwise
    pub async fn get_tool_hook_id(&self, tool_name: &str) -> Option<HookId> {
        self.tool_registry.get_tool_hook_id(tool_name).await
    }

    /// List all registered tools
    ///
    /// # Returns
    /// A list of tool metadata for all registered tools.
    /// Note: This returns ALL registered tools regardless of the agent's whitelist.
    /// The whitelist is enforced at execution time via `invoke_hook`.
    pub async fn list_tools(&self) -> Vec<ToolMetadata> {
        // Collect hook IDs from tool index first
        let hook_ids: Vec<HookId> = self
            .tool_registry
            .tool_index
            .read(|tool_index| tool_index.values().copied().collect())
            .await;

        // Then look up metadata in hooks registry
        let hooks = self.hook_registry.hooks.read().await;

        hook_ids
            .into_iter()
            .filter_map(|hook_id| hooks.get(&hook_id))
            .filter(|hook| hook.enabled)
            .filter_map(|hook| hook.tool_metadata.clone())
            .collect()
    }

    /// List all registered tools as `ToolDefinitions` (for LLM API)
    ///
    /// # Returns
    /// A list of `ToolDefinition` for all enabled tools
    pub async fn list_tool_definitions(&self) -> Vec<crate::providers::ToolDefinition> {
        self.list_tools()
            .await
            .into_iter()
            .map(|metadata| metadata.to_tool_definition())
            .collect()
    }

    /// Unregister a tool by name.
    ///
    /// Removes **all** hooks associated with the tool (execution, prompt, async,
    /// status, cancel) atomically.
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool to unregister
    #[instrument(skip(self), fields(tool_name = %tool_name))]
    pub async fn unregister_tool(&self, tool_name: &str) -> Result<()> {
        let hook_id = self.tool_registry.unregister_tool(tool_name).await?;

        if let Some(primary_hook_id) = hook_id {
            // Unregister companion hooks stored in the primary registration's metadata
            let companion_ids: Vec<HookId> = {
                let hooks = self.hook_registry.hooks.read().await;
                hooks
                    .get(&primary_hook_id)
                    .and_then(|h| h.tool_metadata.as_ref())
                    .and_then(|m| m.companion_hook_ids.clone())
                    .unwrap_or_default()
            };

            for companion_id in companion_ids {
                if let Err(e) = self.hook_registry.unregister_hook(&companion_id).await {
                    debug!(companion_id = %companion_id, error = %e, "Failed to unregister companion hook");
                }
            }

            // Unregister the primary execution hook
            self.hook_registry.unregister_hook(&primary_hook_id).await?;
            debug!(tool_name = %tool_name, "Unregistered tool and all companion hooks");
        } else {
            warn!(tool_name = %tool_name, "Attempted to unregister unknown tool");
        }

        Ok(())
    }

    /// Get the number of registered tools
    pub async fn tool_count(&self) -> usize {
        self.tool_registry.tool_count().await
    }
}

impl Default for ExtensionCore {
    fn default() -> Self {
        Self::new()
    }
}

/// Global instance of `ExtensionCore` (optional convenience)
use std::sync::OnceLock;
static GLOBAL_EXTENSION_CORE: OnceLock<Arc<ExtensionCore>> = OnceLock::new();

/// Initialize the global extension core
pub fn init_global_core(core: Arc<ExtensionCore>) {
    let _ = GLOBAL_EXTENSION_CORE.set(core);
}

/// Get the global extension core
pub fn global_core() -> Option<Arc<ExtensionCore>> {
    GLOBAL_EXTENSION_CORE.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::core::handler::HookHandler;
    use crate::extension::types::HookOutput;

    /// Mock handler for testing
    #[derive(Debug)]
    struct MockHandler {
        point: HookPoint,
        output: HookResult,
    }

    #[async_trait::async_trait]
    impl HookHandler for MockHandler {
        async fn handle(&self, _ctx: HookContext) -> HookResult {
            match &self.output {
                HookResult::Continue(output) => HookResult::Continue(output.clone()),
                HookResult::PassThrough => HookResult::PassThrough,
                HookResult::Handled => HookResult::Handled,
                HookResult::Replace(output) => HookResult::Replace(output.clone()),
                HookResult::Error(e) => HookResult::Error(anyhow::anyhow!(e.to_string())),
            }
        }

        fn hook_point(&self) -> HookPoint {
            self.point.clone()
        }
    }

    #[tokio::test]
    async fn test_register_and_unregister() {
        let core = ExtensionCore::new();

        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolRegister,
            output: HookResult::PassThrough,
        });

        let ext_id = ExtensionId::new("test");
        let reg = core
            .register_hook(HookPoint::ToolRegister, handler, &ext_id)
            .await
            .unwrap();

        assert_eq!(core.hook_count().await, 1);

        core.unregister_hook(&reg.id).await.unwrap();
        assert_eq!(core.hook_count().await, 0);
    }

    #[tokio::test]
    async fn test_enable_disable() {
        let core = ExtensionCore::new();

        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolRegister,
            output: HookResult::PassThrough,
        });

        let ext_id = ExtensionId::new("test");
        let reg = core
            .register_hook(HookPoint::ToolRegister, handler, &ext_id)
            .await
            .unwrap();

        // Initially enabled
        let hooks = core.get_hooks_for_point(&HookPoint::ToolRegister).await;
        assert_eq!(hooks.len(), 1);

        // Disable
        core.disable_hook(&reg.id).await.unwrap();
        let hooks = core.get_hooks_for_point(&HookPoint::ToolRegister).await;
        assert_eq!(hooks.len(), 0);

        // Enable
        core.enable_hook(&reg.id).await.unwrap();
        let hooks = core.get_hooks_for_point(&HookPoint::ToolRegister).await;
        assert_eq!(hooks.len(), 1);
    }

    #[tokio::test]
    async fn test_invoke_hook_passthrough() {
        let core = ExtensionCore::new();

        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolRegister,
            output: HookResult::PassThrough,
        });

        let ext_id = ExtensionId::new("test");
        core.register_hook(HookPoint::ToolRegister, handler, &ext_id)
            .await
            .unwrap();

        let result = core
            .invoke_hook(HookPoint::ToolRegister, HookInput::Unit)
            .await;

        match result {
            HookResult::PassThrough => (), // Expected
            _ => panic!("Expected PassThrough, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_invoke_hook_with_output() {
        let core = ExtensionCore::new();

        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolRegister,
            output: HookResult::Continue(HookOutput::text("test output")),
        });

        let ext_id = ExtensionId::new("test");
        core.register_hook(HookPoint::ToolRegister, handler, &ext_id)
            .await
            .unwrap();

        let result = core
            .invoke_hook(HookPoint::ToolRegister, HookInput::Unit)
            .await;

        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert_eq!(text, "test output");
            }
            _ => panic!("Expected Continue with text, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_priority_ordering() {
        let core = ExtensionCore::new();

        // Create handlers with different priorities
        // Note: MockHandler doesn't support custom priority, so we test ordering
        // by registration sequence (they get default priority 100)

        let handler1 = Arc::new(MockHandler {
            point: HookPoint::ToolRegister,
            output: HookResult::Continue(HookOutput::text("first")),
        });

        let handler2 = Arc::new(MockHandler {
            point: HookPoint::ToolRegister,
            output: HookResult::Continue(HookOutput::text("second")),
        });

        let ext_id = ExtensionId::new("test");
        core.register_hook(HookPoint::ToolRegister, handler1, &ext_id)
            .await
            .unwrap();
        core.register_hook(HookPoint::ToolRegister, handler2, &ext_id)
            .await
            .unwrap();

        let hooks = core.get_hooks_for_point(&HookPoint::ToolRegister).await;
        assert_eq!(hooks.len(), 2);
    }

    #[tokio::test]
    async fn test_globally_disabled() {
        let core = ExtensionCore::new();

        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolRegister,
            output: HookResult::Continue(HookOutput::text("should not see")),
        });

        let ext_id = ExtensionId::new("test");
        core.register_hook(HookPoint::ToolRegister, handler, &ext_id)
            .await
            .unwrap();

        // Disable globally
        core.set_globally_enabled(false).await;

        let result = core
            .invoke_hook(HookPoint::ToolRegister, HookInput::Unit)
            .await;

        match result {
            HookResult::PassThrough => (), // Expected when disabled
            _ => panic!("Expected PassThrough when globally disabled, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_invoke_hook_text() {
        let core = ExtensionCore::new();

        let handler = Arc::new(MockHandler {
            point: HookPoint::PromptSystemSection {
                section: "test".to_string(),
                priority: 100,
            },
            output: HookResult::Continue(HookOutput::text("prompt text")),
        });

        let ext_id = ExtensionId::new("test");
        core.register_hook(
            HookPoint::PromptSystemSection {
                section: "test".to_string(),
                priority: 100,
            },
            handler,
            &ext_id,
        )
        .await
        .unwrap();

        let text = core
            .invoke_hook_text(
                HookPoint::PromptSystemSection {
                    section: "test".to_string(),
                    priority: 100,
                },
                HookInput::Unit,
            )
            .await;

        assert_eq!(text, Some("prompt text".to_string()));
    }

    // ==================== ADR-019: Tool Permission Check Tests ====================

    #[tokio::test]
    async fn test_tool_execute_blocked_when_disabled() {
        let core = ExtensionCore::new();

        // Create a tool execution handler
        let _handler = Arc::new(MockHandler {
            point: HookPoint::ToolExecute {
                tool_name: "test_tool".to_string(),
            },
            output: HookResult::Continue(HookOutput::text("executed")),
        });

        // Register the tool with empty whitelist (all tools disabled)
        let tool_config = crate::types::agent::ToolConfig {
            enabled: vec![], // Empty whitelist = all tools disabled
            ..Default::default()
        };
        core.set_tool_config(tool_config).await;

        // Try to execute the tool - should be blocked
        let result = core
            .invoke_hook(
                HookPoint::ToolExecute {
                    tool_name: "test_tool".to_string(),
                },
                HookInput::ToolCall {
                    tool_name: "test_tool".to_string(),
                    params: serde_json::json!({}),
                    workspace: None,
                    agent_id: None,
                    session_id: None,
                },
            )
            .await;

        match result {
            HookResult::Error(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("disabled"),
                    "Error should mention tool is disabled: {msg}"
                );
            }
            _ => panic!("Expected Error when tool is disabled, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_tool_execute_permitted_when_enabled() {
        let core = ExtensionCore::new();

        // Create and register a tool execution handler
        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolExecute {
                tool_name: "enabled_tool".to_string(),
            },
            output: HookResult::Continue(HookOutput::Json(
                serde_json::json!({"result": "success"}),
            )),
        });

        let ext_id = ExtensionId::new("test");
        core.register_hook(
            HookPoint::ToolExecute {
                tool_name: "enabled_tool".to_string(),
            },
            handler,
            &ext_id,
        )
        .await
        .unwrap();

        // Configure whitelist to enable the tool
        let tool_config = crate::types::agent::ToolConfig {
            enabled: vec!["enabled_tool".to_string()],
            ..Default::default()
        };
        core.set_tool_config(tool_config).await;

        // Execute the tool - should succeed
        let result = core
            .invoke_hook(
                HookPoint::ToolExecute {
                    tool_name: "enabled_tool".to_string(),
                },
                HookInput::ToolCall {
                    tool_name: "enabled_tool".to_string(),
                    params: serde_json::json!({}),
                    workspace: None,
                    agent_id: None,
                    session_id: None,
                },
            )
            .await;

        match result {
            HookResult::Continue(HookOutput::Json(json)) => {
                assert_eq!(json["result"], "success");
            }
            _ => panic!("Expected Continue with JSON result, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn test_non_tool_hooks_not_affected_by_tool_config() {
        let core = ExtensionCore::new();

        // Create a non-tool handler
        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolRegister,
            output: HookResult::Continue(HookOutput::text("registration info")),
        });

        let ext_id = ExtensionId::new("test");
        core.register_hook(HookPoint::ToolRegister, handler, &ext_id)
            .await
            .unwrap();

        // Set empty whitelist (would block tools, but shouldn't affect ToolRegister)
        let tool_config = crate::types::agent::ToolConfig {
            enabled: vec![],
            ..Default::default()
        };
        core.set_tool_config(tool_config).await;

        // ToolRegister hook should work normally
        let result = core
            .invoke_hook(HookPoint::ToolRegister, HookInput::Unit)
            .await;

        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert_eq!(text, "registration info");
            }
            _ => panic!("Expected Continue with text, got {result:?}"),
        }
    }
}
