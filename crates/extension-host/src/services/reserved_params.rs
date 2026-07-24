//! Reserved-parameter resolution helpers
//!
//! Phase 7 split this module into two parts:
//!
//! - The **data types** (`ReservedParamsConfig`, `ParamSource`,
//!   `ReservedParamsService`, `ConfigFormat`) live in the
//!   `peko-extension-api` crate as pure data with serde + builders +
//!   the format enum. The framework host re-exports them from there
//!   through this module's public surface so
//!   `peko::extensions::framework::services::reserved_params::*` paths
//!   keep working unchanged.
//! - The **resolution helpers** (`resolve_reserved_params`,
//!   `resolve_param_source_with_vault`) need a `ToolContext` and a
//!   `Vault`, both of which are root-only types. They live in this
//!   file as free functions so the API crate can stay independent of
//!   the engine and tool-execution crates.
//!
//! # Backward-compat method shape
//!
//! Pre-Phase-7 callers used `config.resolve(ctx)` and
//! `source.resolve_with_vault(ctx, vault)`. Those were inherent
//! methods on the data types; the orphan rule forbids adding an
//! inherent `impl` in this crate for a type that lives in
//! `peko-extension-api`. The free functions below replace the methods
//! at the 5 call sites (`extensions/universal/protocol/adapter.rs`,
//! `extensions/mcp/runtime/injectable_proxy.rs`, and the historical
//! internal sites in this file).

use crate::vault::VaultAccess;
use peko_tools_core::ToolContext;
use secrecy::ExposeSecret;
use serde_json::Value;
use std::collections::HashMap;

// Re-export the data types at this path so existing call sites that
// say `crate::extensions::framework::services::reserved_params::ReservedParamsConfig`
// keep resolving.
pub use peko_extension_api::reserved_params::{
    ConfigFormat, ParamSource, ReservedParamsConfig, ReservedParamsService,
};

/// Resolve every entry in `config.params` against `ctx` + optional `vault`.
///
/// Phase 7 host-side helper. The data type lives in
/// `peko-extension-api`; this function needs `ToolContext` +
/// `VaultAccess`, both of which the framework crate owns.
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
/// Host-only because it pulls in `toml` from the host's dependency
/// set. The `ReservedParamsService` data type lives in
/// `peko-extension-api`, but the parse helpers stay in the host so
/// the API crate doesn't have to depend on `toml`.
pub fn parse_config(data: &str, format: ConfigFormat) -> anyhow::Result<ReservedParamsConfig> {
    match format {
        ConfigFormat::Json => Ok(serde_json::from_str(data)?),
        ConfigFormat::Toml => Ok(toml::from_str(data)?),
    }
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
        let value = resolve_param_source_with_vault(&env_source, None, None);
        assert_eq!(value, json!("test_value"));

        std::env::remove_var("TEST_RESERVED_PARAM");
    }
}
