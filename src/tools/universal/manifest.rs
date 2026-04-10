//! Universal Tool Manifest v2
//!
//! SRP: This module ONLY handles manifest parsing and validation.
//! No protocol logic, no execution.
//!
//! # Note on Reserved Parameters
//!
//! This module now uses the shared `ReservedParamsConfig` from `extensions::services`.
//! The legacy `ReservedParam` and `ParamSource` types are kept for backward compatibility
//! but are deprecated. Use `ReservedParamsConfig` and `ParamSource` from
//! `crate::extensions::services` instead.

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
    /// 
    /// Note: This is stored in the legacy format for backward compatibility.
    /// Use `reserved_params_config()` to get the unified `ReservedParamsConfig`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_parameters: Option<HashMap<String, ReservedParam>>,
    /// Protocol configuration
    #[serde(default)]
    pub protocol: ProtocolConfig,
    /// Additional metadata
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Reserved parameter definition (LEGACY)
///
/// # Deprecated
/// Use `ReservedParamsConfig` from `crate::extensions::services` instead.
/// This type is kept for backward compatibility during manifest parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[deprecated(
    since = "0.2.0",
    note = "Use ReservedParamsConfig from crate::extensions::services instead"
)]
pub struct ReservedParam {
    /// Source of the parameter value
    pub source: ParamSourceLegacy,
    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Source for parameter injection (LEGACY)
///
/// # Deprecated
/// Use `ParamSource` from `crate::extensions::services` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[deprecated(
    since = "0.2.0",
    note = "Use ParamSource from crate::extensions::services instead"
)]
pub enum ParamSourceLegacy {
    /// Injected from runtime context (session_id, agent_id, etc.)
    Runtime { field: String },
    /// Read from environment variable
    Env { var: String },
    /// Static value
    Static { value: Value },
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
    pub async fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = tokio::fs::read_to_string(path).await?;
        let manifest: Self = serde_json::from_str(&content)?;
        Ok(manifest)
    }

    /// Load manifest from file (sync version)
    pub fn from_file_sync(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = serde_json::from_str(&content)?;
        Ok(manifest)
    }

    /// Get reserved parameters as unified config
    ///
    /// This converts the legacy format to the new unified `ReservedParamsConfig`
    /// used by the Extension Framework.
    pub fn reserved_params_config(&self) -> ReservedParamsConfig {
        ReservedParamsConfig::from_universal_legacy(&self.reserved_parameters)
    }

    /// Get the parameter schema exposed to LLM (no reserved params)
    ///
    /// This filters out reserved parameters so they are not visible to the LLM,
    /// preventing confusion and security issues.
    pub fn exposed_parameters(&self) -> Value {
        use crate::tools::shared::filter_reserved_params;
        use std::collections::HashSet;
        
        let reserved: HashSet<String> = self.reserved_param_names()
            .into_iter()
            .cloned()
            .collect();
        
        filter_reserved_params(&self.parameters, &reserved)
    }

    /// Get reserved parameter names
    pub fn reserved_param_names(&self) -> Vec<&String> {
        self.reserved_parameters
            .as_ref()
            .map(|r| r.keys().collect())
            .unwrap_or_default()
    }

    /// Check if a parameter is reserved
    pub fn is_reserved(&self, name: &str) -> bool {
        self.reserved_parameters
            .as_ref()
            .map(|r| r.contains_key(name))
            .unwrap_or(false)
    }

    /// Get the LLM-facing description
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
        use crate::tools::shared::validation;
        use std::collections::HashSet;
        
        let reserved: HashSet<String> = self.reserved_param_names()
            .into_iter()
            .cloned()
            .collect();
        
        // Use shared validation for reserved params check
        validation::validate_no_reserved_in_user_params(params, &reserved)?;
        
        // Get exposed schema (without reserved params) for required check
        let exposed = self.exposed_parameters();
        validation::validate_required_params(params, &exposed)?;
        
        Ok(())
    }
}

/// Create a merged parameter set with injection (LEGACY)
///
/// # Deprecated
/// Use `ToolExecutionService::inject_reserved_params()` from `crate::extensions::services`
/// instead. This function is kept for backward compatibility.
///
/// Takes user-provided params and injects reserved params from context.
#[deprecated(
    since = "0.2.0",
    note = "Use ToolExecutionService::inject_reserved_params() from crate::extensions::services instead"
)]
pub fn merge_with_injection(
    manifest: &Manifest,
    user_params: Value,
    context: &super::protocol::ExecutionContext,
) -> anyhow::Result<Value> {
    use crate::tools::shared::context_resolver::{ContextResolver, ExecutionContextAdapter};
    
    let mut merged = user_params;

    // Ensure params is an object
    if !merged.is_object() {
        return Err(anyhow::anyhow!("Parameters must be an object"));
    }

    let obj = merged.as_object_mut().unwrap();

    // Inject reserved parameters using shared context resolver
    if let Some(ref reserved) = manifest.reserved_parameters {
        let adapter = ExecutionContextAdapter::new(context.clone());
        
        for (name, spec) in reserved {
            let value = match &spec.source {
                ParamSourceLegacy::Runtime { field } => {
                    ContextResolver::resolve_field(&adapter, field)
                }
                ParamSourceLegacy::Env { var } => {
                    std::env::var(var).map(Value::String).unwrap_or(Value::Null)
                }
                ParamSourceLegacy::Static { value } => value.clone(),
            };
            obj.insert(name.clone(), value);
        }
    }

    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_manifest() -> Manifest {
        let mut reserved = HashMap::new();
        #[allow(deprecated)]
        reserved.insert(
            "session_id".to_string(),
            ReservedParam {
                source: ParamSourceLegacy::Runtime {
                    field: "session_id".to_string(),
                },
                description: Some("Current session ID".to_string()),
            },
        );

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
            reserved_parameters: Some(reserved),
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
    fn test_reserved_params_config_conversion() {
        let manifest = create_test_manifest();
        let config = manifest.reserved_params_config();
        
        // Should have converted to unified format
        assert!(config.contains("session_id"));
        assert!(matches!(
            config.get("session_id").unwrap(),
            ParamSource::Runtime { field } if field == "session_id"
        ));
    }

    #[test]
    #[allow(deprecated)]
    fn test_merge_with_injection() {
        let manifest = create_test_manifest();
        let user_params = json!({"query": "hello"});
        let context = super::super::protocol::ExecutionContext {
            session_id: "sess_123".to_string(),
            agent_id: "agent_test".to_string(),
            peer_id: None,
            workspace: "/tmp".to_string(),
            run_id: None,
        };

        let merged = merge_with_injection(&manifest, user_params, &context).unwrap();
        
        assert_eq!(merged["query"], "hello");
        assert_eq!(merged["session_id"], "sess_123");
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
