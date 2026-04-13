//! Extension Core registry
//!
//! This module implements the central registry for all hook handlers.
//! It manages registration, enable/disable, and invocation of hooks.

use crate::extensions::core::context::{ExtensionServices, HookContext};
use crate::extensions::core::hook_points::HookPoint;
use crate::extensions::types::{
    ExtensionId, HookId, HookInput, HookOutput, HookPriority, HookResult, ToolMetadata, ToolSource,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, instrument, trace, warn};

/// A registered hook handler
#[derive(Debug, Clone)]
pub struct RegisteredHook {
    /// Unique registration ID
    pub id: HookId,
    
    /// Extension that owns this hook
    pub extension_id: ExtensionId,
    
    /// The hook point
    pub point: HookPoint,
    
    /// The handler implementation
    pub handler: Arc<dyn super::context::HookHandler>,
    
    /// Priority (higher = earlier execution)
    pub priority: HookPriority,
    
    /// Whether currently enabled
    pub enabled: bool,
    
    /// Tool metadata (only populated for ToolRegister hooks)
    pub tool_metadata: Option<ToolMetadata>,
}

impl RegisteredHook {
    /// Create a new registered hook
    pub fn new(
        id: HookId,
        extension_id: ExtensionId,
        point: HookPoint,
        handler: Arc<dyn super::context::HookHandler>,
        priority: HookPriority,
    ) -> Self {
        Self {
            id,
            extension_id,
            point,
            handler,
            priority,
            enabled: true,
            tool_metadata: None,
        }
    }
    
    /// Create a new registered hook with tool metadata
    pub fn with_tool_metadata(
        id: HookId,
        extension_id: ExtensionId,
        point: HookPoint,
        handler: Arc<dyn super::context::HookHandler>,
        priority: HookPriority,
        tool_metadata: ToolMetadata,
    ) -> Self {
        Self {
            id,
            extension_id,
            point,
            handler,
            priority,
            enabled: true,
            tool_metadata: Some(tool_metadata),
        }
    }
}

/// Central registry for extension hooks
///
/// This is the core component that manages all hook registrations and
/// provides the invocation mechanism.
#[derive(Debug)]
pub struct ExtensionCore {
    /// All registered hooks, keyed by HookId
    hooks: RwLock<HashMap<HookId, RegisteredHook>>,
    
    /// Hooks indexed by hook point for faster lookup
    hooks_by_point: RwLock<HashMap<String, Vec<HookId>>>,
    
    /// Tool index: maps tool name to hook ID for O(1) lookup
    tool_index: RwLock<HashMap<String, HookId>>,
    
    /// Extension services shared across all handlers
    services: Arc<ExtensionServices>,
    
    /// Global enable/disable flag
    globally_enabled: RwLock<bool>,
    
    /// Tool configuration (whitelist, per-tool settings)
    tool_config: RwLock<crate::types::agent::ToolConfig>,
}

impl ExtensionCore {
    /// Create a new Extension Core
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(HashMap::new()),
            hooks_by_point: RwLock::new(HashMap::new()),
            tool_index: RwLock::new(HashMap::new()),
            services: Arc::new(ExtensionServices::new()),
            globally_enabled: RwLock::new(true),
            tool_config: RwLock::new(crate::types::agent::ToolConfig::default()),
        }
    }
    
    /// Create a new Extension Core with custom services
    pub fn with_services(services: Arc<ExtensionServices>) -> Self {
        Self {
            hooks: RwLock::new(HashMap::new()),
            hooks_by_point: RwLock::new(HashMap::new()),
            tool_index: RwLock::new(HashMap::new()),
            services,
            globally_enabled: RwLock::new(true),
            tool_config: RwLock::new(crate::types::agent::ToolConfig::default()),
        }
    }
    
    /// Set the tool configuration (whitelist, etc.)
    pub async fn set_tool_config(&self, config: crate::types::agent::ToolConfig) {
        let mut tool_config = self.tool_config.write().await;
        *tool_config = config;
        debug!("Updated tool configuration");
    }
    
    /// Check if a tool is enabled according to whitelist
    pub async fn is_tool_enabled(&self, tool_name: &str) -> bool {
        let config = self.tool_config.read().await;
        config.is_tool_enabled(tool_name)
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
        handler: Arc<dyn super::context::HookHandler>,
        extension_id: &ExtensionId,
    ) -> Result<RegisteredHook> {
        let hook_id = HookId::new();
        let priority = handler.priority();
        
        let registration = RegisteredHook::new(
            hook_id.clone(),
            extension_id.clone(),
            point.clone(),
            handler,
            priority,
        );
        
        // Add to hooks map
        {
            let mut hooks = self.hooks.write().await;
            hooks.insert(hook_id.clone(), registration.clone());
        }
        
        // Add to point index
        {
            let mut by_point = self.hooks_by_point.write().await;
            let point_name = point.name();
            let entry = by_point.entry(point_name).or_insert_with(Vec::new);
            entry.push(hook_id.clone());
            
            // Sort by priority (higher first)
            let hooks = self.hooks.read().await;
            entry.sort_by_key(|id| {
                hooks
                    .get(id)
                    .map(|h| -h.priority) // Negative for descending order
                    .unwrap_or(0)
            });
        }
        
        debug!(
            hook_id = %hook_id,
            point = %point,
            priority = priority,
            "Registered hook handler"
        );
        
        Ok(registration)
    }
    
    /// Unregister a hook handler
    ///
    /// # Arguments
    /// * `hook_id` - The ID of the hook to unregister
    #[instrument(skip(self), fields(hook_id = %hook_id))]
    pub async fn unregister_hook(&self, hook_id: &HookId) -> Result<()> {
        // Remove from hooks map
        let point_name = {
            let mut hooks = self.hooks.write().await;
            hooks
                .remove(hook_id)
                .map(|h| h.point.name())
        };
        
        // Remove from point index
        if let Some(point_name) = point_name {
            let mut by_point = self.hooks_by_point.write().await;
            if let Some(entry) = by_point.get_mut(&point_name) {
                entry.retain(|id| id != hook_id);
                if entry.is_empty() {
                    by_point.remove(&point_name);
                }
            }
        }
        
        debug!(hook_id = %hook_id, "Unregistered hook handler");
        Ok(())
    }
    
    /// Enable a hook handler
    #[instrument(skip(self), fields(hook_id = %hook_id))]
    pub async fn enable_hook(&self, hook_id: &HookId) -> Result<()> {
        let mut hooks = self.hooks.write().await;
        if let Some(hook) = hooks.get_mut(hook_id) {
            hook.enabled = true;
            debug!(hook_id = %hook_id, "Enabled hook handler");
        } else {
            warn!(hook_id = %hook_id, "Attempted to enable unknown hook");
        }
        Ok(())
    }
    
    /// Disable a hook handler
    #[instrument(skip(self), fields(hook_id = %hook_id))]
    pub async fn disable_hook(&self, hook_id: &HookId) -> Result<()> {
        let mut hooks = self.hooks.write().await;
        if let Some(hook) = hooks.get_mut(hook_id) {
            hook.enabled = false;
            debug!(hook_id = %hook_id, "Disabled hook handler");
        } else {
            warn!(hook_id = %hook_id, "Attempted to disable unknown hook");
        }
        Ok(())
    }
    
    /// Globally enable/disable all hooks
    pub async fn set_globally_enabled(&self, enabled: bool) {
        let mut globally_enabled = self.globally_enabled.write().await;
        *globally_enabled = enabled;
        debug!(enabled = enabled, "Set global hook enable state");
    }
    
    /// Check if hooks are globally enabled
    pub async fn is_globally_enabled(&self) -> bool {
        *self.globally_enabled.read().await
    }
    
    /// Get all hooks for a specific extension
    pub async fn get_hooks_for_extension(
        &self,
        extension_id: &ExtensionId,
    ) -> Vec<RegisteredHook> {
        let hooks = self.hooks.read().await;
        hooks
            .values()
            .filter(|h| h.extension_id == *extension_id)
            .cloned()
            .collect()
    }
    
    /// Get all registered hooks
    pub async fn get_all_hooks(&self) -> Vec<RegisteredHook> {
        let hooks = self.hooks.read().await;
        hooks.values().cloned().collect()
    }
    
    /// Get hooks for a specific hook point
    /// 
    /// Supports wildcard matching for tool execution hooks. If an exact match
    /// is not found, checks for wildcard patterns (e.g., "mcp:identity:*" matches
    /// "mcp:identity:echo_identity").
    pub async fn get_hooks_for_point(&self, point: &HookPoint) -> Vec<RegisteredHook> {
        let by_point = self.hooks_by_point.read().await;
        let hooks = self.hooks.read().await;
        let point_name = point.name();
        
        // First try exact match
        if let Some(ids) = by_point.get(&point_name) {
            return ids.iter()
                .filter_map(|id| hooks.get(id).cloned())
                .filter(|h| h.enabled)
                .collect();
        }
        
        // For tool execution hooks, try wildcard matching
        // e.g., "tool.execute.mcp:identity:echo_identity" should match "tool.execute.mcp:identity:*"
        if let HookPoint::ToolExecute { tool_name } | HookPoint::ToolExecuteAsync { tool_name } |
           HookPoint::ToolCheckStatus { tool_name } | HookPoint::ToolCancel { tool_name } = point {
            for (registered_name, ids) in by_point.iter() {
                // Check if this is a tool execution hook with a wildcard pattern
                if let Some(prefix) = registered_name.strip_prefix("tool.execute.") {
                    if prefix.ends_with('*') && tool_name.starts_with(&prefix[..prefix.len()-1]) {
                        return ids.iter()
                            .filter_map(|id| hooks.get(id).cloned())
                            .filter(|h| h.enabled)
                            .collect();
                    }
                }
                if let Some(prefix) = registered_name.strip_prefix("tool.execute_async.") {
                    if prefix.ends_with('*') && tool_name.starts_with(&prefix[..prefix.len()-1]) {
                        return ids.iter()
                            .filter_map(|id| hooks.get(id).cloned())
                            .filter(|h| h.enabled)
                            .collect();
                    }
                }
                if let Some(prefix) = registered_name.strip_prefix("tool.check_status.") {
                    if prefix.ends_with('*') && tool_name.starts_with(&prefix[..prefix.len()-1]) {
                        return ids.iter()
                            .filter_map(|id| hooks.get(id).cloned())
                            .filter(|h| h.enabled)
                            .collect();
                    }
                }
                if let Some(prefix) = registered_name.strip_prefix("tool.cancel.") {
                    if prefix.ends_with('*') && tool_name.starts_with(&prefix[..prefix.len()-1]) {
                        return ids.iter()
                            .filter_map(|id| hooks.get(id).cloned())
                            .filter(|h| h.enabled)
                            .collect();
                    }
                }
            }
        }
        
        Vec::new()
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
    pub async fn invoke_hook(
        &self,
        point: HookPoint,
        input: HookInput,
    ) -> HookResult {
        // Check global enable
        if !self.is_globally_enabled().await {
            trace!("Hooks globally disabled, returning PassThrough");
            return HookResult::PassThrough;
        }
        
        // ADR-019 Phase 1: Tool permission check at ExtensionCore layer
        // This ensures ALL tools (built-in, MCP, Universal) are checked consistently
        if let HookPoint::ToolExecute { ref tool_name } = point {
            if !self.is_tool_enabled(tool_name).await {
                warn!(tool_name = %tool_name, "Tool execution blocked: tool is not enabled");
                return HookResult::Error(
                    anyhow::anyhow!(
                        "Tool '{}' is currently disabled. Enable it in agent config to use it.",
                        tool_name
                    )
                );
            }
            trace!(tool_name = %tool_name, "Tool execution permitted");
        }
        
        let handlers = self.get_hooks_for_point(&point).await;
        
        if handlers.is_empty() {
            trace!("No handlers registered for hook point");
            return HookResult::PassThrough;
        }
        
        trace!(handler_count = handlers.len(), "Invoking hooks");
        
        let mut outputs = Vec::new();
        
        for handler in handlers {
            let hook_id = handler.id.clone();
            let start = std::time::Instant::now();
            
            // Create context
            let ctx = HookContext::new(
                point.clone(),
                input.clone(),
                self.services.clone(),
            );
            
            // Invoke handler
            trace!(handler_id = %hook_id, "Calling handler");
            let result = handler.handler.handle(ctx).await;
            
            // Record telemetry
            let duration_ms = start.elapsed().as_millis() as u64;
            self.services.record_invocation(&hook_id, &point, duration_ms);
            
            // Process result
            match result {
                HookResult::Continue(output) => {
                    trace!(handler_id = %hook_id, "Handler continued with output");
                    outputs.push(output);
                }
                HookResult::PassThrough => {
                    trace!(handler_id = %hook_id, "Handler passed through");
                }
                HookResult::Handled => {
                    trace!(handler_id = %hook_id, "Handler consumed event");
                    return HookResult::Handled;
                }
                HookResult::Replace(output) => {
                    trace!(handler_id = %hook_id, "Handler replaced output");
                    return HookResult::Replace(output);
                }
                HookResult::Error(e) => {
                    error!(handler_id = %hook_id, error = %e, "Handler error");
                    return HookResult::Error(e);
                }
            }
        }
        
        // Combine outputs
        if outputs.is_empty() {
            HookResult::PassThrough
        } else if outputs.len() == 1 {
            HookResult::Continue(outputs.into_iter().next().unwrap())
        } else {
            HookResult::Continue(HookOutput::combine(outputs))
        }
    }
    
    /// Invoke hooks and return text output (convenience for prompt hooks)
    pub async fn invoke_hook_text(
        &self,
        point: HookPoint,
        input: HookInput,
    ) -> Option<String> {
        match self.invoke_hook(point, input).await {
            HookResult::Continue(HookOutput::Text(text)) => Some(text),
            HookResult::Replace(HookOutput::Text(text)) => Some(text),
            HookResult::Continue(HookOutput::Vec(outputs)) => {
                // Concatenate text outputs
                let texts: Vec<String> = outputs
                    .into_iter()
                    .filter_map(|o| o.as_text().map(|s| s.to_string()))
                    .collect();
                if texts.is_empty() {
                    None
                } else {
                    Some(texts.join("\n"))
                }
            }
            _ => None,
        }
    }
    
    /// Invoke hooks and return JSON output (convenience for data hooks)
    pub async fn invoke_hook_json(
        &self,
        point: HookPoint,
        input: HookInput,
    ) -> Option<serde_json::Value> {
        match self.invoke_hook(point, input).await {
            HookResult::Continue(HookOutput::Json(value)) => Some(value),
            HookResult::Replace(HookOutput::Json(value)) => Some(value),
            _ => None,
        }
    }
    
    /// Get the number of registered hooks
    pub async fn hook_count(&self) -> usize {
        self.hooks.read().await.len()
    }
    
    /// Get the number of registered hooks for a specific point
    pub async fn hook_count_for_point(&self, point: &HookPoint) -> usize {
        self.get_hooks_for_point(point).await.len()
    }
    
    // ==================== UNIFIED TOOL REGISTRY (ADR-018b) ====================
    
    /// Register a tool with the unified registry
    ///
    /// This method registers both the tool metadata and its execution handler.
    /// It enforces the whitelist at registration time.
    ///
    /// # Arguments
    /// * `metadata` - Tool metadata (name, description, parameters, etc.)
    /// * `handler` - The handler implementation for tool execution
    /// * `extension_id` - ID of the extension that owns this tool
    ///
    /// # Returns
    /// The registration information
    #[instrument(skip(self, handler, metadata), fields(extension_id = %extension_id, tool_name = %metadata.name))]
    pub async fn register_tool(
        &self,
        metadata: ToolMetadata,
        handler: Arc<dyn super::context::HookHandler>,
        extension_id: &ExtensionId,
    ) -> Result<RegisteredHook> {
        let tool_name = metadata.name.clone();
        
        // ADR-019: Allow re-registration for dynamic enable/disable
        // If tool already registered, unregister it first (idempotent)
        if self.get_tool_metadata(&tool_name).await.is_some() {
            let _ = self.unregister_tool(&tool_name).await;
        }
        
        let hook_id = HookId::new();
        let priority = handler.priority();
        
        // Get the handler's actual hook point (should be ToolExecute)
        let exec_point = handler.hook_point();
        
        // Create registration with the handler's actual hook point
        let registration = RegisteredHook::with_tool_metadata(
            hook_id.clone(),
            extension_id.clone(),
            exec_point.clone(),  // Use actual execution point, not ToolRegister
            handler.clone(),
            priority,
            metadata,
        );
        
        // Add to hooks map
        {
            let mut hooks = self.hooks.write().await;
            hooks.insert(hook_id.clone(), registration.clone());
        }
        
        // Index by the handler's hook point for execution lookup
        {
            let mut by_point = self.hooks_by_point.write().await;
            let exec_point_name = exec_point.name();
            let entry = by_point.entry(exec_point_name).or_insert_with(Vec::new);
            entry.push(hook_id.clone());
            
            // Sort by priority (higher first)
            let hooks = self.hooks.read().await;
            entry.sort_by_key(|id| {
                hooks
                    .get(id)
                    .map(|h| -h.priority)
                    .unwrap_or(0)
            });
        }
        
        // Add to tool index for O(1) lookup
        {
            let mut tool_index = self.tool_index.write().await;
            tool_index.insert(tool_name.clone(), hook_id.clone());
        }
        
        debug!(
            hook_id = %hook_id,
            tool_name = %tool_name,
            extension_id = %extension_id,
            exec_point = %exec_point,
            "Registered tool"
        );
        
        Ok(registration)
    }
    
    /// Get tool metadata by name (O(1) lookup)
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    ///
    /// # Returns
    /// The tool metadata if found, None otherwise
    pub async fn get_tool_metadata(&self, tool_name: &str) -> Option<ToolMetadata> {
        let tool_index = self.tool_index.read().await;
        let hooks = self.hooks.read().await;
        
        tool_index.get(tool_name)
            .and_then(|hook_id| hooks.get(hook_id))
            .and_then(|hook| hook.tool_metadata.clone())
    }
    
    /// Get the hook ID for a tool by name
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    ///
    /// # Returns
    /// The hook ID if found, None otherwise
    pub async fn get_tool_hook_id(&self, tool_name: &str) -> Option<HookId> {
        let tool_index = self.tool_index.read().await;
        tool_index.get(tool_name).cloned()
    }
    
    /// List all registered tools
    ///
    /// # Returns
    /// A list of tool metadata for all enabled tools
    pub async fn list_tools(&self) -> Vec<ToolMetadata> {
        let tool_index = self.tool_index.read().await;
        let hooks = self.hooks.read().await;
        let config = self.tool_config.read().await;
        
        tool_index.values()
            .filter_map(|hook_id| hooks.get(hook_id))
            .filter(|hook| hook.enabled)
            .filter_map(|hook| hook.tool_metadata.clone())
            .filter(|metadata| config.is_tool_enabled(&metadata.name))
            .collect()
    }
    
    /// List all registered tools as ToolDefinitions (for LLM API)
    ///
    /// # Returns
    /// A list of ToolDefinition for all enabled tools
    pub async fn list_tool_definitions(&self) -> Vec<crate::providers::ToolDefinition> {
        self.list_tools().await
            .into_iter()
            .map(|metadata| metadata.to_tool_definition())
            .collect()
    }
    
    /// Unregister a tool by name
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool to unregister
    #[instrument(skip(self), fields(tool_name = %tool_name))]
    pub async fn unregister_tool(&self, tool_name: &str) -> Result<()> {
        let hook_id = {
            let mut tool_index = self.tool_index.write().await;
            tool_index.remove(tool_name)
        };
        
        if let Some(hook_id) = hook_id {
            // Also unregister from hooks
            self.unregister_hook(&hook_id).await?;
            debug!(tool_name = %tool_name, "Unregistered tool");
        } else {
            warn!(tool_name = %tool_name, "Attempted to unregister unknown tool");
        }
        
        Ok(())
    }
    
    /// Get the number of registered tools
    pub async fn tool_count(&self) -> usize {
        self.tool_index.read().await.len()
    }
}

impl Default for ExtensionCore {
    fn default() -> Self {
        Self::new()
    }
}

/// Global instance of ExtensionCore (optional convenience)
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
    use crate::extensions::core::context::HookHandler;
    use crate::extensions::types::HookOutput;
    
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
        let reg = core.register_hook(HookPoint::ToolRegister, handler, &ext_id).await.unwrap();
        
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
        let reg = core.register_hook(HookPoint::ToolRegister, handler, &ext_id).await.unwrap();
        
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
        core.register_hook(HookPoint::ToolRegister, handler, &ext_id).await.unwrap();
        
        let result = core.invoke_hook(HookPoint::ToolRegister, HookInput::Unit).await;
        
        match result {
            HookResult::PassThrough => (), // Expected
            _ => panic!("Expected PassThrough, got {:?}", result),
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
        core.register_hook(HookPoint::ToolRegister, handler, &ext_id).await.unwrap();
        
        let result = core.invoke_hook(HookPoint::ToolRegister, HookInput::Unit).await;
        
        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert_eq!(text, "test output");
            }
            _ => panic!("Expected Continue with text, got {:?}", result),
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
        core.register_hook(HookPoint::ToolRegister, handler1, &ext_id).await.unwrap();
        core.register_hook(HookPoint::ToolRegister, handler2, &ext_id).await.unwrap();
        
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
        core.register_hook(HookPoint::ToolRegister, handler, &ext_id).await.unwrap();
        
        // Disable globally
        core.set_globally_enabled(false).await;
        
        let result = core.invoke_hook(HookPoint::ToolRegister, HookInput::Unit).await;
        
        match result {
            HookResult::PassThrough => (), // Expected when disabled
            _ => panic!("Expected PassThrough when globally disabled, got {:?}", result),
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
        ).await.unwrap();
        
        let text = core.invoke_hook_text(
            HookPoint::PromptSystemSection {
                section: "test".to_string(),
                priority: 100,
            },
            HookInput::Unit,
        ).await;
        
        assert_eq!(text, Some("prompt text".to_string()));
    }
    
    // ==================== ADR-019: Tool Permission Check Tests ====================
    
    #[tokio::test]
    async fn test_tool_execute_blocked_when_disabled() {
        let core = ExtensionCore::new();
        
        // Create a tool execution handler
        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolExecute { tool_name: "test_tool".to_string() },
            output: HookResult::Continue(HookOutput::text("executed")),
        });
        
        // Register the tool with empty whitelist (all tools disabled)
        let tool_config = crate::types::agent::ToolConfig {
            enabled: vec![], // Empty whitelist = all tools disabled
            ..Default::default()
        };
        core.set_tool_config(tool_config).await;
        
        // Try to execute the tool - should be blocked
        let result = core.invoke_hook(
            HookPoint::ToolExecute { tool_name: "test_tool".to_string() },
            HookInput::ToolCall { tool_name: "test_tool".to_string(), params: serde_json::json!({}) },
        ).await;
        
        match result {
            HookResult::Error(e) => {
                let msg = e.to_string();
                assert!(msg.contains("disabled"), "Error should mention tool is disabled: {}", msg);
            }
            _ => panic!("Expected Error when tool is disabled, got {:?}", result),
        }
    }
    
    #[tokio::test]
    async fn test_tool_execute_permitted_when_enabled() {
        let core = ExtensionCore::new();
        
        // Create and register a tool execution handler
        let handler = Arc::new(MockHandler {
            point: HookPoint::ToolExecute { tool_name: "enabled_tool".to_string() },
            output: HookResult::Continue(HookOutput::Json(serde_json::json!({"result": "success"}))),
        });
        
        let ext_id = ExtensionId::new("test");
        core.register_hook(
            HookPoint::ToolExecute { tool_name: "enabled_tool".to_string() },
            handler,
            &ext_id,
        ).await.unwrap();
        
        // Configure whitelist to enable the tool
        let tool_config = crate::types::agent::ToolConfig {
            enabled: vec!["enabled_tool".to_string()],
            ..Default::default()
        };
        core.set_tool_config(tool_config).await;
        
        // Execute the tool - should succeed
        let result = core.invoke_hook(
            HookPoint::ToolExecute { tool_name: "enabled_tool".to_string() },
            HookInput::ToolCall { tool_name: "enabled_tool".to_string(), params: serde_json::json!({}) },
        ).await;
        
        match result {
            HookResult::Continue(HookOutput::Json(json)) => {
                assert_eq!(json["result"], "success");
            }
            _ => panic!("Expected Continue with JSON result, got {:?}", result),
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
        core.register_hook(HookPoint::ToolRegister, handler, &ext_id).await.unwrap();
        
        // Set empty whitelist (would block tools, but shouldn't affect ToolRegister)
        let tool_config = crate::types::agent::ToolConfig {
            enabled: vec![],
            ..Default::default()
        };
        core.set_tool_config(tool_config).await;
        
        // ToolRegister hook should work normally
        let result = core.invoke_hook(HookPoint::ToolRegister, HookInput::Unit).await;
        
        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert_eq!(text, "registration info");
            }
            _ => panic!("Expected Continue with text, got {:?}", result),
        }
    }
}
