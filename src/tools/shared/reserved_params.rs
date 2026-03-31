//! Unified Reserved Parameter Configuration
//!
//! Provides a single, consistent format for defining reserved parameters
//! across both Universal Tools and MCP tools.
//!
//! # Supported Sources
//!
//! - `runtime`: Injected from runtime context (session_id, agent_id, etc.)
//! - `env`: Read from environment variable
//! - `static`: Hardcoded static value
//!
//! # Example (JSON)
//!
//! ```json
//! {
//!   "reserved_parameters": {
//!     "agent_id": {
//!       "source": "runtime",
//!       "field": "agent_id"
//!     },
//!     "api_key": {
//!       "source": "env",
//!       "var": "API_KEY"
//!     },
//!     "version": {
//!       "source": "static",
//!       "value": "1.0.0"
//!     }
//!   }
//! }
//! ```
//!
//! # Example (TOML)
//!
//! ```toml
//! [reserved_parameters]
//! agent_id = { source = "runtime", field = "agent_id" }
//! api_key = { source = "env", var = "API_KEY" }
//! version = { source = "static", value = "1.0.0" }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reserved parameter definition (unified format)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReservedParamSource {
    /// Injected from runtime context (session_id, agent_id, etc.)
    Runtime { field: String },
    /// Read from environment variable
    Env { var: String },
    /// Static value
    Static { value: Value },
}

impl ReservedParamSource {
    /// Create a runtime parameter from a context field
    pub fn runtime(field: impl Into<String>) -> Self {
        Self::Runtime {
            field: field.into(),
        }
    }

    /// Create a parameter from an environment variable
    pub fn env(var: impl Into<String>) -> Self {
        Self::Env { var: var.into() }
    }

    /// Create a static parameter with a hardcoded value
    pub fn static_value(value: impl Into<Value>) -> Self {
        Self::Static {
            value: value.into(),
        }
    }

    /// Get the source type as a string
    #[must_use]
    pub fn source_type(&self) -> &'static str {
        match self {
            Self::Runtime { .. } => "runtime",
            Self::Env { .. } => "env",
            Self::Static { .. } => "static",
        }
    }

    /// Resolve the parameter value
    ///
    /// # Arguments
    /// * `ctx` - Optional ToolContext for runtime resolution
    ///
    /// # Returns
    /// The resolved value or Value::Null if not available
    pub fn resolve(&self, ctx: Option<&crate::tools::ToolContext>) -> Value {
        use crate::tools::shared::context_resolver::{ContextResolver, ToolContextAdapter};

        match self {
            Self::Runtime { field } => {
                if let Some(ctx) = ctx {
                    let adapter = ToolContextAdapter::new(ctx);
                    ContextResolver::resolve_field(&adapter, field)
                } else {
                    Value::Null
                }
            }
            Self::Env { var } => {
                std::env::var(var).map_or(Value::Null, |v| Value::String(v))
            }
            Self::Static { value } => value.clone(),
        }
    }
}

/// Reserved parameter with optional metadata
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReservedParam {
    /// Source of the parameter value
    #[serde(flatten)]
    pub source: ReservedParamSource,
    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ReservedParam {
    /// Create a new reserved parameter
    pub fn new(source: ReservedParamSource) -> Self {
        Self {
            source,
            description: None,
        }
    }

    /// Add a description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Resolve the parameter value
    pub fn resolve(&self, ctx: Option<&crate::tools::ToolContext>) -> Value {
        self.source.resolve(ctx)
    }
}

impl From<ReservedParamSource> for ReservedParam {
    fn from(source: ReservedParamSource) -> Self {
        Self::new(source)
    }
}

/// Collection of reserved parameters
pub type ReservedParams = std::collections::HashMap<String, ReservedParam>;

/// Resolve all reserved parameters in a collection
pub fn resolve_all(params: &ReservedParams, ctx: Option<&crate::tools::ToolContext>) -> Value {
    let mut result = serde_json::Map::new();
    for (name, param) in params {
        result.insert(name.clone(), param.resolve(ctx));
    }
    Value::Object(result)
}

/// Convert from old MCP format to unified format
pub fn from_mcp_config(
    source: &str,
    field: Option<&str>,
    var: Option<&str>,
    value: Option<&str>,
) -> Option<ReservedParamSource> {
    match source {
        "runtime" => field.map(|f| ReservedParamSource::runtime(f.to_string())),
        "env" => var.map(|v| ReservedParamSource::env(v.to_string())),
        "static" => value.map(|v| ReservedParamSource::static_value(v.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_runtime_param() {
        let param = ReservedParamSource::runtime("agent_id");
        assert_eq!(param.source_type(), "runtime");

        // Create a context for testing
        let abort_signal = crate::tools::AbortSignal::new();
        let ctx = abort_signal
            .create_context("run_123", "tool_1", "test_tool")
            .with_agent_id("agent_test");

        let value = param.resolve(Some(&ctx));
        assert_eq!(value, json!("agent_test"));
    }

    #[test]
    fn test_env_param() {
        // Set an environment variable for testing
        std::env::set_var("TEST_PEKOBOT_VAR", "test_value");

        let param = ReservedParamSource::env("TEST_PEKOBOT_VAR");
        assert_eq!(param.source_type(), "env");

        let value = param.resolve(None);
        assert_eq!(value, json!("test_value"));

        // Clean up
        std::env::remove_var("TEST_PEKOBOT_VAR");
    }

    #[test]
    fn test_static_param() {
        let param = ReservedParamSource::static_value("1.0.0");
        assert_eq!(param.source_type(), "static");

        let value = param.resolve(None);
        assert_eq!(value, json!("1.0.0"));
    }

    #[test]
    fn test_param_with_description() {
        let param = ReservedParam::new(ReservedParamSource::runtime("session_id"))
            .with_description("Current session ID");

        assert!(param.description.is_some());
        assert_eq!(param.description.unwrap(), "Current session ID");
    }

    #[test]
    fn test_resolve_all() {
        let mut params = std::collections::HashMap::new();
        params.insert(
            "agent_id".to_string(),
            ReservedParam::new(ReservedParamSource::runtime("agent_id")),
        );
        params.insert(
            "version".to_string(),
            ReservedParam::new(ReservedParamSource::static_value("1.0.0")),
        );

        let abort_signal = crate::tools::AbortSignal::new();
        let ctx = abort_signal
            .create_context("run_123", "tool_1", "test_tool")
            .with_agent_id("agent_test");

        let result = resolve_all(&params, Some(&ctx));
        
        assert_eq!(result["agent_id"], json!("agent_test"));
        assert_eq!(result["version"], json!("1.0.0"));
    }

    #[test]
    fn test_json_serialization_runtime() {
        let param = ReservedParamSource::runtime("agent_id");
        let json = serde_json::to_string(&param).unwrap();
        
        assert!(json.contains("runtime"));
        assert!(json.contains("agent_id"));

        let deserialized: ReservedParamSource = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.source_type(), "runtime");
    }

    #[test]
    fn test_json_serialization_env() {
        let param = ReservedParamSource::env("API_KEY");
        let json = serde_json::to_string(&param).unwrap();
        
        assert!(json.contains("env"));
        assert!(json.contains("API_KEY"));

        let deserialized: ReservedParamSource = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.source_type(), "env");
    }

    #[test]
    fn test_reserved_param_wrapper() {
        // Test ReservedParam wrapper with description
        let param = ReservedParam::new(ReservedParamSource::runtime("session_id"))
            .with_description("Current session ID");
        
        assert_eq!(param.source.source_type(), "runtime");
        assert_eq!(param.description, Some("Current session ID".to_string()));
        
        // Test serialization
        let json = serde_json::to_string(&param).unwrap();
        assert!(json.contains("runtime"));
        assert!(json.contains("session_id"));
        
        // Deserialize and verify
        let deserialized: ReservedParam = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.source.source_type(), "runtime");
    }

    #[test]
    fn test_from_mcp_config() {
        let runtime = from_mcp_config("runtime", Some("agent_id"), None, None);
        assert!(runtime.is_some());
        assert_eq!(runtime.unwrap().source_type(), "runtime");

        let env = from_mcp_config("env", None, Some("API_KEY"), None);
        assert!(env.is_some());
        assert_eq!(env.unwrap().source_type(), "env");

        let static_val = from_mcp_config("static", None, None, Some("1.0.0"));
        assert!(static_val.is_some());
        assert_eq!(static_val.unwrap().source_type(), "static");
    }
}
