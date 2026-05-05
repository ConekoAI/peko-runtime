//! Extension manifest types

use crate::extension::types::ExtensionId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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

    /// Additional metadata (type-specific)
    #[serde(flatten)]
    pub metadata: HashMap<String, serde_json::Value>,
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
            metadata: HashMap::new(),
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
        assert_eq!(manifest.get("key"), Some(&serde_json::Value::String("value".to_string())));
    }
}
