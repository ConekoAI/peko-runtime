//! Configuration path manipulation utilities
//!
//! Provides dot-notation get/set operations on `AgentConfig` via JSON intermediate
//! representation, enabling generic CLI commands like:
//!   peko agent config get my-agent tools.enabled
//!   peko agent config set my-agent tools.enabled '["shell","read_file"]'

use crate::types::agent::AgentConfig;
use anyhow::{Context, Result};

/// Get a value from `AgentConfig` by dot-separated path.
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
                anyhow::anyhow!("Config key '{segment}' not found in path '{path}'")
            })?,
            _ => {
                return Err(anyhow::anyhow!(
                    "Cannot traverse into non-object at '{segment}' in path '{path}'"
                ))
            }
        };
    }

    Ok(current.clone())
}

/// Set a value on `AgentConfig` by dot-separated path.
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
                anyhow::bail!("Cannot set value on non-object at '{segment}'");
            }
        } else if let serde_json::Value::Object(map) = current {
            current = map
                .entry(segment.to_string())
                .or_insert_with(|| serde_json::json!({}));
            if matches!(current, serde_json::Value::Null) {
                *current = serde_json::json!({});
            }
        } else {
            anyhow::bail!("Cannot traverse into non-object at '{segment}'");
        }
    }

    *config = serde_json::from_value(json_value).with_context(|| {
        format!(
            "Invalid value for path '{path}': '{value_str}' cannot be converted to the expected type"
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
            .with_context(|| format!("Invalid JSON value: '{value_str}'"))?;
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

// ============================================================================
// Generic TOML value operations (for global config CLI)
// ============================================================================

/// Get a value from a `toml::Value` by dot-separated path.
///
/// Returns a cloned `toml::Value` so callers can format it as needed.
pub fn get_toml_value(value: &toml::Value, path: &str) -> Result<toml::Value> {
    if path.is_empty() {
        anyhow::bail!("Config path cannot be empty");
    }

    let mut current = value;

    for segment in path.split('.') {
        current = match current {
            toml::Value::Table(map) => map.get(segment).ok_or_else(|| {
                anyhow::anyhow!("Config key '{segment}' not found in path '{path}'")
            })?,
            _ => {
                return Err(anyhow::anyhow!(
                    "Cannot traverse into non-table at '{segment}' in path '{path}'"
                ))
            }
        };
    }

    Ok(current.clone())
}

/// Set a value on a `toml::Value` by dot-separated path.
///
/// The value string is parsed as JSON when possible (arrays, objects, numbers,
/// booleans, quoted strings). Otherwise it falls back to a plain string.
/// The modified TOML value is returned.
pub fn set_toml_value(mut value: toml::Value, path: &str, value_str: &str) -> Result<toml::Value> {
    if path.is_empty() {
        anyhow::bail!("Config path cannot be empty");
    }

    let new_value = json_to_toml(parse_value(value_str)?)?;
    let segments: Vec<&str> = path.split('.').collect();

    // Use a recursive helper to avoid borrow checker issues with &mut
    fn set_at_path(
        value: &mut toml::Value,
        segments: &[&str],
        new_value: toml::Value,
    ) -> Result<()> {
        if segments.is_empty() {
            return Ok(());
        }
        let segment = segments[0];
        if segments.len() == 1 {
            if let toml::Value::Table(map) = value {
                map.insert(segment.to_string(), new_value);
                Ok(())
            } else {
                anyhow::bail!("Cannot set value on non-table at '{segment}'")
            }
        } else if let toml::Value::Table(map) = value {
            let next = map
                .entry(segment.to_string())
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
            if !matches!(next, toml::Value::Table(_)) {
                *next = toml::Value::Table(toml::map::Map::new());
            }
            set_at_path(next, &segments[1..], new_value)
        } else {
            anyhow::bail!("Cannot traverse into non-table at '{segment}'")
        }
    }

    set_at_path(&mut value, &segments, new_value)?;
    Ok(value)
}

/// Convert a `serde_json::Value` into a `toml::Value`.
fn json_to_toml(json: serde_json::Value) -> Result<toml::Value> {
    match json {
        serde_json::Value::Null => anyhow::bail!("Null values are not supported in TOML"),
        serde_json::Value::Bool(b) => Ok(toml::Value::Boolean(b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                anyhow::bail!("Unsupported number format: {n}")
            }
        }
        serde_json::Value::String(s) => Ok(toml::Value::String(s)),
        serde_json::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                out.push(json_to_toml(v)?);
            }
            Ok(toml::Value::Array(out))
        }
        serde_json::Value::Object(obj) => {
            let mut map = toml::map::Map::with_capacity(obj.len());
            for (k, v) in obj {
                map.insert(k, json_to_toml(v)?);
            }
            Ok(toml::Value::Table(map))
        }
    }
}

/// Format a `toml::Value` for human-readable CLI output.
pub fn format_toml_value(value: &toml::Value) -> Result<String> {
    match value {
        toml::Value::String(s) => Ok(s.clone()),
        toml::Value::Array(arr) => {
            // Serialize array elements as JSON for consistent output
            // (toml::to_string doesn't support arrays as root documents)
            let json_arr: Vec<serde_json::Value> = arr
                .iter()
                .map(|v| match v {
                    toml::Value::String(s) => serde_json::Value::String(s.clone()),
                    toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
                    toml::Value::Float(f) => serde_json::json!(f),
                    toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
                    toml::Value::Array(a) => {
                        serde_json::from_str(&format_toml_value(&toml::Value::Array(a.clone())).unwrap_or_default())
                            .unwrap_or_default()
                    }
                    toml::Value::Table(t) => serde_json::from_str(&toml::to_string_pretty(t).unwrap_or_default())
                        .unwrap_or_default(),
                    toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
                })
                .collect();
            Ok(serde_json::to_string(&json_arr)?)
        }
        toml::Value::Table(obj) => Ok(toml::to_string_pretty(obj)?.trim().to_string()),
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
        let value = get_config_value(&config, "extensions.enabled").unwrap();
        // Default whitelist enables common built-in tools
        assert!(value.is_array());
        let arr = value.as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(arr.contains(&serde_json::json!("builtin:tool:shell")));
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
        set_config_value(
            &mut config,
            "extensions.enabled",
            r#"["shell","read_file"]"#,
        )
        .unwrap();
        assert_eq!(
            config.extensions.as_ref().unwrap().enabled,
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
        let err =
            set_config_value(&mut config, "default_timeout_seconds", "not-a-number").unwrap_err();
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

    // ------------------------------------------------------------------
    // get_toml_value tests
    // ------------------------------------------------------------------

    #[test]
    fn test_get_toml_simple_string() {
        let config = toml::Value::Table(toml::toml! {
            name = "test"
        });
        let value = get_toml_value(&config, "name").unwrap();
        assert_eq!(value, toml::Value::String("test".to_string()));
    }

    #[test]
    fn test_get_toml_nested() {
        let config = toml::Value::Table(toml::toml! {
            [daemon]
            bind_address = "127.0.0.1:11435"
        });
        let value = get_toml_value(&config, "daemon.bind_address").unwrap();
        assert_eq!(value, toml::Value::String("127.0.0.1:11435".to_string()));
    }

    #[test]
    fn test_get_toml_missing_key() {
        let config = toml::Value::Table(toml::toml! {
            name = "test"
        });
        let err = get_toml_value(&config, "does.not.exist").unwrap_err();
        assert!(err.to_string().contains("does"));
    }

    #[test]
    fn test_get_toml_empty_path() {
        let config = toml::Value::Table(toml::toml! {
            name = "test"
        });
        let err = get_toml_value(&config, "").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    // ------------------------------------------------------------------
    // set_toml_value tests
    // ------------------------------------------------------------------

    #[test]
    fn test_set_toml_string_value() {
        let config = toml::Value::Table(toml::toml! {
            name = "old"
        });
        let updated = set_toml_value(config, "name", "new").unwrap();
        assert_eq!(
            updated.get("name").unwrap(),
            &toml::Value::String("new".to_string())
        );
    }

    #[test]
    fn test_set_toml_nested_value() {
        let config = toml::Value::Table(toml::toml! {
            [daemon]
            bind_address = "127.0.0.1:11435"
        });
        let updated = set_toml_value(config, "daemon.bind_address", "0.0.0.0:8080").unwrap();
        assert_eq!(
            updated.get("daemon").unwrap().get("bind_address").unwrap(),
            &toml::Value::String("0.0.0.0:8080".to_string())
        );
    }

    #[test]
    fn test_set_toml_creates_missing_table() {
        let config = toml::Value::Table(toml::toml! {
            name = "test"
        });
        let updated = set_toml_value(config, "defaults.provider", "kimi").unwrap();
        assert_eq!(
            updated.get("defaults").unwrap().get("provider").unwrap(),
            &toml::Value::String("kimi".to_string())
        );
    }

    #[test]
    fn test_set_toml_bool_value() {
        let config = toml::Value::Table(toml::toml! {
            debug = false
        });
        let updated = set_toml_value(config, "debug", "true").unwrap();
        assert_eq!(updated.get("debug").unwrap(), &toml::Value::Boolean(true));
    }

    #[test]
    fn test_set_toml_number_value() {
        let config = toml::Value::Table(toml::toml! {
            port = 8080
        });
        let updated = set_toml_value(config, "port", "9090").unwrap();
        assert_eq!(updated.get("port").unwrap(), &toml::Value::Integer(9090));
    }

    #[test]
    fn test_set_toml_array_value() {
        let config = toml::Value::Table(toml::toml! {
            items = ["a"]
        });
        let updated = set_toml_value(config, "items", r#"["a", "b"]"#).unwrap();
        assert_eq!(
            updated.get("items").unwrap(),
            &toml::Value::Array(vec![
                toml::Value::String("a".to_string()),
                toml::Value::String("b".to_string()),
            ])
        );
    }

    #[test]
    fn test_set_toml_empty_path_fails() {
        let config = toml::Value::Table(toml::toml! {
            name = "test"
        });
        let err = set_toml_value(config, "", "value").unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    // ------------------------------------------------------------------
    // format_toml_value tests
    // ------------------------------------------------------------------

    #[test]
    fn test_format_toml_string() {
        assert_eq!(
            format_toml_value(&toml::Value::String("hello".to_string())).unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_format_toml_number() {
        assert_eq!(format_toml_value(&toml::Value::Integer(42)).unwrap(), "42");
    }

    #[test]
    fn test_format_toml_bool() {
        assert_eq!(
            format_toml_value(&toml::Value::Boolean(true)).unwrap(),
            "true"
        );
    }
}
