//! Tool Registry
//!
//! This module implements the registry for tools and tool policy.
//! It manages tool registration, metadata, listing, and whitelist enforcement.
//!
//! Built on [`crate::common::registry::SharedRegistry`] to avoid hand-rolling
//! `Arc<RwLock<HashMap<K, V>>>` patterns.

use crate::common::registry::SharedRegistry;
use crate::extensions::framework::types::{ExtensionId, HookId};
use crate::principal::{Capabilities, Capability};
use anyhow::Result;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{debug, instrument, warn};

/// Registry for tools and tool policy
///
/// This component manages tool registrations and enforces the whitelist policy.
/// The tool index is backed by a [`SharedRegistry`] for thread-safe access.
#[derive(Debug)]
pub struct ToolRegistry {
    /// Tool index: maps tool name to hook ID for O(1) lookup
    pub(crate) tool_index: SharedRegistry<String, HookId>,

    /// Maps tool name to the owning extension ID for whitelist checking.
    /// This decouples the whitelist from tool-name string parsing.
    tool_owners: RwLock<HashMap<String, ExtensionId>>,

    /// Tool configuration (whitelist, per-tool settings)
    tool_config: RwLock<crate::common::types::agent_legacy::ExtensionConfig>,
}

impl ToolRegistry {
    /// Create a new Tool Registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_index: SharedRegistry::new(),
            tool_owners: RwLock::new(HashMap::new()),
            tool_config: RwLock::new(
                crate::common::types::agent_legacy::ExtensionConfig::default(),
            ),
        }
    }

    /// Set the tool configuration (whitelist, etc.)
    pub async fn set_tool_config(
        &self,
        config: crate::common::types::agent_legacy::ExtensionConfig,
    ) {
        let mut tool_config = self.tool_config.write().await;
        *tool_config = config;
        debug!("Updated tool configuration");
    }

    /// Check if a tool is enabled according to whitelist.
    ///
    /// Looks up the tool's owning `extension_id` and checks whether *that*
    /// canonical ID is present in the whitelist.  This makes the check
    /// independent of any tool-name naming convention.
    pub async fn is_tool_enabled(&self, tool_name: &str) -> bool {
        self.is_tool_enabled_with_whitelist(tool_name, None).await
    }

    /// Check if a tool is enabled, using a per-call capability set when provided.
    ///
    /// When `capabilities` is `Some`, the capability `tool:{tool_name}` must be
    /// granted. Wildcards such as `tool:*` are expanded by the capability set.
    ///
    /// When `capabilities` is `None`, the caller is unbound from any principal
    /// (e.g. standalone/test fixtures). Permit every registered tool.
    pub async fn is_tool_enabled_with_whitelist(
        &self,
        tool_name: &str,
        capabilities: Option<&Capabilities>,
    ) -> bool {
        match capabilities {
            Some(caps) => caps.is_granted(&Capability::new(format!("tool:{tool_name}"))),
            None => true,
        }
    }

    /// Register a tool in the index
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    /// * `hook_id` - The hook ID associated with this tool
    /// * `extension_id` - ID of the extension that owns this tool
    #[instrument(skip(self), fields(tool_name = %tool_name, hook_id = %hook_id, extension_id = %extension_id))]
    pub async fn register_tool(
        &self,
        tool_name: &str,
        hook_id: HookId,
        extension_id: ExtensionId,
    ) -> Result<()> {
        self.tool_index.insert(tool_name.to_string(), hook_id).await;
        self.tool_owners
            .write()
            .await
            .insert(tool_name.to_string(), extension_id);
        debug!(tool_name = %tool_name, hook_id = %hook_id, "Registered tool in index");
        Ok(())
    }

    /// Unregister a tool by name
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool to unregister
    #[instrument(skip(self), fields(tool_name = %tool_name))]
    pub async fn unregister_tool(&self, tool_name: &str) -> Result<Option<HookId>> {
        let hook_id = self.tool_index.remove(&tool_name.to_string()).await;
        self.tool_owners.write().await.remove(tool_name);
        if hook_id.is_some() {
            debug!(tool_name = %tool_name, "Unregistered tool from index");
        } else {
            warn!(tool_name = %tool_name, "Attempted to unregister unknown tool");
        }
        Ok(hook_id)
    }

    /// Get the hook ID for a tool by name
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    ///
    /// # Returns
    /// The hook ID if found, None otherwise
    pub async fn get_tool_hook_id(&self, tool_name: &str) -> Option<HookId> {
        self.tool_index.get(&tool_name.to_string()).await
    }

    /// Get the number of registered tools
    pub async fn tool_count(&self) -> usize {
        self.tool_index.len().await
    }

    /// List all registered tool names
    pub async fn list_tool_names(&self) -> Vec<String> {
        self.tool_index.keys().await
    }

    /// Resolve a list of bare tool names to their canonical
    /// `extension_id` form.
    ///
    /// For each input name, if the registry knows the tool's owning
    /// `extension_id` (e.g. `"Read"` → `"builtin:tool:Read"`), the
    /// canonical form is returned. For unknown names — typically
    /// extension-provided skills or MCPs whose canonical ID is
    /// already the bare name — the bare name is returned unchanged.
    ///
    /// This is what the principal's `capabilities` go through before
    /// they land in `ExtensionConfig.enabled`: the core's whitelist
    /// check ([`Self::is_tool_enabled`]) prefers the canonical form,
    /// and the bare-name fallback is a defensive escape hatch the
    /// Phase-4 cleanup is moving away from.
    pub async fn resolve_canonical_ids(&self, names: &[String]) -> Vec<String> {
        let owners = self.tool_owners.read().await;
        names
            .iter()
            .map(|name| {
                owners
                    .get(name)
                    .map(|ext_id| ext_id.0.clone())
                    .unwrap_or_else(|| name.clone())
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_per_call_allowlist_ignores_global_config() {
        let registry = ToolRegistry::new();
        registry
            .register_tool("Read", HookId::new(), ExtensionId::new("builtin:tool:Read"))
            .await
            .unwrap();

        // Set a global config that *disables* Read.
        let global = crate::common::types::agent_legacy::ExtensionConfig {
            enabled: vec!["other".to_string()],
            ..Default::default()
        };
        registry.set_tool_config(global).await;

        // A per-call capability set that *enables* Read should still permit it.
        let caps = Capabilities::with_grants(["tool:Read"]);
        assert!(registry
            .is_tool_enabled_with_whitelist("Read", Some(&caps))
            .await);

        // A per-call capability set without Read should deny it.
        let caps = Capabilities::with_grants(["tool:Other"]);
        assert!(!registry
            .is_tool_enabled_with_whitelist("Read", Some(&caps))
            .await);
    }

    #[tokio::test]
    async fn test_unknown_tool_with_per_call_allowlist_uses_capability() {
        let registry = ToolRegistry::new();
        // No registration, so no owner is recorded.
        let caps = Capabilities::with_grants(["tool:custom_skill"]);
        assert!(registry
            .is_tool_enabled_with_whitelist("custom_skill", Some(&caps))
            .await);
    }

    #[tokio::test]
    async fn test_empty_allowlist_denies_all() {
        let registry = ToolRegistry::new();
        registry
            .register_tool("Read", HookId::new(), ExtensionId::new("builtin:tool:Read"))
            .await
            .unwrap();

        // An empty capability set must fail-closed.
        let caps = Capabilities::new();
        assert!(
            !registry
                .is_tool_enabled_with_whitelist("Read", Some(&caps))
                .await,
            "empty capability set should deny every tool"
        );
    }

    #[tokio::test]
    async fn test_wildcard_capability_matches() {
        let registry = ToolRegistry::new();
        registry
            .register_tool("Read", HookId::new(), ExtensionId::new("builtin:tool:Read"))
            .await
            .unwrap();

        let caps = Capabilities::with_grants(["tool:*"]);
        assert!(registry
            .is_tool_enabled_with_whitelist("Read", Some(&caps))
            .await);
    }
}
