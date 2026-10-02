//! Tool-related types
//!
//! Lifted from `src/extensions/framework/types/tool.rs` in Phase 7.
//! `to_tool_definition` produces
//! `peko_provider_api::ToolDefinition` (was `crate::providers::ToolDefinition`).
//! `reserved_params` is the data-only `peko_extension_api::ReservedParamsConfig`
//! from this crate (no resolution methods — those live in the host).

use crate::reserved_params::ReservedParamsConfig;
use peko_provider_api::ToolDefinition;
use serde::{Deserialize, Serialize};

/// Source of a tool (for metadata tracking)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Built-in tool (part of the core codebase)
    BuiltIn,
    /// MCP tool from an MCP server
    Mcp { server: String },
}

impl ToolSource {
    /// Get a human-readable description of the source
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            ToolSource::BuiltIn => "built-in".to_string(),
            ToolSource::Mcp { server } => format!("MCP server: {server}"),
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
    pub reserved_params: ReservedParamsConfig,
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
            reserved_params: ReservedParamsConfig::new(),
        }
    }

    /// Set reserved params configuration
    #[must_use]
    pub fn with_reserved_params(mut self, config: ReservedParamsConfig) -> Self {
        self.reserved_params = config;
        self
    }

    /// Convert to `ToolDefinition` for LLM API
    #[must_use]
    pub fn to_tool_definition(&self) -> ToolDefinition {
        ToolDefinition {
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
    fn metadata_round_trips_without_exposure() {
        let metadata = ToolMetadata::new(
            "Read",
            "Reads files",
            serde_json::json!({"type":"object"}),
            ToolSource::BuiltIn,
        );
        let encoded = serde_json::to_value(&metadata).unwrap();
        assert!(encoded.get("exposure").is_none());
        let decoded: ToolMetadata = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.to_tool_definition().name, "Read");
    }
}
