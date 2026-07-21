//! Reserved-parameter configuration as pure data
//!
//! The data shape (struct + enum + builders + serde) lives here so
//! `ToolMetadata::reserved_params` (a contract type) can be expressed
//! without coupling the API crate to the framework host's resolution
//! services.
//!
//! Resolution methods that need a `ToolContext` and a `Vault` (both
//! root-only types) live in the framework host as free functions:
//! `resolve_reserved_params` and `resolve_param_source_with_vault` in
//! `src/extensions/framework/services/reserved_params.rs`. They are
//! not part of the contract surface; they are an implementation detail
//! of how the host wires the data type into the rest of the system.

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
    /// Read from the encrypted vault (RP3C).
    ///
    /// The value is resolved at execution time via the framework host's
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

/// Reserved parameters service stub
///
/// The host crate (`src/extensions/framework/services/reserved_params.rs`)
/// re-exports this for backward compatibility; the actual parse helpers
/// stay in the host because they pull in `serde_json` + `toml` from the
/// host's dependency set.
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
/// The data type lives in the API crate so callers can name the format
/// in cross-crate signatures; the host crate provides the actual
/// parse helpers (`ReservedParamsService::parse_config`) because they
/// need the `toml` parser from the host's dependency set.
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
