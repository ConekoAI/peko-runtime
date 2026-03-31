//! Validation Utilities
//!
//! Security and consistency validation for tool implementations.
//! Ensures reserved parameters never leak to LLMs and tool configurations
//! are valid.

use serde_json::Value;
use std::collections::HashSet;

/// Errors that can occur during validation
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// A reserved parameter was found in the LLM-visible schema
    ReservedParamLeak { param: String, location: String },
    /// An invalid parameter was provided by the user
    InvalidUserParam { param: String, reason: String },
    /// Schema type mismatch
    SchemaTypeMismatch { expected: String, actual: String },
    /// Missing required parameter
    MissingRequiredParam { param: String },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::ReservedParamLeak { param, location } => {
                write!(
                    f,
                    "Security violation: reserved parameter '{}' found in {}. \
                     Reserved parameters must be injected at runtime, not provided by LLM.",
                    param, location
                )
            }
            ValidationError::InvalidUserParam { param, reason } => {
                write!(f, "Invalid parameter '{}': {}", param, reason)
            }
            ValidationError::SchemaTypeMismatch { expected, actual } => {
                write!(f, "Schema type mismatch: expected '{}', got '{}'", expected, actual)
            }
            ValidationError::MissingRequiredParam { param } => {
                write!(f, "Missing required parameter: '{}'", param)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validate that no reserved parameters are present in user-provided parameters
///
/// # Security Note
/// This prevents LLMs from providing values for parameters that should be
/// injected at runtime (like agent_id, session_id).
pub fn validate_no_reserved_in_user_params(
    user_params: &Value,
    reserved: &HashSet<String>,
) -> Result<(), ValidationError> {
    let obj = user_params
        .as_object()
        .ok_or_else(|| ValidationError::SchemaTypeMismatch {
            expected: "object".to_string(),
            actual: user_params.to_string(),
        })?;

    for key in obj.keys() {
        if reserved.contains(key) {
            return Err(ValidationError::InvalidUserParam {
                param: key.clone(),
                reason: format!(
                    "Parameter '{}' is reserved and should not be provided by user. \
                     It will be injected at runtime.",
                    key
                ),
            });
        }
    }

    Ok(())
}

/// Validate that no reserved parameters leak to the LLM-visible schema
///
/// # Security Note
/// This ensures that reserved parameters are never visible to the LLM,
/// preventing confusion and potential security issues.
pub fn validate_no_reserved_params_leak(
    exposed_schema: &Value,
    reserved: &HashSet<String>,
) -> Result<(), ValidationError> {
    // Check properties
    if let Some(properties) = exposed_schema.get("properties") {
        if let Some(props_obj) = properties.as_object() {
            for key in reserved.iter() {
                if props_obj.contains_key(key) {
                    return Err(ValidationError::ReservedParamLeak {
                        param: key.clone(),
                        location: "schema.properties".to_string(),
                    });
                }
            }
        }
    }

    // Check required array
    if let Some(required) = exposed_schema.get("required") {
        if let Some(req_array) = required.as_array() {
            for item in req_array.iter().filter_map(|v| v.as_str()) {
                if reserved.contains(item) {
                    return Err(ValidationError::ReservedParamLeak {
                        param: item.to_string(),
                        location: "schema.required".to_string(),
                    });
                }
            }
        }
    }

    // Check allOf/anyOf/oneOf for nested schemas
    for key in &["allOf", "anyOf", "oneOf"] {
        if let Some(subschemas) = exposed_schema.get(key) {
            if let Some(array) = subschemas.as_array() {
                for subschema in array {
                    validate_no_reserved_params_leak(subschema, reserved)?;
                }
            }
        }
    }

    // Check nested properties in additionalProperties if it's an object
    if let Some(additional) = exposed_schema.get("additionalProperties") {
        if additional.is_object() {
            validate_no_reserved_params_leak(additional, reserved)?;
        }
    }

    // Check items for array types
    if let Some(items) = exposed_schema.get("items") {
        if items.is_object() {
            validate_no_reserved_params_leak(items, reserved)?;
        }
    }

    Ok(())
}

/// Validate that all required parameters are present
pub fn validate_required_params(
    params: &Value,
    schema: &Value,
) -> Result<(), ValidationError> {
    let obj = params.as_object().ok_or_else(|| ValidationError::SchemaTypeMismatch {
        expected: "object".to_string(),
        actual: params.to_string(),
    })?;

    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for req in required.iter().filter_map(|v| v.as_str()) {
            if !obj.contains_key(req) {
                return Err(ValidationError::MissingRequiredParam {
                    param: req.to_string(),
                });
            }
        }
    }

    Ok(())
}

/// Validate tool result format
///
/// Ensures the result follows the expected ExecuteResult format or
/// is a valid plain object.
pub fn validate_tool_result(result: &Value) -> Result<ToolResultFormat, ValidationError> {
    // Check if it follows ExecuteResult format
    if let Some(obj) = result.as_object() {
        if obj.contains_key("success") {
            let success = obj["success"].as_bool().ok_or_else(|| ValidationError::InvalidUserParam {
                param: "success".to_string(),
                reason: "Must be a boolean".to_string(),
            })?;

            if success {
                // Success result should have data field or be treated as data itself
                return Ok(ToolResultFormat::ExecuteResult { has_data: obj.contains_key("data") });
            } else {
                // Error result should have error field
                if !obj.contains_key("error") {
                    return Err(ValidationError::InvalidUserParam {
                        param: "error".to_string(),
                        reason: "Error result must contain 'error' field".to_string(),
                    });
                }
                return Ok(ToolResultFormat::ExecuteResult { has_data: false });
            }
        }
    }

    // Plain object result
    Ok(ToolResultFormat::PlainObject)
}

/// Represents the format of a tool result
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolResultFormat {
    /// Standard ExecuteResult format with success/data/error fields
    ExecuteResult { has_data: bool },
    /// Plain object result (treated as data payload)
    PlainObject,
}

/// Comprehensive validation result
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<String>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn add_error(&mut self, error: ValidationError) {
        self.errors.push(error);
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }
}

/// Builder for validation checks
pub struct ValidationBuilder {
    reserved: HashSet<String>,
    schema: Option<Value>,
}

impl ValidationBuilder {
    pub fn new() -> Self {
        Self {
            reserved: HashSet::new(),
            schema: None,
        }
    }

    pub fn with_reserved(mut self, params: &[String]) -> Self {
        self.reserved = params.iter().cloned().collect();
        self
    }

    pub fn with_schema(mut self, schema: Value) -> Self {
        self.schema = Some(schema);
        self
    }

    pub fn validate_user_params(&self, params: &Value) -> Result<(), ValidationError> {
        validate_no_reserved_in_user_params(params, &self.reserved)?;
        
        if let Some(ref schema) = self.schema {
            validate_required_params(params, schema)?;
        }
        
        Ok(())
    }

    pub fn validate_exposed_schema(&self, schema: &Value) -> Result<(), ValidationError> {
        validate_no_reserved_params_leak(schema, &self.reserved)
    }
}

impl Default for ValidationBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_reserved() -> HashSet<String> {
        let mut set = HashSet::new();
        set.insert("agent_id".to_string());
        set.insert("session_id".to_string());
        set
    }

    #[test]
    fn test_validate_no_reserved_in_user_params_ok() {
        let params = json!({"query": "test", "count": 5});
        let reserved = test_reserved();
        
        assert!(validate_no_reserved_in_user_params(&params, &reserved).is_ok());
    }

    #[test]
    fn test_validate_no_reserved_in_user_params_fail() {
        let params = json!({"query": "test", "agent_id": "hacked"});
        let reserved = test_reserved();
        
        let result = validate_no_reserved_in_user_params(&params, &reserved);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::InvalidUserParam { param, .. } if param == "agent_id"
        ));
    }

    #[test]
    fn test_validate_no_reserved_params_leak_ok() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": ["query"]
        });
        let reserved = test_reserved();
        
        assert!(validate_no_reserved_params_leak(&schema, &reserved).is_ok());
    }

    #[test]
    fn test_validate_no_reserved_params_leak_in_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "agent_id": {"type": "string"}  // Leaked!
            }
        });
        let reserved = test_reserved();
        
        let result = validate_no_reserved_params_leak(&schema, &reserved);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::ReservedParamLeak { param, .. } if param == "agent_id"
        ));
    }

    #[test]
    fn test_validate_no_reserved_params_leak_in_required() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": ["query", "session_id"]  // Leaked!
        });
        let reserved = test_reserved();
        
        let result = validate_no_reserved_params_leak(&schema, &reserved);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::ReservedParamLeak { param, .. } if param == "session_id"
        ));
    }

    #[test]
    fn test_validate_required_params_ok() {
        let params = json!({"query": "test", "count": 5});
        let schema = json!({
            "type": "object",
            "required": ["query"]
        });
        
        assert!(validate_required_params(&params, &schema).is_ok());
    }

    #[test]
    fn test_validate_required_params_missing() {
        let params = json!({"count": 5});
        let schema = json!({
            "type": "object",
            "required": ["query"]
        });
        
        let result = validate_required_params(&params, &schema);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::MissingRequiredParam { param } if param == "query"
        ));
    }

    #[test]
    fn test_validate_tool_result_execute_result_success() {
        let result = json!({
            "success": true,
            "data": {"value": 42}
        });
        
        let format = validate_tool_result(&result).unwrap();
        assert!(matches!(
            format,
            ToolResultFormat::ExecuteResult { has_data: true }
        ));
    }

    #[test]
    fn test_validate_tool_result_execute_result_error() {
        let result = json!({
            "success": false,
            "error": "Something went wrong"
        });
        
        let format = validate_tool_result(&result).unwrap();
        assert!(matches!(
            format,
            ToolResultFormat::ExecuteResult { has_data: false }
        ));
    }

    #[test]
    fn test_validate_tool_result_plain_object() {
        let result = json!({"value": 42, "message": "hello"});
        
        let format = validate_tool_result(&result).unwrap();
        assert_eq!(format, ToolResultFormat::PlainObject);
    }

    #[test]
    fn test_validate_tool_result_error_missing_error_field() {
        let result = json!({
            "success": false
            // Missing "error" field
        });
        
        let result = validate_tool_result(&result);
        assert!(result.is_err());
    }

    #[test]
    fn test_validation_builder() {
        let validator = ValidationBuilder::new()
            .with_reserved(&["agent_id".to_string(), "session_id".to_string()]);

        let params = json!({"query": "test"});
        assert!(validator.validate_user_params(&params).is_ok());

        let bad_params = json!({"agent_id": "hacked"});
        assert!(validator.validate_user_params(&bad_params).is_err());
    }

    #[test]
    fn test_nested_schema_validation() {
        // Test that reserved params in nested allOf are caught
        let schema = json!({
            "type": "object",
            "allOf": [
                {
                    "type": "object",
                    "properties": {
                        "agent_id": {"type": "string"}  // Leaked in nested schema!
                    }
                }
            ]
        });
        let reserved = test_reserved();
        
        let result = validate_no_reserved_params_leak(&schema, &reserved);
        assert!(result.is_err());
    }
}
