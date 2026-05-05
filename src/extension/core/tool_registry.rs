//! Tool Registry
//!
//! This module implements the registry for tools and tool policy.
//! It manages tool registration, metadata, listing, and whitelist enforcement.
//!
//! Built on [`crate::common::registry::SharedRegistry`] to avoid hand-rolling
//! `Arc<RwLock<HashMap<K, V>>>` patterns.

use crate::common::registry::SharedRegistry;
use crate::extension::types::{HookId, ToolMetadata};
use anyhow::Result;
use std::sync::Arc;
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

    /// Tool configuration (whitelist, per-tool settings)
    tool_config: RwLock<crate::types::agent::ToolConfig>,
}

impl ToolRegistry {
    /// Create a new Tool Registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_index: SharedRegistry::new(),
            tool_config: RwLock::new(crate::types::agent::ExtensionConfig::default()),
        }
    }

    /// Set the tool configuration (whitelist, etc.)
    pub async fn set_tool_config(&self, config: crate::types::agent::ExtensionConfig) {
        let mut tool_config = self.tool_config.write().await;
        *tool_config = config;
        debug!("Updated tool configuration");
    }

    /// Check if a tool is enabled according to whitelist
    pub async fn is_tool_enabled(&self, tool_name: &str) -> bool {
        let config = self.tool_config.read().await;
        config.is_extension_enabled(tool_name)
    }

    /// Register a tool in the index
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    /// * `hook_id` - The hook ID associated with this tool
    #[instrument(skip(self), fields(tool_name = %tool_name, hook_id = %hook_id))]
    pub async fn register_tool(&self, tool_name: &str, hook_id: HookId) -> Result<()> {
        self.tool_index.insert(tool_name.to_string(), hook_id).await;
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
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
