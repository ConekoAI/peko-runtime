//! MCP reserved parameter configuration and runtime/vault resolution.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Reserved parameter configuration for MCP tool injection.
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
    /// Read from the encrypted vault (RP3C).
    ///
    /// The value is resolved at execution time via this module's
    /// `resolve_param_source_with_vault` helper. A missing credential
    /// is treated as `Value::Null` at runtime; the MCP manager refuses
    /// to start a server whose vault-backed reserved param is absent.
    Vault { namespace: String, name: String },
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

    /// Add a vault-backed parameter (RP3C).
    pub fn with_vault(
        mut self,
        name: impl Into<String>,
        namespace: impl Into<String>,
        param_name: impl Into<String>,
    ) -> Self {
        self.params.insert(
            name.into(),
            ParamSource::Vault {
                namespace: namespace.into(),
                name: param_name.into(),
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
}

impl ParamSource {
    /// Get the source type as a string
    #[must_use]
    pub fn source_type(&self) -> &'static str {
        match self {
            Self::Runtime { .. } => "runtime",
            Self::Env { .. } => "env",
            Self::Static { .. } => "static",
            Self::Vault { .. } => "vault",
        }
    }
}

/// Parses reserved parameter configuration and resolves injected values.
#[derive(Debug, Default)]
pub struct ReservedParamsService;

impl ReservedParamsService {
    /// Create a new reserved parameters service
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Configuration file format
///
/// Accepted by [`ReservedParamsService::parse_config`].
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
    fn test_param_source_serde_roundtrip() {
        let env_source = ParamSource::Env {
            var: "API_KEY".to_string(),
        };
        let s = serde_json::to_string(&env_source).unwrap();
        let back: ParamSource = serde_json::from_str(&s).unwrap();
        assert_eq!(back, env_source);

        let static_source = ParamSource::Static {
            value: json!("1.0.0"),
        };
        let s = serde_json::to_string(&static_source).unwrap();
        let back: ParamSource = serde_json::from_str(&s).unwrap();
        assert_eq!(back, static_source);
    }

    #[test]
    fn test_param_source_type() {
        assert_eq!(
            ParamSource::Runtime {
                field: "x".to_string()
            }
            .source_type(),
            "runtime"
        );
        assert_eq!(
            ParamSource::Env {
                var: "X".to_string()
            }
            .source_type(),
            "env"
        );
        assert_eq!(
            ParamSource::Static { value: Value::Null }.source_type(),
            "static"
        );
        assert_eq!(
            ParamSource::Vault {
                namespace: "n".to_string(),
                name: "m".to_string()
            }
            .source_type(),
            "vault"
        );
    }
}

use crate::extensions::framework::vault::VaultAccess;
use peko_tools_core::ToolContext;
use secrecy::ExposeSecret;

/// Resolve every entry in `config.params` against `ctx` + optional `vault`.
///
/// Resolve MCP injection with attributed tool context and the host vault port.
#[must_use]
pub fn resolve_reserved_params(
    config: &ReservedParamsConfig,
    ctx: Option<&ToolContext>,
    vault: Option<&dyn VaultAccess>,
) -> HashMap<String, Value> {
    let mut result = HashMap::new();
    for (name, source) in &config.params {
        result.insert(
            name.clone(),
            resolve_param_source_with_vault(source, ctx, vault),
        );
    }
    result
}

/// Resolve a single `ParamSource` against `ctx` + optional `vault`.
///
/// Phase 7 host-side helper. Mirrors the pre-Phase-7
/// `ParamSource::resolve_with_vault`.
pub fn resolve_param_source_with_vault(
    source: &ParamSource,
    ctx: Option<&ToolContext>,
    vault: Option<&dyn VaultAccess>,
) -> Value {
    use peko_tools_core::context_source::ContextResolver;
    use peko_tools_core::ToolContextAdapter;

    match source {
        ParamSource::Runtime { field } => ctx.map_or(Value::Null, |c| {
            let adapter = ToolContextAdapter::new(c);
            ContextResolver::resolve_field(&adapter, field)
        }),
        ParamSource::Env { var } => std::env::var(var).map_or(Value::Null, Value::String),
        ParamSource::Static { value } => value.clone(),
        ParamSource::Vault { namespace, name } => vault
            .and_then(|v| v.get_material_for(namespace, name).ok().flatten())
            .map(|s| Value::String(s.expose_secret().to_string()))
            .unwrap_or(Value::Null),
    }
}

/// Parse a `ReservedParamsConfig` from a TOML/JSON string.
///
/// Parse the MCP injection configuration from either supported format.
pub fn parse_config(data: &str, format: ConfigFormat) -> anyhow::Result<ReservedParamsConfig> {
    match format {
        ConfigFormat::Json => Ok(serde_json::from_str(data)?),
        ConfigFormat::Toml => Ok(toml::from_str(data)?),
    }
}

#[cfg(test)]
mod resolution_tests {
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
        let value = resolve_param_source_with_vault(&env_source, None, None);
        assert_eq!(value, json!("test_value"));

        std::env::remove_var("TEST_RESERVED_PARAM");
    }
}
