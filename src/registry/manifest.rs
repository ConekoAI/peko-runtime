//! Registry manifest types
//!
//! JSON-serializable manifest for registry push/pull protocol.
//! This is the wire format — the canonical on-disk format is TOML (`AgentManifest`).

use crate::portable::types::Layer;
use serde::{Deserialize, Serialize};

/// Schema version for registry manifests
pub const REGISTRY_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Registry manifest — JSON wire format for push/pull
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryManifest {
    /// Schema version
    pub schema_version: u32,
    /// Package kind: "agent", "extension", or "team"
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Agent name
    pub name: String,
    /// Package version
    pub version: String,
    /// Full reference (e.g., "pekohub.com/agents/researcher:v2.5")
    pub r#ref: String,
    /// Content-addressable digest
    pub digest: String,
    /// Creation timestamp (ISO 8601)
    pub created_at: String,
    /// Source: "registry" or "local"
    pub source: String,
    /// Layers composing this package
    pub layers: Vec<Layer>,
}

fn default_kind() -> String {
    "agent".to_string()
}

impl RegistryManifest {
    /// Create a new manifest
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            schema_version: REGISTRY_MANIFEST_SCHEMA_VERSION,
            kind: default_kind(),
            name: name.into(),
            version: version.into(),
            r#ref: String::new(),
            digest: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            source: "local".to_string(),
            layers: Vec::new(),
        }
    }

    /// Set the kind.
    #[must_use]
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = kind.into();
        self
    }

    /// Set the reference.
    #[must_use]
    pub fn with_ref(mut self, r#ref: impl Into<String>) -> Self {
        self.r#ref = r#ref.into();
        self
    }

    /// Set the digest.
    #[must_use]
    pub fn with_digest(mut self, digest: impl Into<String>) -> Self {
        self.digest = digest.into();
        self
    }

    /// Add a layer
    pub fn add_layer(&mut self, layer: Layer) {
        self.layers.push(layer);
    }

    /// Calculate total size
    #[must_use]
    pub fn total_size_bytes(&self) -> u64 {
        self.layers.iter().map(|l| l.size_bytes).sum()
    }

    /// Serialize to JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> anyhow::Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize manifest: {e}"))
    }

    /// Deserialize from JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails.
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("Failed to parse manifest: {e}"))
    }

    /// Get layer by type
    #[must_use]
    pub fn get_layer(&self, layer_type: crate::portable::types::LayerType) -> Option<&Layer> {
        self.layers.iter().find(|l| l.layer_type == layer_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portable::types::LayerType;

    #[test]
    fn test_registry_manifest_serialization() {
        let mut manifest = RegistryManifest::new("test-agent", "1.0.0")
            .with_ref("pekohub.com/test-agent:v1.0.0")
            .with_digest("sha256:abc123");

        manifest.add_layer(Layer::new("sha256:config123", LayerType::Config, 512));

        let json = manifest.to_json().unwrap();
        assert!(json.contains("\"schema_version\": 1"));
        assert!(json.contains("\"name\": \"test-agent\""));
        assert!(json.contains("\"type\": \"config\""));
        assert!(json.contains("\"kind\": \"agent\""));
    }

    #[test]
    fn test_total_size() {
        let mut manifest = RegistryManifest::new("test", "1.0.0");
        manifest.add_layer(Layer::new("sha256:a", LayerType::Config, 100));
        manifest.add_layer(Layer::new("sha256:b", LayerType::Skills, 200));
        assert_eq!(manifest.total_size_bytes(), 300);
    }

    #[test]
    fn test_kind_field() {
        let manifest = RegistryManifest::new("test", "1.0.0");
        assert_eq!(manifest.kind, "agent");

        let manifest = RegistryManifest::new("test", "1.0.0").with_kind("extension");
        assert_eq!(manifest.kind, "extension");

        let manifest = RegistryManifest::new("test", "1.0.0").with_kind("team");
        assert_eq!(manifest.kind, "team");
    }

    #[test]
    fn test_kind_serialization() {
        let manifest = RegistryManifest::new("test", "1.0.0").with_kind("extension");
        let json = manifest.to_json().unwrap();
        assert!(json.contains("\"kind\": \"extension\""));

        // Default kind should be serialized too
        let manifest = RegistryManifest::new("test", "1.0.0");
        let json = manifest.to_json().unwrap();
        assert!(json.contains("\"kind\": \"agent\""));
    }
}
