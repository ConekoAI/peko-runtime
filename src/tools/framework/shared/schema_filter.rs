//! Schema Filter Utilities
//!
//! Provides consistent schema manipulation for hiding reserved parameters
//! from LLM visibility while maintaining validation capabilities.

use serde_json::Value;
use std::collections::HashSet;

/// Filter reserved parameters from a JSON schema
///
/// This creates a modified schema that hides reserved parameters from the LLM,
/// preventing them from being provided by the user while allowing them to be
/// injected at runtime.
///
/// # Arguments
/// * `schema` - The original JSON schema (should be object type)
/// * `reserved` - Set of parameter names to filter out
///
/// # Returns
/// A new schema with reserved parameters removed from both `properties` and `required`
///
/// # Example
/// ```
/// use serde_json::json;
/// use std::collections::HashSet;
/// use pekobot::tools::framework::shared::filter_reserved_params;
///
/// let schema = json!({
///     "type": "object",
///     "properties": {
///         "query": {"type": "string"},
///         "agent_id": {"type": "string"}
///     },
///     "required": ["query", "agent_id"]
/// });
///
/// let mut reserved = HashSet::new();
/// reserved.insert("agent_id".to_string());
///
/// let filtered = filter_reserved_params(&schema, &reserved);
/// // filtered no longer contains agent_id
/// ```
#[must_use]
pub fn filter_reserved_params(schema: &Value, reserved: &HashSet<String>) -> Value {
    if reserved.is_empty() {
        return schema.clone();
    }

    let mut filtered = schema.clone();

    // Remove reserved params from properties
    if let Some(properties) = filtered.get_mut("properties") {
        if let Some(props_obj) = properties.as_object_mut() {
            for key in reserved {
                props_obj.remove(key);
            }
        }
    }

    // Remove reserved params from required array
    if let Some(required) = filtered.get_mut("required") {
        if let Some(req_array) = required.as_array_mut() {
            req_array.retain(|v| v.as_str().is_none_or(|s| !reserved.contains(s)));
        }
    }

    filtered
}

/// Filter reserved parameters from a JSON schema using a slice
///
/// Convenience wrapper for `filter_reserved_params` that accepts a slice.
#[must_use]
pub fn filter_reserved_params_slice(schema: &Value, reserved: &[String]) -> Value {
    let set: HashSet<String> = reserved.iter().cloned().collect();
    filter_reserved_params(schema, &set)
}

/// Create a schema that only shows the exposed (non-reserved) parameters
///
/// This is a convenience function that combines filtering with validation.
pub fn create_exposed_schema(
    full_schema: &Value,
    reserved: &HashSet<String>,
) -> Result<Value, SchemaFilterError> {
    // Validate that the schema is an object type
    let schema_type = full_schema.get("type").and_then(|t| t.as_str());
    if schema_type != Some("object") {
        return Err(SchemaFilterError::InvalidSchemaType(
            schema_type.unwrap_or("missing").to_string(),
        ));
    }

    let filtered = filter_reserved_params(full_schema, reserved);

    // Security validation: ensure no reserved params leaked through
    validate_no_leak(&filtered, reserved)?;

    Ok(filtered)
}

/// Errors that can occur during schema filtering
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaFilterError {
    InvalidSchemaType(String),
    ReservedParamLeak(String),
}

impl std::fmt::Display for SchemaFilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaFilterError::InvalidSchemaType(t) => {
                write!(f, "Schema must be type 'object', got '{t}'")
            }
            SchemaFilterError::ReservedParamLeak(param) => {
                write!(
                    f,
                    "Security error: reserved parameter '{param}' leaked to LLM schema"
                )
            }
        }
    }
}

impl std::error::Error for SchemaFilterError {}

/// Validate that no reserved parameters exist in the filtered schema
///
/// This is a security check to ensure reserved params are never visible to the LLM.
fn validate_no_leak(filtered: &Value, reserved: &HashSet<String>) -> Result<(), SchemaFilterError> {
    if let Some(properties) = filtered.get("properties") {
        if let Some(props_obj) = properties.as_object() {
            for key in reserved {
                if props_obj.contains_key(key) {
                    return Err(SchemaFilterError::ReservedParamLeak(key.clone()));
                }
            }
        }
    }

    if let Some(required) = filtered.get("required") {
        if let Some(req_array) = required.as_array() {
            for item in req_array.iter().filter_map(|v| v.as_str()) {
                if reserved.contains(item) {
                    return Err(SchemaFilterError::ReservedParamLeak(item.to_string()));
                }
            }
        }
    }

    Ok(())
}

/// Get the list of parameter names from a schema
#[must_use]
pub fn get_parameter_names(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Check if a schema contains any reserved parameters
#[must_use]
pub fn contains_reserved_params(schema: &Value, reserved: &HashSet<String>) -> bool {
    if let Some(properties) = schema.get("properties") {
        if let Some(props_obj) = properties.as_object() {
            return reserved.iter().any(|key| props_obj.contains_key(key));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_reserved_set() -> HashSet<String> {
        let mut set = HashSet::new();
        set.insert("agent_id".to_string());
        set.insert("session_id".to_string());
        set
    }

    #[test]
    fn test_filter_removes_from_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "agent_id": {"type": "string"},
                "session_id": {"type": "string"},
                "count": {"type": "integer"}
            }
        });

        let reserved = test_reserved_set();
        let filtered = filter_reserved_params(&schema, &reserved);

        let props = filtered["properties"].as_object().unwrap();
        assert!(props.contains_key("query"));
        assert!(props.contains_key("count"));
        assert!(!props.contains_key("agent_id"));
        assert!(!props.contains_key("session_id"));
    }

    #[test]
    fn test_filter_removes_from_required() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "agent_id": {"type": "string"}
            },
            "required": ["query", "agent_id"]
        });

        let reserved = test_reserved_set();
        let filtered = filter_reserved_params(&schema, &reserved);

        let required = filtered["required"].as_array().unwrap();
        assert!(required.contains(&json!("query")));
        assert!(!required.contains(&json!("agent_id")));
    }

    #[test]
    fn test_filter_empty_reserved() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            }
        });

        let reserved: HashSet<String> = HashSet::new();
        let filtered = filter_reserved_params(&schema, &reserved);

        assert_eq!(filtered, schema);
    }

    #[test]
    fn test_filter_with_slice() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "agent_id": {"type": "string"}
            }
        });

        let reserved = vec!["agent_id".to_string()];
        let filtered = filter_reserved_params_slice(&schema, &reserved);

        let props = filtered["properties"].as_object().unwrap();
        assert!(!props.contains_key("agent_id"));
        assert!(props.contains_key("query"));
    }

    #[test]
    fn test_validate_no_leak_success() {
        let filtered = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            }
        });

        let mut reserved = HashSet::new();
        reserved.insert("agent_id".to_string());

        assert!(validate_no_leak(&filtered, &reserved).is_ok());
    }

    #[test]
    fn test_validate_no_leak_failure() {
        let filtered = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "agent_id": {"type": "string"}  // Should have been filtered
            }
        });

        let mut reserved = HashSet::new();
        reserved.insert("agent_id".to_string());

        let result = validate_no_leak(&filtered, &reserved);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SchemaFilterError::ReservedParamLeak(_)
        ));
    }

    #[test]
    fn test_create_exposed_schema_invalid_type() {
        let schema = json!({
            "type": "array",
            "items": {"type": "string"}
        });

        let reserved = test_reserved_set();
        let result = create_exposed_schema(&schema, &reserved);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SchemaFilterError::InvalidSchemaType(_)
        ));
    }

    #[test]
    fn test_get_parameter_names() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "integer"},
                "c": {"type": "boolean"}
            }
        });

        let names = get_parameter_names(&schema);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"c".to_string()));
    }

    #[test]
    fn test_contains_reserved_params() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "agent_id": {"type": "string"}
            }
        });

        let mut reserved = HashSet::new();
        reserved.insert("agent_id".to_string());

        assert!(contains_reserved_params(&schema, &reserved));

        let mut other = HashSet::new();
        other.insert("session_id".to_string());
        assert!(!contains_reserved_params(&schema, &other));
    }

    #[test]
    fn test_security_leak_in_required() {
        // Simulate a bug where filtering failed to remove from required
        let filtered = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": ["query", "agent_id"]  // agent_id leaked!
        });

        let mut reserved = HashSet::new();
        reserved.insert("agent_id".to_string());

        let result = validate_no_leak(&filtered, &reserved);
        assert!(result.is_err());
    }
}
