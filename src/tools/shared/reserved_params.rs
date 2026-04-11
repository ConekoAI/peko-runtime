//! Reserved Parameter Configuration (Compatibility Re-export)
//!
//! This module is deprecated. Use `crate::extensions::services` instead.
//!
//! This file now re-exports from the unified `extensions::services` module
//! for backward compatibility during migration.

// Re-export canonical types from extensions::services
pub use crate::extensions::services::{ParamSource as ReservedParamSource, ReservedParamsConfig};

/// Reserved parameter with optional metadata (deprecated, use `ReservedParamsConfig` directly)
#[deprecated(note = "Use ReservedParamsConfig from crate::extensions::services instead")]
pub struct ReservedParam {
    /// Source of the parameter value
    pub source: ReservedParamSource,
    /// Optional description
    pub description: Option<String>,
}

/// Collection of reserved parameters (deprecated, use `ReservedParamsConfig`)
#[deprecated(note = "Use ReservedParamsConfig from crate::extensions::services instead")]
pub type ReservedParams = std::collections::HashMap<String, ReservedParam>;

/// Resolve all reserved parameters in a collection (deprecated)
#[deprecated(note = "Use ReservedParamsConfig::resolve() instead")]
pub fn resolve_all(
    _params: &ReservedParams,
    _ctx: Option<&crate::tools::ToolContext>,
) -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Convert from old MCP format to unified format (deprecated)
#[deprecated(note = "Use ReservedParamsConfig directly")]
pub fn from_mcp_config(
    source: &str,
    field: Option<&str>,
    var: Option<&str>,
    value: Option<&str>,
) -> Option<ReservedParamSource> {
    match source {
        "runtime" => field.map(|f| ReservedParamSource::Runtime {
            field: f.to_string(),
        }),
        "env" => var.map(|v| ReservedParamSource::Env { var: v.to_string() }),
        "static" => value.map(|val| ReservedParamSource::Static {
            value: serde_json::json!(val),
        }),
        _ => None,
    }
}
