//! Hook Registry
//!
//! This module implements the registry for hook handlers.
//! It manages registration, enable/disable, and invocation of hooks.

use crate::extensions::framework::core::config::ExtensionServices;
use crate::extensions::framework::core::context::HookContext;
use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::types::{
    ExtensionId, HookId, HookInput, HookOutput, HookPriority, HookResult, ToolMetadata,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, instrument, trace, warn};

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
    pub handler: Arc<dyn super::handler::HookHandler>,

    /// Priority (higher = earlier execution)
    pub priority: HookPriority,

    /// Whether currently enabled
    pub enabled: bool,

    /// Tool metadata (only populated for `ToolRegister` hooks)
    pub tool_metadata: Option<ToolMetadata>,
}

impl RegisteredHook {
    /// Create a new registered hook
    pub fn new(
        id: HookId,
        extension_id: ExtensionId,
        point: HookPoint,
        handler: Arc<dyn super::handler::HookHandler>,
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
        handler: Arc<dyn super::handler::HookHandler>,
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

/// Information about a built-in extension
///
/// Built-in extensions are compiled into the binary and register hooks
/// with `HookRegistry` under IDs like `builtin:tool:Bash`.
#[derive(Debug, Clone)]
pub struct BuiltinExtensionInfo {
    /// Full extension ID (e.g., "builtin:tool:Bash")
    pub id: String,
    /// Extension type (e.g., "tool", "gateway")
    pub ext_type: String,
    /// Short name (e.g., "Bash")
    pub name: String,
    /// Whether any hook for this extension is enabled
    pub enabled: bool,
    /// Which hook points are registered
    pub capabilities: Vec<String>,
}

/// Registry for hook handlers
///
/// This component manages all hook registrations and provides the invocation mechanism.
#[derive(Debug)]
pub struct HookRegistry {
    /// All registered hooks, keyed by `HookId`
    pub(crate) hooks: RwLock<HashMap<HookId, RegisteredHook>>,

    /// Hooks indexed by hook point for faster lookup
    pub(crate) hooks_by_point: RwLock<HashMap<String, Vec<HookId>>>,

    /// Extension services shared across all handlers
    services: Arc<ExtensionServices>,

    /// Global enable/disable flag
    globally_enabled: RwLock<bool>,
}

impl HookRegistry {
    /// Create a new Hook Registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(HashMap::new()),
            hooks_by_point: RwLock::new(HashMap::new()),
            services: Arc::new(ExtensionServices::new()),
            globally_enabled: RwLock::new(true),
        }
    }

    /// Create a new Hook Registry with custom services
    pub fn with_services(services: Arc<ExtensionServices>) -> Self {
        Self {
            hooks: RwLock::new(HashMap::new()),
            hooks_by_point: RwLock::new(HashMap::new()),
            services,
            globally_enabled: RwLock::new(true),
        }
    }

    /// Get the services
    pub fn services(&self) -> Arc<ExtensionServices> {
        self.services.clone()
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
        let hook_id = HookId::new();
        let priority = handler.priority();

        let registration = RegisteredHook::new(
            hook_id,
            extension_id.clone(),
            point.clone(),
            handler,
            priority,
        );

        // Add to hooks map
        {
            let mut hooks = self.hooks.write().await;
            hooks.insert(hook_id, registration.clone());
        }

        // Add to point index
        {
            let mut by_point = self.hooks_by_point.write().await;
            let point_name = point.name();
            let entry = by_point.entry(point_name).or_insert_with(Vec::new);
            entry.push(hook_id);

            // Sort by priority (higher first)
            let hooks = self.hooks.read().await;
            entry.sort_by_key(|id| hooks.get(id).map_or(0, |h| -h.priority));
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
            hooks.remove(hook_id).map(|h| h.point.name())
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
    pub async fn get_hooks_for_extension(&self, extension_id: &ExtensionId) -> Vec<RegisteredHook> {
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

    /// List all built-in extensions registered with this registry
    ///
    /// Built-in extensions have IDs in the form `builtin:{type}:{name}`.
    /// This is type-agnostic — it will return tools, gateways, skills, or any
    /// future built-in extension type that registers hooks.
    pub async fn list_builtin_extensions(&self) -> Vec<BuiltinExtensionInfo> {
        let hooks = self.hooks.read().await;
        let mut groups: HashMap<String, Vec<&RegisteredHook>> = HashMap::new();

        for hook in hooks.values() {
            let ext_id = &hook.extension_id.0;
            if ext_id.starts_with("builtin:") {
                groups.entry(ext_id.clone()).or_default().push(hook);
            }
        }

        let mut results = Vec::new();
        for (ext_id, hooks) in groups {
            // Parse builtin:{type}:{name}
            let parts: Vec<&str> = ext_id.splitn(3, ':').collect();
            if parts.len() == 3 {
                let ext_type = parts[1].to_string();
                let name = parts[2].to_string();
                let enabled = hooks.iter().any(|h| h.enabled);
                let mut capabilities: Vec<String> = hooks.iter().map(|h| h.point.name()).collect();
                capabilities.sort_unstable();
                capabilities.dedup();

                results.push(BuiltinExtensionInfo {
                    id: ext_id,
                    ext_type,
                    name,
                    enabled,
                    capabilities,
                });
            }
        }

        results.sort_by(|a, b| a.ext_type.cmp(&b.ext_type).then(a.name.cmp(&b.name)));
        results
    }

    /// Get hooks for a specific hook point
    ///
    /// Supports wildcard matching for tool execution hooks. If an exact match
    /// is not found, checks for wildcard patterns (e.g., "mcp:identity:*" matches
    /// "`mcp:identity:echo_identity`").
    pub async fn get_hooks_for_point(&self, point: &HookPoint) -> Vec<RegisteredHook> {
        let by_point = self.hooks_by_point.read().await;
        let hooks = self.hooks.read().await;
        let point_name = point.name();

        // First try exact match
        if let Some(ids) = by_point.get(&point_name) {
            return ids
                .iter()
                .filter_map(|id| hooks.get(id).cloned())
                .filter(|h| h.enabled)
                .collect();
        }

        // For tool execution hooks, try wildcard matching
        // e.g., "tool.execute.mcp:identity:echo_identity" should match "tool.execute.mcp:identity:*"
        if let HookPoint::ToolExecute { tool_name }
        | HookPoint::ToolExecuteAsync { tool_name }
        | HookPoint::ToolCheckStatus { tool_name }
        | HookPoint::ToolCancel { tool_name } = point
        {
            for (registered_name, ids) in by_point.iter() {
                // Check if this is a tool execution hook with a wildcard pattern
                if let Some(prefix) = registered_name.strip_prefix("tool.execute.") {
                    if prefix.ends_with('*') && tool_name.starts_with(&prefix[..prefix.len() - 1]) {
                        return ids
                            .iter()
                            .filter_map(|id| hooks.get(id).cloned())
                            .filter(|h| h.enabled)
                            .collect();
                    }
                }
                if let Some(prefix) = registered_name.strip_prefix("tool.execute_async.") {
                    if prefix.ends_with('*') && tool_name.starts_with(&prefix[..prefix.len() - 1]) {
                        return ids
                            .iter()
                            .filter_map(|id| hooks.get(id).cloned())
                            .filter(|h| h.enabled)
                            .collect();
                    }
                }
                if let Some(prefix) = registered_name.strip_prefix("tool.check_status.") {
                    if prefix.ends_with('*') && tool_name.starts_with(&prefix[..prefix.len() - 1]) {
                        return ids
                            .iter()
                            .filter_map(|id| hooks.get(id).cloned())
                            .filter(|h| h.enabled)
                            .collect();
                    }
                }
                if let Some(prefix) = registered_name.strip_prefix("tool.cancel.") {
                    if prefix.ends_with('*') && tool_name.starts_with(&prefix[..prefix.len() - 1]) {
                        return ids
                            .iter()
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
    pub async fn invoke_hook(&self, point: HookPoint, input: HookInput) -> HookResult {
        // Check global enable
        if !self.is_globally_enabled().await {
            trace!("Hooks globally disabled, returning PassThrough");
            return HookResult::PassThrough;
        }

        let handlers = self.get_hooks_for_point(&point).await;

        if handlers.is_empty() {
            trace!("No handlers registered for hook point");
            return HookResult::PassThrough;
        }

        trace!(handler_count = handlers.len(), "Invoking hooks");

        let mut outputs = Vec::new();

        for handler in handlers {
            let hook_id = handler.id;
            let start = std::time::Instant::now();

            // Create context
            let mut ctx = HookContext::new(point.clone(), input.clone(), self.services.clone());

            // For tool calls, inject runtime context into state for reserved parameter resolution
            if let HookInput::ToolCall {
                ref agent_id,
                ref session_id,
                ref workspace,
                ..
            } = input
            {
                let tool_ctx = crate::extensions::framework::types::ToolRuntimeContext::new()
                    .with_run_id("hook_run")
                    .with_agent_id(agent_id.clone().unwrap_or_else(|| "unknown".to_string()))
                    .with_session_id(session_id.clone().unwrap_or_else(|| "unknown".to_string()));
                let tool_ctx = if let Some(ref ws) = workspace {
                    tool_ctx.with_workspace(ws.clone())
                } else {
                    tool_ctx
                };
                ctx.set_state("tool_context", tool_ctx);
            }

            // Invoke handler
            trace!(handler_id = %hook_id, "Calling handler");
            let result = handler.handler.handle(ctx).await;

            // Record telemetry
            let duration_ms = start.elapsed().as_millis() as u64;
            self.services
                .record_invocation(&hook_id, &point, duration_ms);

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
                    debug!(handler_id = %hook_id, error = %e, "Handler error");
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
    pub async fn invoke_hook_text(&self, point: HookPoint, input: HookInput) -> Option<String> {
        match self.invoke_hook(point, input).await {
            HookResult::Continue(HookOutput::Text(text)) => Some(text),
            HookResult::Replace(HookOutput::Text(text)) => Some(text),
            HookResult::Continue(HookOutput::Vec(outputs)) => {
                // Concatenate text outputs
                let texts: Vec<String> = outputs
                    .into_iter()
                    .filter_map(|o| o.as_text().map(std::string::ToString::to_string))
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

    /// Get a hook by ID
    pub async fn get_hook(&self, hook_id: &HookId) -> Option<RegisteredHook> {
        self.hooks.read().await.get(hook_id).cloned()
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}
