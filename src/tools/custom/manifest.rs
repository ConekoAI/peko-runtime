//! Custom tool manifest parsing
//!
//! Parses `<toolname>.json` sidecar files that describe tool parameters
//! and metadata using JSON Schema draft-07 format.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, trace, warn};

/// Tool manifest schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    /// Tool name (should match filename)
    #[serde(default)]
    pub name: Option<String>,
    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema for tool parameters
    #[serde(default, rename = "parameters")]
    pub parameters: serde_json::Value,
    /// Additional metadata
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl ToolManifest {
    /// Load a manifest from a file path
    pub async fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = tokio::fs::read_to_string(path.as_ref()).await?;
        let manifest: ToolManifest = serde_json::from_str(&content)?;
        Ok(manifest)
    }

    /// Load from file synchronously
    pub fn from_file_sync(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let manifest: ToolManifest = serde_json::from_str(&content)?;
        Ok(manifest)
    }

    /// Get the tool name, falling back to a default
    pub fn name_or(&self, default: &str) -> String {
        self.name.clone().unwrap_or_else(|| default.to_string())
    }

    /// Get the description, falling back to a default
    pub fn description_or(&self, default: &str) -> String {
        self.description
            .clone()
            .unwrap_or_else(|| default.to_string())
    }

    /// Get the parameter schema, returning a minimal schema if none exists
    pub fn parameters(&self) -> serde_json::Value {
        if self.parameters.is_object() && !self.parameters.as_object().unwrap().is_empty() {
            self.parameters.clone()
        } else {
            // Return minimal schema
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
        }
    }

    /// Validate parameters against this manifest's schema
    ///
    /// Returns Ok(()) if valid, Err with message if invalid
    pub fn validate_params(&self, params: &serde_json::Value) -> anyhow::Result<()> {
        // Basic validation - check required fields
        if let Some(schema) = self.parameters.as_object() {
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                let params_obj = params.as_object().ok_or_else(|| {
                    anyhow::anyhow!("Parameters must be an object, got: {}", params)
                })?;

                for req in required {
                    if let Some(req_str) = req.as_str() {
                        if !params_obj.contains_key(req_str) {
                            return Err(anyhow::anyhow!("Missing required parameter: {}", req_str));
                        }
                    }
                }
            }
        }

        // Note: Full JSON Schema validation would require an external crate
        // For now we do basic structural validation

        Ok(())
    }
}

impl Default for ToolManifest {
    fn default() -> Self {
        Self {
            name: None,
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            extra: HashMap::new(),
        }
    }
}

/// Parse a tool manifest from JSON string
pub fn parse_manifest(json: &str) -> anyhow::Result<ToolManifest> {
    let manifest: ToolManifest = serde_json::from_str(json)?;
    Ok(manifest)
}

/// Try to load a manifest for a tool
pub async fn try_load_manifest(tools_dir: &Path, tool_name: &str) -> Option<ToolManifest> {
    // Try both normalized name and original name
    let manifest_path = tools_dir.join(format!("{}.json", tool_name));

    if manifest_path.exists() {
        trace!("Loading manifest from {:?}", manifest_path);
        match ToolManifest::from_file(&manifest_path).await {
            Ok(manifest) => {
                debug!(
                    "Loaded manifest for '{}' from {:?}",
                    tool_name, manifest_path
                );
                return Some(manifest);
            }
            Err(e) => {
                warn!("Failed to load manifest from {:?}: {}", manifest_path, e);
            }
        }
    }

    // Also try with hyphens (if tool name uses underscores)
    let hyphenated = tool_name.replace('_', "-");
    if hyphenated != tool_name {
        let manifest_path = tools_dir.join(format!("{}.json", hyphenated));
        if manifest_path.exists() {
            match ToolManifest::from_file(&manifest_path).await {
                Ok(manifest) => {
                    debug!(
                        "Loaded manifest for '{}' from {:?}",
                        tool_name, manifest_path
                    );
                    return Some(manifest);
                }
                Err(e) => {
                    warn!("Failed to load manifest: {}", e);
                }
            }
        }
    }

    None
}

/// Generate a minimal manifest for a tool without one
pub fn generate_minimal_manifest(tool_name: &str) -> ToolManifest {
    ToolManifest {
        name: Some(tool_name.to_string()),
        description: Some(format!("Custom tool: {}", tool_name)),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
        extra: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_valid_manifest() {
        let json = r#"{
            "name": "web_search",
            "description": "Search the web",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    }
                },
                "required": ["query"]
            }
        }"#;

        let manifest = parse_manifest(json).unwrap();
        assert_eq!(manifest.name, Some("web_search".to_string()));
        assert_eq!(manifest.description, Some("Search the web".to_string()));
    }

    #[test]
    fn test_parse_minimal_manifest() {
        let json = r"{}";

        let manifest = parse_manifest(json).unwrap();
        assert!(manifest.name.is_none());
        assert!(manifest.description.is_none());
    }

    #[tokio::test]
    async fn test_load_from_file() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("my_tool.json");

        std::fs::write(
            &manifest_path,
            r#"{"name": "my_tool", "description": "Test tool"}"#,
        )
        .unwrap();

        let manifest = ToolManifest::from_file(&manifest_path).await.unwrap();
        assert_eq!(manifest.name, Some("my_tool".to_string()));
        assert_eq!(manifest.description, Some("Test tool".to_string()));
    }

    #[test]
    fn test_validate_params_valid() {
        let manifest = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
            extra: HashMap::new(),
        };

        let params = serde_json::json!({"query": "test"});
        assert!(manifest.validate_params(&params).is_ok());
    }

    #[test]
    fn test_validate_params_missing_required() {
        let manifest = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "required": ["query"]
            }),
            extra: HashMap::new(),
        };

        let params = serde_json::json!({});
        assert!(manifest.validate_params(&params).is_err());
    }

    #[test]
    fn test_default_parameters() {
        let manifest = ToolManifest::default();
        let params = manifest.parameters();

        assert_eq!(params["type"], "object");
        assert!(params["properties"].as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_try_load_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path();

        // Create manifest file
        std::fs::write(
            tools_dir.join("my_tool.json"),
            r#"{"name": "my_tool", "description": "Test"}"#,
        )
        .unwrap();

        let manifest = try_load_manifest(tools_dir, "my_tool").await;
        assert!(manifest.is_some());
        assert_eq!(manifest.unwrap().name, Some("my_tool".to_string()));
    }

    #[tokio::test]
    async fn test_try_load_manifest_hyphenated() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path();

        // Create manifest with hyphens
        std::fs::write(
            tools_dir.join("my-tool.json"),
            r#"{"name": "my-tool", "description": "Test"}"#,
        )
        .unwrap();

        // Try to load with underscores
        let manifest = try_load_manifest(tools_dir, "my_tool").await;
        assert!(manifest.is_some());
    }

    #[tokio::test]
    async fn test_try_load_manifest_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path();

        let manifest = try_load_manifest(tools_dir, "nonexistent").await;
        assert!(manifest.is_none());
    }

    #[test]
    fn test_generate_minimal_manifest() {
        let manifest = generate_minimal_manifest("test_tool");

        assert_eq!(manifest.name, Some("test_tool".to_string()));
        assert!(manifest.description.is_some());
        assert!(manifest.description.as_ref().unwrap().contains("test_tool"));
        assert_eq!(manifest.parameters()["type"], "object");
    }

    #[test]
    fn test_manifest_name_or_default() {
        let manifest = ToolManifest {
            name: Some("explicit_name".to_string()),
            description: None,
            parameters: serde_json::json!({}),
            extra: HashMap::new(),
        };

        assert_eq!(manifest.name_or("fallback"), "explicit_name");

        let manifest_no_name = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({}),
            extra: HashMap::new(),
        };

        assert_eq!(manifest_no_name.name_or("fallback"), "fallback");
    }

    #[test]
    fn test_manifest_description_or_default() {
        let manifest = ToolManifest {
            name: None,
            description: Some("Explicit description".to_string()),
            parameters: serde_json::json!({}),
            extra: HashMap::new(),
        };

        assert_eq!(manifest.description_or("fallback"), "Explicit description");

        let manifest_no_desc = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({}),
            extra: HashMap::new(),
        };

        assert_eq!(manifest_no_desc.description_or("fallback"), "fallback");
    }

    #[test]
    fn test_manifest_to_json() {
        let manifest = ToolManifest {
            name: Some("test".to_string()),
            description: Some("A test tool".to_string()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "arg": { "type": "string" }
                }
            }),
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("test"));
        assert!(json.contains("A test tool"));
        assert!(json.contains("properties"));
    }

    #[test]
    fn test_parse_manifest_with_extra_fields() {
        let json = r#"{
            "name": "advanced_tool",
            "description": "A tool with extra metadata",
            "parameters": {
                "type": "object",
                "properties": {}
            },
            "author": "Test Author",
            "version": "1.0.0",
            "license": "MIT"
        }"#;

        let manifest = parse_manifest(json).unwrap();
        assert_eq!(manifest.name, Some("advanced_tool".to_string()));
        assert!(manifest.extra.contains_key("author"));
        assert!(manifest.extra.contains_key("version"));
        assert!(manifest.extra.contains_key("license"));
        assert_eq!(
            manifest.extra.get("author").unwrap().as_str(),
            Some("Test Author")
        );
    }

    #[test]
    fn test_validate_params_non_object() {
        let manifest = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "required": ["query"]
            }),
            extra: HashMap::new(),
        };

        // Non-object params should fail
        let params = serde_json::json!("string");
        assert!(manifest.validate_params(&params).is_err());

        let params = serde_json::json!(42);
        assert!(manifest.validate_params(&params).is_err());

        let params = serde_json::json!([]);
        assert!(manifest.validate_params(&params).is_err());
    }

    #[test]
    fn test_validate_params_multiple_required() {
        let manifest = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "required": ["arg1", "arg2", "arg3"]
            }),
            extra: HashMap::new(),
        };

        // Missing one required
        let params = serde_json::json!({"arg1": "a", "arg2": "b"});
        assert!(manifest.validate_params(&params).is_err());

        // All required present
        let params = serde_json::json!({"arg1": "a", "arg2": "b", "arg3": "c"});
        assert!(manifest.validate_params(&params).is_ok());

        // Extra fields allowed
        let params = serde_json::json!({"arg1": "a", "arg2": "b", "arg3": "c", "extra": "d"});
        assert!(manifest.validate_params(&params).is_ok());
    }

    #[test]
    fn test_validate_params_no_required() {
        let manifest = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "optional": { "type": "string" }
                }
            }),
            extra: HashMap::new(),
        };

        // Empty object should pass when no required fields
        let params = serde_json::json!({});
        assert!(manifest.validate_params(&params).is_ok());
    }

    #[test]
    fn test_from_file_sync() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = temp_dir.path().join("sync_tool.json");

        std::fs::write(
            &manifest_path,
            r#"{"name": "sync_tool", "description": "Sync test"}"#,
        )
        .unwrap();

        let manifest = ToolManifest::from_file_sync(&manifest_path).unwrap();
        assert_eq!(manifest.name, Some("sync_tool".to_string()));
    }

    #[test]
    fn test_parse_invalid_json() {
        let json = r"{invalid json}";
        assert!(parse_manifest(json).is_err());
    }

    #[test]
    fn test_parse_manifest_with_nested_parameters() {
        let json = r#"{
            "name": "complex_tool",
            "parameters": {
                "type": "object",
                "properties": {
                    "config": {
                        "type": "object",
                        "properties": {
                            "host": { "type": "string" },
                            "port": { "type": "integer" }
                        }
                    },
                    "items": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["config"]
            }
        }"#;

        let manifest = parse_manifest(json).unwrap();
        assert_eq!(manifest.name, Some("complex_tool".to_string()));

        // Validate with nested object
        let params = serde_json::json!({
            "config": {
                "host": "localhost",
                "port": 8080
            }
        });
        assert!(manifest.validate_params(&params).is_ok());
    }
}
