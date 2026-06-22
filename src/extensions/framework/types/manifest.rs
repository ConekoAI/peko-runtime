//! Extension manifest types

use crate::extensions::framework::types::ExtensionId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A declared dependency on another extension or MCP server
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionDependency {
    /// Package reference (e.g., "pekohub.com/extensions/docker-skill", "mcp::filesystem")
    pub package: String,
    /// Optional version constraint (e.g., ">=1.0.0", "^2.0")
    pub version: Option<String>,
    /// Optional: marked as required vs optional
    #[serde(default = "default_required")]
    pub required: bool,
}

fn default_required() -> bool {
    true
}

/// Extension manifest metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionManifest {
    /// Unique identifier for the extension
    pub id: ExtensionId,

    /// Extension type (skill, mcp, tool, channel, etc.)
    pub extension_type: String,

    /// Human-readable name
    pub name: String,

    /// Description of what the extension does
    pub description: String,

    /// Version of the extension
    pub version: String,

    /// Path to the extension directory
    pub path: PathBuf,

    /// First-class dependency list (replaces metadata "dependencies" convention)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ExtensionDependency>,

    /// Additional metadata (type-specific)
    #[serde(flatten)]
    pub metadata: HashMap<String, serde_json::Value>,

    /// Registry reference this extension was originally pulled from (e.g., "pekohub.com/extensions/calc:latest")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl ExtensionManifest {
    /// Create a new extension manifest
    pub fn new(
        id: impl Into<String>,
        extension_type: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        version: impl Into<String>,
        path: PathBuf,
    ) -> Self {
        Self {
            id: ExtensionId::new(id),
            extension_type: extension_type.into(),
            name: name.into(),
            description: description.into(),
            version: version.into(),
            path,
            dependencies: Vec::new(),
            metadata: HashMap::new(),
            source: None,
        }
    }

    /// Get a metadata value
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.metadata.get(key)
    }

    /// Set a metadata value
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.metadata.insert(key.into(), value.into());
    }

    /// Migrate legacy `metadata["dependencies"]` (array of strings) into the typed
    /// `dependencies` field.  This should be called immediately after deserialisation
    /// whenever an `ExtensionManifest` is loaded from an on-disk format.
    pub fn migrate_legacy_dependencies(&mut self) {
        if let Some(deps) = self.metadata.remove("dependencies") {
            if let Some(arr) = deps.as_array() {
                for dep in arr {
                    if let Some(pkg) = dep.as_str() {
                        // Only add if not already present in the typed field
                        if !self.dependencies.iter().any(|d| d.package == pkg) {
                            self.dependencies.push(ExtensionDependency {
                                package: pkg.to_string(),
                                version: None,
                                required: true,
                            });
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_manifest() {
        let manifest = ExtensionManifest::new(
            "docker-skill",
            "skill",
            "Docker Skill",
            "Manage Docker containers",
            "1.0.0",
            PathBuf::from("/tmp/skills/docker"),
        );

        assert_eq!(manifest.id.0, "docker-skill");
        assert_eq!(manifest.extension_type, "skill");
        assert_eq!(manifest.name, "Docker Skill");
    }

    #[test]
    fn test_manifest_metadata() {
        let mut manifest = ExtensionManifest::new(
            "test",
            "skill",
            "Test",
            "Desc",
            "1.0.0",
            PathBuf::from("/tmp"),
        );

        manifest.set("key", "value");
        assert_eq!(
            manifest.get("key"),
            Some(&serde_json::Value::String("value".to_string()))
        );
    }

    #[test]
    fn test_legacy_dependency_migration() {
        let mut manifest = ExtensionManifest::new(
            "test",
            "skill",
            "Test",
            "Desc",
            "1.0.0",
            PathBuf::from("/tmp"),
        );
        manifest.set(
            "dependencies",
            serde_json::json!(["pekohub.com/ext/a", "pekohub.com/ext/b"]),
        );

        assert!(manifest.dependencies.is_empty());
        manifest.migrate_legacy_dependencies();
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].package, "pekohub.com/ext/a");
        assert!(manifest.dependencies[0].required);
        assert_eq!(manifest.dependencies[0].version, None);
        assert_eq!(manifest.dependencies[1].package, "pekohub.com/ext/b");
        // Should be removed from metadata
        assert!(!manifest.metadata.contains_key("dependencies"));
    }

    #[test]
    fn test_legacy_migration_does_not_duplicate() {
        let mut manifest = ExtensionManifest::new(
            "test",
            "skill",
            "Test",
            "Desc",
            "1.0.0",
            PathBuf::from("/tmp"),
        );
        manifest.dependencies.push(ExtensionDependency {
            package: "pekohub.com/ext/a".to_string(),
            version: Some("^1.0".to_string()),
            required: true,
        });
        manifest.set("dependencies", serde_json::json!(["pekohub.com/ext/a"]));

        manifest.migrate_legacy_dependencies();
        assert_eq!(manifest.dependencies.len(), 1);
        // Typed version should be preserved (not overwritten)
        assert_eq!(manifest.dependencies[0].version, Some("^1.0".to_string()));
    }

    #[test]
    fn test_dependency_default_required() {
        let dep: ExtensionDependency = serde_json::from_str(r#"{"package": "foo"}"#).unwrap();
        assert!(dep.required);
    }

    #[test]
    fn test_dependency_optional() {
        let dep: ExtensionDependency =
            serde_json::from_str(r#"{"package": "foo", "required": false}"#).unwrap();
        assert!(!dep.required);
    }
}
