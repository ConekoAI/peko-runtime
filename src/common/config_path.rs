//! Configuration path manipulation utilities
//!
//! Provides dot-notation get/set operations on AgentConfig via JSON intermediate
//! representation, enabling generic CLI commands like:
//!   pekobot agent config get my-agent tools.enabled
//!   pekobot agent config set my-agent tools.enabled '["shell","read_file"]'

use crate::types::agent::AgentConfig;
use anyhow::{Context, Result};

/// Get a value from AgentConfig by dot-separated path.
///
/// Returns a cloned `serde_json::Value` so callers can format it as needed
/// (plain text for CLI, embedded JSON for `--json` output).
pub fn get_config_value(config: &AgentConfig, path: &str) -> Result<serde_json::Value> {
    if path.is_empty() {
        anyhow::bail!("Config path cannot be empty");
    }

    let json_value = serde_json::to_value(config)?;
    let mut current = &json_value;

    for segment in path.split('.') {
        current = match current {
            serde_json::Value::Object(map) => map.get(segment).ok_or_else(|| {
                anyhow::anyhow!("Config key '{}' not found in path '{}'", segment, path)
            })?,
            _ => {
                return Err(anyhow::anyhow!(
                    "Cannot traverse into non-object at '{}' in path '{}'",
                    segment,
                    path
                ))
            }
        };
    }

    Ok(current.clone())
}

/// Set a value on AgentConfig by dot-separated path.
///
/// The value string is parsed as JSON when possible (arrays, objects, numbers,
/// booleans, quoted strings). Otherwise it falls back to a plain string.
pub fn set_config_value(config: &mut AgentConfig, path: &str, value_str: &str) -> Result<()> {
    if path.is_empty() {
        anyhow::bail!("Config path cannot be empty");
    }

    let mut json_value = serde_json::to_value(&*config)?;
    let new_value = parse_value(value_str)?;
    let segments: Vec<&str> = path.split('.').collect();

    let mut current = &mut json_value;
    for (i, segment) in segments.iter().enumerate() {
        if i == segments.len() - 1 {
            if let serde_json::Value::Object(map) = current {
                map.insert(segment.to_string(), new_value.clone());
            } else {
                anyhow::bail!("Cannot set value on non-object at '{}'", segment);
            }
        } else {
            if let serde_json::Value::Object(map) = current {
                current = map
                    .entry(segment.to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if matches!(current, serde_json::Value::Null) {
                    *current = serde_json::json!({});
                }
            } else {
                anyhow::bail!("Cannot traverse into non-object at '{}'", segment);
            }
        }
    }

    *config = serde_json::from_value(json_value).with_context(|| {
        format!(
            "Invalid value for path '{}': '{}' cannot be converted to the expected type",
            path, value_str
        )
    })?;

    Ok(())
}

/// Parse a user-supplied value string into a JSON value.
///
/// Tries JSON parsing first so arrays, objects, numbers and booleans work
/// out of the box. Falls back to a plain string if JSON parsing fails.
/// Returns an error if the value looks like JSON but fails to parse.
fn parse_value(value_str: &str) -> Result<serde_json::Value> {
    // Check if it looks like JSON (starts with [, {, ", or is a number/bool literal)
    let looks_like_json = value_str.starts_with('[')
        || value_str.starts_with('{')
        || value_str.starts_with('"')
        || value_str.starts_with('\'')
        || value_str == "true"
        || value_str == "false"
        || value_str.parse::<f64>().is_ok();

    if looks_like_json {
        let parsed: serde_json::Value = serde_json::from_str(value_str)
            .with_context(|| format!("Invalid JSON value: '{}'", value_str))?;
        return Ok(parsed);
    }
    Ok(serde_json::Value::String(value_str.to_string()))
}

/// Format a JSON value for human-readable CLI output.
pub fn format_value(value: &serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Array(arr) => Ok(serde_json::to_string(arr)?),
        serde_json::Value::Object(obj) => Ok(serde_json::to_string_pretty(obj)?),
        other => Ok(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // get_config_value tests
    // ------------------------------------------------------------------

    #[test]
    fn test_get_simple_string() {
        let config = AgentConfig::default();
        let value = get_config_value(&config, "name").unwrap();
        assert_eq!(value, serde_json::json!("unnamed-agent"));
    }

    #[test]
    fn test_get_array() {
        let config = AgentConfig::default();
        let value = get_config_value(&config, "tools.enabled").unwrap();
        // Default whitelist is empty (secure-by-default)
        assert_eq!(value, serde_json::json!([]));
    }

    #[test]
    fn test_get_bool() {
        let config = AgentConfig::default();
        let value = get_config_value(&config, "auto_accept_trusted").unwrap();
        assert_eq!(value, serde_json::json!(false));
    }

    #[test]
    fn test_get_number() {
        let config = AgentConfig::default();
        let value = get_config_value(&config, "default_timeout_seconds").unwrap();
        assert_eq!(value, serde_json::json!(300));
    }

    #[test]
    fn test_get_missing_key() {
        let config = AgentConfig::default();
        let err = get_config_value(&config, "does.not.exist").unwrap_err();
        assert!(err.to_string().contains("does"));
    }

    #[test]
    fn test_get_empty_path() {
        let config = AgentConfig::default();
        let err = get_config_value(&config, "").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    // ------------------------------------------------------------------
    // set_config_value tests
    // ------------------------------------------------------------------

    #[test]
    fn test_set_string_value() {
        let mut config = AgentConfig::default();
        set_config_value(&mut config, "name", "my-agent").unwrap();
        assert_eq!(config.name, "my-agent");
    }

    #[test]
    fn test_set_quoted_string_value() {
        let mut config = AgentConfig::default();
        set_config_value(&mut config, "name", "\"quoted-agent\"").unwrap();
        assert_eq!(config.name, "quoted-agent");
    }

    #[test]
    fn test_set_array_value() {
        let mut config = AgentConfig::default();
        set_config_value(&mut config, "tools.enabled", r#"["shell","read_file"]"#).unwrap();
        assert_eq!(
            config.tools.as_ref().unwrap().enabled,
            vec!["shell", "read_file"]
        );
    }

    #[test]
    fn test_set_bool_value() {
        let mut config = AgentConfig::default();
        set_config_value(&mut config, "auto_accept_trusted", "true").unwrap();
        assert!(config.auto_accept_trusted);
    }

    #[test]
    fn test_set_number_value() {
        let mut config = AgentConfig::default();
        set_config_value(&mut config, "default_timeout_seconds", "600").unwrap();
        assert_eq!(config.default_timeout_seconds, 600);
    }

    #[test]
    fn test_set_invalid_type_fails() {
        let mut config = AgentConfig::default();
        let err = set_config_value(&mut config, "default_timeout_seconds", "not-a-number")
            .unwrap_err();
        assert!(err.to_string().contains("Invalid value"));
    }

    #[test]
    fn test_set_empty_path_fails() {
        let mut config = AgentConfig::default();
        let err = set_config_value(&mut config, "", "value").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    // ------------------------------------------------------------------
    // format_value tests
    // ------------------------------------------------------------------

    #[test]
    fn test_format_string() {
        assert_eq!(format_value(&serde_json::json!("hello")).unwrap(), "hello");
    }

    #[test]
    fn test_format_array() {
        let formatted = format_value(&serde_json::json!(["a", "b"])).unwrap();
        assert_eq!(formatted, r#"["a","b"]"#);
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_value(&serde_json::json!(42)).unwrap(), "42");
    }

    #[test]
    fn test_format_bool() {
        assert_eq!(format_value(&serde_json::json!(true)).unwrap(), "true");
    }
}
