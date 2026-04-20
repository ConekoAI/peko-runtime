//! Unified Reserved Parameter Configuration
//!
//! This module provides a single, unified type for reserved parameter configuration
//! across all extension types (Universal Tools, MCP, and future extensions).
//!
//! # Design Principles
//!
//! 1. **Single source of truth**: One `ReservedParamsConfig` type for the entire system
//! 2. **Source-agnostic**: Same config format works for runtime, env, and static sources
//! 3. **Resolution context**: Uses shared `ContextResolver` for consistent field resolution
//!
//! # Example Configuration
//!
//! ```toml
//! [reserved_parameters]
//! agent_id = { source = "runtime", field = "agent_id" }
//! api_key = { source = "env", var = "API_KEY" }
//! version = { source = "static", value = "1.0.0" }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Unified reserved parameter configuration
///
/// This is the single source of truth for reserved parameter configuration
/// across all extension types (Universal Tools, MCP, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ReservedParamsConfig {
    /// Map of parameter name to its source configuration
    #[serde(flatten)]
    pub params: HashMap<String, ParamSource>,
}

/// Source of a reserved parameter value
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum ParamSource {
    /// Injected from runtime context (`session_id`, `agent_id`, etc.)
    Runtime { field: String },
    /// Read from environment variable
    Env { var: String },
    /// Static hardcoded value
    Static { value: Value },
}

impl ReservedParamsConfig {
    /// Create an empty configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a runtime parameter
    pub fn with_runtime(mut self, name: impl Into<String>, field: impl Into<String>) -> Self {
        self.params.insert(
            name.into(),
            ParamSource::Runtime {
                field: field.into(),
            },
        );
        self
    }

    /// Add an environment variable parameter
    pub fn with_env(mut self, name: impl Into<String>, var: impl Into<String>) -> Self {
        self.params
            .insert(name.into(), ParamSource::Env { var: var.into() });
        self
    }

    /// Add a static parameter
    pub fn with_static(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.params.insert(
            name.into(),
            ParamSource::Static {
                value: value.into(),
            },
        );
        self
    }

    /// Check if configuration is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    /// Get number of configured parameters
    #[must_use]
    pub fn len(&self) -> usize {
        self.params.len()
    }

    /// Get parameter names
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.params.keys()
    }

    /// Check if a parameter is configured
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.params.contains_key(name)
    }

    /// Get a specific parameter source
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ParamSource> {
        self.params.get(name)
    }

    /// Resolve all parameters to their values
    ///
    /// # Arguments
    /// * `ctx` - Optional tool context for runtime resolution
    ///
    /// # Returns
    /// Map of parameter names to resolved values
    #[must_use]
    pub fn resolve(&self, ctx: Option<&crate::tools::ToolContext>) -> HashMap<String, Value> {
        let mut result = HashMap::new();
        for (name, source) in &self.params {
            result.insert(name.clone(), source.resolve(ctx));
        }
        result
    }

    /// Convert to JSON object with resolved values
    #[must_use]
    pub fn resolve_to_object(&self, ctx: Option<&crate::tools::ToolContext>) -> Value {
        let resolved = self.resolve(ctx);
        Value::Object(
            resolved
                .into_iter()
                .collect::<serde_json::Map<String, Value>>(),
        )
    }
}

impl ParamSource {
    /// Resolve this parameter source to a value
    ///
    /// # Arguments
    /// * `ctx` - Optional tool context for runtime field resolution
    ///
    /// # Returns
    /// The resolved value, or `Value::Null` if not available
    pub fn resolve(&self, ctx: Option<&crate::tools::ToolContext>) -> Value {
        use crate::tools::shared::context_resolver::{ContextResolver, ToolContextAdapter};

        match self {
            Self::Runtime { field } => ctx.map_or(Value::Null, |c| {
                let adapter = ToolContextAdapter::new(c);
                ContextResolver::resolve_field(&adapter, field)
            }),
            Self::Env { var } => std::env::var(var).map_or(Value::Null, Value::String),
            Self::Static { value } => value.clone(),
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
}

/// Reserved parameters service
///
/// Provides centralized reserved parameter configuration management
/// and resolution services for the Extension system.
#[derive(Debug, Default)]
pub struct ReservedParamsService;

impl ReservedParamsService {
    /// Create a new reserved parameters service
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Parse configuration from TOML/JSON string
    pub fn parse_config(
        &self,
        data: &str,
        format: ConfigFormat,
    ) -> anyhow::Result<ReservedParamsConfig> {
        match format {
            ConfigFormat::Json => {
                let config: ReservedParamsConfig = serde_json::from_str(data)?;
                Ok(config)
            }
            ConfigFormat::Toml => {
                let config: ReservedParamsConfig = toml::from_str(data)?;
                Ok(config)
            }
        }
    }
}

/// Configuration file format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Toml,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_reserved_params_config_builder() {
        let config = ReservedParamsConfig::new()
            .with_runtime("agent_id", "agent_id")
            .with_env("api_key", "API_KEY")
            .with_static("version", "1.0.0");

        assert_eq!(config.len(), 3);
        assert!(config.contains("agent_id"));
        assert!(config.contains("api_key"));
        assert!(config.contains("version"));
    }

    #[test]
    fn test_param_source_resolution() {
        // Set env var for testing
        std::env::set_var("TEST_RESERVED_PARAM", "test_value");

        let env_source = ParamSource::Env {
            var: "TEST_RESERVED_PARAM".to_string(),
        };
        assert_eq!(env_source.resolve(None), json!("test_value"));

        let static_source = ParamSource::Static {
            value: json!("hardcoded"),
        };
        assert_eq!(static_source.resolve(None), json!("hardcoded"));

        // Clean up
        std::env::remove_var("TEST_RESERVED_PARAM");
    }

    #[test]
    fn test_json_serialization() {
        let config = ReservedParamsConfig::new()
            .with_runtime("agent_id", "agent_id")
            .with_static("version", "1.0.0");

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("agent_id"));
        assert!(json.contains("runtime"));
        assert!(json.contains("version"));

        let deserialized: ReservedParamsConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_toml_serialization() {
        let config = ReservedParamsConfig::new().with_env("api_key", "API_KEY");

        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("api_key"));
        assert!(toml_str.contains("env"));
        assert!(toml_str.contains("API_KEY"));
    }

    #[test]
    fn test_empty_config() {
        let config = ReservedParamsConfig::new();
        assert!(config.is_empty());
        assert_eq!(config.len(), 0);
        assert_eq!(config.resolve(None).len(), 0);
    }

    #[test]
    fn test_service_parse_config() {
        let service = ReservedParamsService::new();

        let json_config = r#"{"agent_id":{"source":"runtime","field":"agent_id"}}"#;
        let config = service
            .parse_config(json_config, ConfigFormat::Json)
            .unwrap();

        assert!(config.contains("agent_id"));
        assert!(matches!(
            config.get("agent_id").unwrap(),
            ParamSource::Runtime { field } if field == "agent_id"
        ));
    }
}
