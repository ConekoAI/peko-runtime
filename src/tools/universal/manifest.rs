//! Universal Tool Manifest v2
//!
//! SRP: This module ONLY handles manifest parsing and validation.
//! No protocol logic, no execution.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_parameters: Option<HashMap<String, ReservedParam>>,
    /// Protocol configuration
    #[serde(default)]
    pub protocol: ProtocolConfig,
    /// Additional metadata
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Reserved parameter definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservedParam {
    /// Source of the parameter value
    pub source: ParamSource,
    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Source for parameter injection
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamSource {
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

    /// Get the parameter schema exposed to LLM (no reserved params)
    pub fn exposed_parameters(&self) -> &Value {
        &self.parameters
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
        let obj = params
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Parameters must be an object"))?;

        // Check no reserved params in input
        if let Some(ref reserved) = self.reserved_parameters {
            for key in obj.keys() {
                if reserved.contains_key(key) {
                    return Err(anyhow::anyhow!(
                        "Parameter '{}' is reserved and should not be provided",
                        key
                    ));
                }
            }
        }

        // Check required params
        if let Some(required) = self.parameters.get("required").and_then(|r| r.as_array()) {
            for req in required {
                if let Some(req_str) = req.as_str() {
                    if !obj.contains_key(req_str) {
                        return Err(anyhow::anyhow!("Missing required parameter: {}", req_str));
                    }
                }
            }
        }

        Ok(())
    }
}

/// Create a merged parameter set with injection
/// 
/// Takes user-provided params and injects reserved params from context
pub fn merge_with_injection(
    manifest: &Manifest,
    user_params: Value,
    context: &super::protocol::ExecutionContext,
) -> anyhow::Result<Value> {
    let mut merged = user_params;

    // Ensure params is an object
    if !merged.is_object() {
        return Err(anyhow::anyhow!("Parameters must be an object"));
    }

    let obj = merged.as_object_mut().unwrap();

    // Inject reserved parameters
    if let Some(ref reserved) = manifest.reserved_parameters {
        for (name, spec) in reserved {
            let value = match &spec.source {
                ParamSource::Runtime { field } => match field.as_str() {
                    "session_id" => Value::String(context.session_id.clone()),
                    "agent_id" => Value::String(context.agent_id.clone()),
                    "peer_id" => context.peer_id.clone().map(Value::String).unwrap_or(Value::Null),
                    "workspace" => Value::String(context.workspace.clone()),
                    "run_id" => context.run_id.clone().map(Value::String).unwrap_or(Value::Null),
                    _ => Value::Null,
                },
                ParamSource::Env { var } => {
                    std::env::var(var).map(Value::String).unwrap_or(Value::Null)
                }
                ParamSource::Static { value } => value.clone(),
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
        reserved.insert(
            "session_id".to_string(),
            ReservedParam {
                source: ParamSource::Runtime {
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
