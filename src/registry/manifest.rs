//! Registry manifest types
//!
//! JSON-serializable manifest for registry push/pull protocol.
//! Wire format follows OCI Image Manifest v1.1 spec for interoperability.
//! The canonical on-disk format remains TOML (`AgentManifest`).

use crate::portable::types::Layer;
use serde::{Deserialize, Serialize};

/// OCI Image Manifest media type
pub const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// OCI config media type for Peko packages
pub const PEKO_CONFIG_MEDIA_TYPE: &str = "application/vnd.peko.config.v1+json";

/// Schema version for OCI image manifests
pub const REGISTRY_MANIFEST_SCHEMA_VERSION: u32 = 2;

/// OCI config descriptor (required by OCI manifest spec)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDescriptor {
    /// Media type of the config blob
    #[serde(rename = "mediaType")]
    pub media_type: String,
    /// Digest of the config blob
    pub digest: String,
    /// Size of the config blob in bytes
    pub size: u64,
}

/// Registry manifest — OCI Image Manifest v1.1 wire format for push/pull
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryManifest {
    /// OCI schema version (always 2)
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// OCI manifest media type
    #[serde(rename = "mediaType")]
    pub media_type: String,
    /// Config descriptor (required by OCI spec)
    pub config: ConfigDescriptor,
    /// Layers composing this package (OCI descriptors)
    pub layers: Vec<Layer>,
    /// Peko-specific annotations (preserved in OCI annotations field)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Map<String, serde_json::Value>>,
    // --- Peko-specific fields (not serialized to wire) ---
    /// Package kind: "agent", "extension", or "team"
    #[serde(skip)]
    pub kind: String,
    /// Agent name
    #[serde(skip)]
    pub name: String,
    /// Package version
    #[serde(skip)]
    pub version: String,
    /// Full reference (e.g., "pekohub.com/agents/researcher:v2.5")
    #[serde(skip)]
    pub r#ref: String,
    /// Content-addressable digest
    #[serde(skip)]
    pub digest: String,
    /// Creation timestamp (ISO 8601)
    #[serde(skip)]
    pub created_at: String,
    /// Source: "registry" or "local"
    #[serde(skip)]
    pub source: String,
}

fn default_kind() -> String {
    "agent".to_string()
}

impl RegistryManifest {
    /// Create a new manifest with a default config descriptor.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            schema_version: REGISTRY_MANIFEST_SCHEMA_VERSION,
            media_type: OCI_MANIFEST_MEDIA_TYPE.to_string(),
            config: ConfigDescriptor {
                media_type: PEKO_CONFIG_MEDIA_TYPE.to_string(),
                digest: String::new(),
                size: 0,
            },
            layers: Vec::new(),
            annotations: None,
            kind: default_kind(),
            name: name.into(),
            version: version.into(),
            r#ref: String::new(),
            digest: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            source: "local".to_string(),
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

    /// Calculate total size of all layers
    #[must_use]
    pub fn total_size_bytes(&self) -> u64 {
        self.layers.iter().map(|l| l.size_bytes).sum()
    }

    /// Build annotations map from Peko-specific fields.
    fn build_annotations(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let mut map = serde_json::Map::new();
        if !self.name.is_empty() {
            map.insert("org.peko.name".to_string(), serde_json::Value::String(self.name.clone()));
        }
        if !self.version.is_empty() {
            map.insert("org.peko.version".to_string(), serde_json::Value::String(self.version.clone()));
        }
        if !self.kind.is_empty() {
            map.insert("org.peko.kind".to_string(), serde_json::Value::String(self.kind.clone()));
        }
        if !self.r#ref.is_empty() {
            map.insert("org.peko.ref".to_string(), serde_json::Value::String(self.r#ref.clone()));
        }
        if !self.digest.is_empty() {
            map.insert("org.peko.digest".to_string(), serde_json::Value::String(self.digest.clone()));
        }
        if !self.created_at.is_empty() {
            map.insert("org.peko.createdAt".to_string(), serde_json::Value::String(self.created_at.clone()));
        }
        if !self.source.is_empty() {
            map.insert("org.peko.source".to_string(), serde_json::Value::String(self.source.clone()));
        }
        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    }

    /// Extract Peko-specific fields from annotations map.
    fn apply_annotations(&mut self, annotations: &Option<serde_json::Map<String, serde_json::Value>>) {
        if let Some(map) = annotations {
            if let Some(v) = map.get("org.peko.name").and_then(|v| v.as_str()) {
                self.name = v.to_string();
            }
            if let Some(v) = map.get("org.peko.version").and_then(|v| v.as_str()) {
                self.version = v.to_string();
            }
            if let Some(v) = map.get("org.peko.kind").and_then(|v| v.as_str()) {
                self.kind = v.to_string();
            }
            if let Some(v) = map.get("org.peko.ref").and_then(|v| v.as_str()) {
                self.r#ref = v.to_string();
            }
            if let Some(v) = map.get("org.peko.digest").and_then(|v| v.as_str()) {
                self.digest = v.to_string();
            }
            if let Some(v) = map.get("org.peko.createdAt").and_then(|v| v.as_str()) {
                self.created_at = v.to_string();
            }
            if let Some(v) = map.get("org.peko.source").and_then(|v| v.as_str()) {
                self.source = v.to_string();
            }
        }
    }

    /// Serialize to JSON (OCI format).
    ///
    /// Peko-specific metadata is stored in OCI annotations.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> anyhow::Result<String> {
        let mut manifest = self.clone();
        manifest.annotations = self.build_annotations();
        serde_json::to_string_pretty(&manifest)
            .map_err(|e| anyhow::anyhow!("Failed to serialize manifest: {e}"))
    }

    /// Deserialize from JSON (OCI format).
    ///
    /// Peko-specific metadata is restored from OCI annotations.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails.
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let mut manifest: Self = serde_json::from_str(json)
            .map_err(|e| anyhow::anyhow!("Failed to parse manifest: {e}"))?;
        manifest.apply_annotations(&manifest.annotations.clone());
        Ok(manifest)
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
    fn test_registry_manifest_oci_serialization() {
        let mut manifest = RegistryManifest::new("test-agent", "1.0.0")
            .with_ref("pekohub.com/test-agent:v1.0.0")
            .with_digest("sha256:abc123");

        manifest.add_layer(Layer::new("sha256:config123", LayerType::Config, 512));

        let json = manifest.to_json().unwrap();
        println!("{json}");
        // OCI fields
        assert!(json.contains("\"schemaVersion\": 2"));
        assert!(json.contains("\"mediaType\": \"application/vnd.oci.image.manifest.v1+json\""));
        assert!(json.contains("\"config\""));
        assert!(json.contains("\"mediaType\": \"application/vnd.peko.config.v1+json\""));
        // Layer as OCI descriptor
        assert!(json.contains("\"mediaType\": \"application/vnd.peko.layer.config.v1+json\""));
        assert!(json.contains("\"size\": 512"));
        // Peko fields are NOT in wire output
        assert!(!json.contains("\"name\":"));
        assert!(!json.contains("\"kind\":"));
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
    fn test_kind_not_serialized() {
        let manifest = RegistryManifest::new("test", "1.0.0").with_kind("extension");
        let json = manifest.to_json().unwrap();
        assert!(!json.contains("\"kind\""));
    }

    #[test]
    fn test_annotation_roundtrip() {
        let mut manifest = RegistryManifest::new("test-agent", "1.2.3")
            .with_kind("agent")
            .with_ref("pekohub.com/ns/test-agent:v1.2.3")
            .with_digest("sha256:deadbeef");
        manifest.add_layer(Layer::new("sha256:layer1", LayerType::Config, 100));

        // Serialize — Peko metadata goes into annotations
        let json = manifest.to_json().unwrap();
        assert!(json.contains("\"annotations\""));
        assert!(json.contains("org.peko.name"));
        assert!(json.contains("test-agent"));
        assert!(json.contains("org.peko.version"));
        assert!(json.contains("1.2.3"));
        assert!(json.contains("org.peko.kind"));
        assert!(json.contains("agent"));

        // Deserialize — Peko metadata restored from annotations
        let restored = RegistryManifest::from_json(&json).unwrap();
        assert_eq!(restored.name, "test-agent");
        assert_eq!(restored.version, "1.2.3");
        assert_eq!(restored.kind, "agent");
        assert_eq!(restored.r#ref, "pekohub.com/ns/test-agent:v1.2.3");
        assert_eq!(restored.digest, "sha256:deadbeef");
        assert_eq!(restored.layers.len(), 1);
        assert_eq!(restored.layers[0].digest, "sha256:layer1");
    }

    #[test]
    fn test_annotation_from_oci_without_peko_annotations() {
        // Simulate receiving a pure OCI manifest without Peko annotations
        let oci_json = r#"{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "config": {
    "mediaType": "application/vnd.peko.config.v1+json",
    "digest": "sha256:config",
    "size": 123
  },
  "layers": [
    {
      "digest": "sha256:layer1",
      "mediaType": "application/vnd.peko.layer.config.v1+json",
      "size": 100
    }
  ]
}"#;
        let manifest = RegistryManifest::from_json(oci_json).unwrap();
        assert_eq!(manifest.schema_version, 2);
        assert_eq!(manifest.name, ""); // No annotation, so empty
        assert_eq!(manifest.layers.len(), 1);
    }
}
