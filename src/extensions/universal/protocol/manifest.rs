//! Universal Tool Manifest v2
//!
//! SRP: This module ONLY handles manifest parsing and validation.
//! No protocol logic, no execution.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

// Re-export shared types for convenience
pub use crate::extensions::services::{ParamSource, ReservedParamsConfig};

/// Tool manifest with reserved parameter support
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Tool name
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// LLM-optimized description with usage guidance
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_description: Option<String>,
    /// JSON Schema for exposed parameters (what LLM sees)
    pub parameters: Value,
    /// Reserved parameters (injected at runtime, hidden from LLM)
    #[serde(default, skip_serializing_if = "ReservedParamsConfig::is_empty")]
    pub reserved_parameters: ReservedParamsConfig,
    /// Protocol configuration
    #[serde(default)]
    pub protocol: ProtocolConfig,
    /// Additional metadata
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Protocol configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProtocolConfig {
    /// Protocol version
    #[serde(default = "default_version")]
    pub version: String,
    /// Transport type
    #[serde(default)]
    pub transport: TransportType,
    /// Whether tool supports streaming/progress
    #[serde(default)]
    pub supports_streaming: bool,
}

fn default_version() -> String {
    "2.0".to_string()
}

/// Transport mechanism
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransportType {
    #[default]
    Stdio,
    Tcp,
    UnixSocket,
}

impl Manifest {
    /// Load manifest from file
    ///
    /// Supports both JSON (legacy tool protocol format) and YAML (ADR-024 unified manifest).
    pub async fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = tokio::fs::read_to_string(&path).await?;
        Self::from_str(&content, path.as_ref())
    }

    /// Load manifest from file (sync version)
    ///
    /// Supports both JSON (legacy tool protocol format) and YAML (ADR-024 unified manifest).
    pub fn from_file_sync(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(&path)?;
        Self::from_str(&content, path.as_ref())
    }

    /// Parse manifest from string, auto-detecting JSON vs YAML.
    fn from_str(content: &str, path: &Path) -> anyhow::Result<Self> {
        // Try JSON first (legacy tool protocol format)
        if let Ok(manifest) = serde_json::from_str::<Self>(content) {
            return Ok(manifest);
        }

        // Fall back to YAML (ADR-024 unified manifest format)
        let yaml: serde_yaml::Value = serde_yaml::from_str(content)
            .map_err(|e| anyhow::anyhow!("Failed to parse manifest as JSON or YAML at {:?}: {}", path, e))?;

        let name = yaml.get("name")
            .or_else(|| yaml.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Manifest missing 'name' or 'id' field at {:?}", path))?
            .to_string();

        let description = yaml.get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let llm_description = yaml.get("llm_description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let parameters = yaml.get("parameters")
            .map(|v| crate::extensions::adapters::parsing::yaml_to_json(v.clone()))
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));

        let reserved_parameters: ReservedParamsConfig = yaml.get("reserved_parameters")
            .and_then(|v| {
                let json_val = crate::extensions::adapters::parsing::yaml_to_json(v.clone());
                serde_json::from_value(json_val).ok()
            })
            .unwrap_or_default();

        let protocol = yaml.get("protocol")
            .and_then(|v| {
                let json_val = crate::extensions::adapters::parsing::yaml_to_json(v.clone());
                serde_json::from_value(json_val).ok()
            })
            .unwrap_or_default();

        Ok(Self {
            name,
            description,
            llm_description,
            parameters,
            reserved_parameters,
            protocol,
            extra: std::collections::HashMap::new(),
        })
    }

    /// Get the parameter schema exposed to LLM (no reserved params)
    ///
    /// This filters out reserved parameters so they are not visible to the LLM,
    /// preventing confusion and security issues.
    #[must_use]
    pub fn exposed_parameters(&self) -> Value {
        use crate::extension::protocols::shared::filter_reserved_params;
        use std::collections::HashSet;

        let reserved: HashSet<String> = self.reserved_param_names().into_iter().cloned().collect();

        filter_reserved_params(&self.parameters, &reserved)
    }

    /// Get reserved parameter names
    #[must_use]
    pub fn reserved_param_names(&self) -> Vec<&String> {
        self.reserved_parameters.names().collect()
    }

    /// Check if a parameter is reserved
    #[must_use]
    pub fn is_reserved(&self, name: &str) -> bool {
        self.reserved_parameters.contains(name)
    }

    /// Get the LLM-facing description
    #[must_use]
    pub fn llm_description(&self) -> String {
        self.llm_description
            .clone()
            .unwrap_or_else(|| self.description.clone())
    }

    /// Validate parameters against schema
    ///
    /// This checks that:
    /// 1. All required exposed parameters are present
    /// 2. No reserved parameters are present (they should be injected)
    pub fn validate_params(&self, params: &Value) -> anyhow::Result<()> {
        use crate::extension::protocols::shared::validation;
        use std::collections::HashSet;

        let reserved: HashSet<String> = self.reserved_param_names().into_iter().cloned().collect();

        // Use shared validation for reserved params check
        validation::validate_no_reserved_in_user_params(params, &reserved)?;

        // Get exposed schema (without reserved params) for required check
        let exposed = self.exposed_parameters();
        validation::validate_required_params(params, &exposed)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_manifest() -> Manifest {
        Manifest {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            llm_description: Some("Use when testing".to_string()),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
            reserved_parameters: ReservedParamsConfig::new()
                .with_runtime("session_id", "session_id"),
            protocol: ProtocolConfig::default(),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn test_manifest_validation_ok() {
        let manifest = create_test_manifest();
        let params = json!({"query": "test"});
        assert!(manifest.validate_params(&params).is_ok());
    }

    #[test]
    fn test_manifest_validation_missing_required() {
        let manifest = create_test_manifest();
        let params = json!({});
        assert!(manifest.validate_params(&params).is_err());
    }

    #[test]
    fn test_manifest_validation_reserved_in_input() {
        let manifest = create_test_manifest();
        // Should fail - session_id is reserved
        let params = json!({"query": "test", "session_id": "bad"});
        assert!(manifest.validate_params(&params).is_err());
    }

    #[test]
    fn test_reserved_params_access() {
        let manifest = create_test_manifest();

        // Direct access to reserved params
        assert!(manifest.reserved_parameters.contains("session_id"));
        assert!(matches!(
            manifest.reserved_parameters.get("session_id").unwrap(),
            ParamSource::Runtime { field } if field == "session_id"
        ));
    }

    #[test]
    fn test_llm_description_fallback() {
        let mut manifest = create_test_manifest();

        // With llm_description
        assert_eq!(manifest.llm_description(), "Use when testing");

        // Without llm_description
        manifest.llm_description = None;
        assert_eq!(manifest.llm_description(), "A test tool");
    }
}
