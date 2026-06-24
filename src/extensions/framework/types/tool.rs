//! Tool-related types

use crate::extensions::framework::types::HookId;
use serde::{Deserialize, Serialize};

/// Source of a tool (for metadata tracking)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Built-in tool (part of the core codebase)
    BuiltIn,
    /// MCP tool from an MCP server
    Mcp { server: String },
    /// Universal tool from an extension
    Universal { extension_id: String },
    /// General extension tool
    General { extension_id: String },
}

impl ToolSource {
    /// Get a human-readable description of the source
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            ToolSource::BuiltIn => "built-in".to_string(),
            ToolSource::Mcp { server } => format!("MCP server: {server}"),
            ToolSource::Universal { extension_id } => {
                format!("universal extension: {extension_id}")
            }
            ToolSource::General { extension_id } => format!("extension: {extension_id}"),
        }
    }
}

/// Metadata for a registered tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    /// Tool name (unique identifier)
    pub name: String,
    /// Tool description (LLM-optimized)
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
    /// Source of the tool
    pub source: ToolSource,
    /// Reserved parameters configuration
    pub reserved_params:
        crate::extensions::framework::services::reserved_params::ReservedParamsConfig,
    /// Companion hook IDs registered alongside the primary execution hook.
    /// Populated by `ExtensionCore::register_tool()` and used during
    /// `unregister_tool()` for atomic cleanup.
    #[serde(skip)]
    pub companion_hook_ids: Option<Vec<HookId>>,
}

impl ToolMetadata {
    /// Create new tool metadata
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        source: ToolSource,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            source,
            reserved_params:
                crate::extensions::framework::services::reserved_params::ReservedParamsConfig::new(),
            companion_hook_ids: None,
        }
    }

    /// Set reserved params configuration
    #[must_use]
    pub fn with_reserved_params(
        mut self,
        config: crate::extensions::framework::services::reserved_params::ReservedParamsConfig,
    ) -> Self {
        self.reserved_params = config;
        self
    }

    /// Set companion hook IDs (used internally by `ExtensionCore::register_tool`).
    #[must_use]
    pub fn with_companion_hook_ids(mut self, ids: Vec<HookId>) -> Self {
        self.companion_hook_ids = Some(ids);
        self
    }

    /// Convert to `ToolDefinition` for LLM API
    #[must_use]
    pub fn to_tool_definition(&self) -> crate::providers::ToolDefinition {
        crate::providers::ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_source() {
        assert_eq!(ToolSource::BuiltIn.description(), "built-in");
        assert_eq!(
            ToolSource::Mcp {
                server: "test".to_string()
            }
            .description(),
            "MCP server: test"
        );
    }

    #[test]
    fn test_tool_metadata() {
        let meta = ToolMetadata::new(
            "test_tool",
            "A test tool",
            serde_json::json!({"type": "object"}),
            ToolSource::BuiltIn,
        );
        assert_eq!(meta.name, "test_tool");
        assert!(meta.companion_hook_ids.is_none());
    }
}
