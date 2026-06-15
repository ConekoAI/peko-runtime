//! Registry manifest types
//!
//! JSON-serializable manifest for registry push/pull protocol.
//! Wire format follows OCI Image Manifest v1.1 spec for interoperability.
//! The canonical on-disk format remains TOML (`AgentManifest`).

use crate::portable::types::{ExtensionRef, Layer};
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
    /// OCI annotations (discovery metadata, not identity)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Map<String, serde_json::Value>>,
    // --- Internal fields (not serialized to wire) ---
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
    // --- Discovery metadata (serialized to annotations) ---
    /// Human-readable description
    #[serde(skip)]
    pub description: Option<String>,
    /// Author(s)
    #[serde(skip)]
    pub author: Option<String>,
    /// SPDX license expression
    #[serde(skip)]
    pub license: Option<String>,
    /// Bundle type: "agent", "team", or "extension"
    #[serde(skip)]
    pub bundle_type: Option<String>,
    /// Extension type: "skill", "mcp", "tool", "channel", etc.
    #[serde(skip)]
    pub extension_type: Option<String>,
    /// Tags (JSON array string)
    #[serde(skip)]
    pub tags: Option<String>,
    /// Categories (JSON array string)
    #[serde(skip)]
    pub categories: Option<String>,
    /// Readme content or URL
    #[serde(skip)]
    pub readme: Option<String>,
    /// Hooks (JSON array string)
    #[serde(skip)]
    pub hooks: Option<String>,
    /// Compatibility (JSON object string)
    #[serde(skip)]
    pub compatibility: Option<String>,
    /// Model providers (JSON array string)
    #[serde(skip)]
    pub model_providers: Option<String>,
    /// Required MCP servers (JSON array string)
    #[serde(skip)]
    pub required_mcp_servers: Option<String>,
    /// Extension dependencies declared by the agent (auto-pulled on
    /// `peko agent pull`). Stored as a JSON string in annotations
    /// for OCI compatibility; restored to a `Vec<ExtensionRef>` on
    /// deserialize.
    ///
    /// Without this round-trip the runtime silently drops the
    /// agent's `extensions` list at the registry push/pull boundary
    /// — Phase D3 scenario 5b/5c/5d is the first end-to-end test
    /// that exercises this code path and surfaces the regression.
    #[serde(skip)]
    pub extensions: Vec<ExtensionRef>,
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
            description: None,
            author: None,
            license: None,
            bundle_type: None,
            extension_type: None,
            tags: None,
            categories: None,
            readme: None,
            hooks: None,
            compatibility: None,
            model_providers: None,
            required_mcp_servers: None,
            extensions: Vec::new(),
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

    /// Set the OCI config descriptor (required by the OCI Image Manifest
    /// spec; the registry validates it and rejects manifests where
    /// `config.digest` is empty or not in `sha256:<hex>` form).
    ///
    /// `digest` should be the digest of the config *blob* (not the
    /// manifest blob) — typically the same digest you'd `add_layer`
    /// for a `LayerType::Config` entry, but the descriptor lives in
    /// its own top-level `config` field, not the `layers` array.
    #[must_use]
    pub fn with_config(
        mut self,
        digest: impl Into<String>,
        size: u64,
        media_type: Option<impl Into<String>>,
    ) -> Self {
        self.config.digest = digest.into();
        self.config.size = size;
        if let Some(mt) = media_type {
            self.config.media_type = mt.into();
        }
        self
    }

    /// Set the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the author.
    #[must_use]
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Set the license.
    #[must_use]
    pub fn with_license(mut self, license: impl Into<String>) -> Self {
        self.license = Some(license.into());
        self
    }

    /// Set the bundle type.
    #[must_use]
    pub fn with_bundle_type(mut self, bundle_type: impl Into<String>) -> Self {
        self.bundle_type = Some(bundle_type.into());
        self
    }

    /// Set the extension type.
    #[must_use]
    pub fn with_extension_type(mut self, extension_type: impl Into<String>) -> Self {
        self.extension_type = Some(extension_type.into());
        self
    }

    /// Set tags (JSON array string).
    #[must_use]
    pub fn with_tags(mut self, tags: impl Into<String>) -> Self {
        self.tags = Some(tags.into());
        self
    }

    /// Set categories (JSON array string).
    #[must_use]
    pub fn with_categories(mut self, categories: impl Into<String>) -> Self {
        self.categories = Some(categories.into());
        self
    }

    /// Set readme content or URL.
    #[must_use]
    pub fn with_readme(mut self, readme: impl Into<String>) -> Self {
        self.readme = Some(readme.into());
        self
    }

    /// Set hooks (JSON array string).
    #[must_use]
    pub fn with_hooks(mut self, hooks: impl Into<String>) -> Self {
        self.hooks = Some(hooks.into());
        self
    }

    /// Set compatibility (JSON object string).
    #[must_use]
    pub fn with_compatibility(mut self, compatibility: impl Into<String>) -> Self {
        self.compatibility = Some(compatibility.into());
        self
    }

    /// Set model providers (JSON array string).
    #[must_use]
    pub fn with_model_providers(mut self, model_providers: impl Into<String>) -> Self {
        self.model_providers = Some(model_providers.into());
        self
    }

    /// Set required MCP servers (JSON array string).
    #[must_use]
    pub fn with_required_mcp_servers(mut self, required_mcp_servers: impl Into<String>) -> Self {
        self.required_mcp_servers = Some(required_mcp_servers.into());
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

    /// Build annotations map from discovery and identity fields.
    ///
    /// Emits flat OCI standard and Peko-specific annotations.
    /// Identity fields (name, version, kind) are included for pull-side round-tripping.
    fn build_annotations(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let mut map = serde_json::Map::new();

        // Identity fields (for pull-side round-tripping)
        if !self.name.is_empty() {
            map.insert(
                "org.peko.name".to_string(),
                serde_json::Value::String(self.name.clone()),
            );
        }
        if !self.version.is_empty() {
            map.insert(
                "org.peko.version".to_string(),
                serde_json::Value::String(self.version.clone()),
            );
        }
        if !self.kind.is_empty() {
            map.insert(
                "org.peko.kind".to_string(),
                serde_json::Value::String(self.kind.clone()),
            );
        }

        // OCI standard annotations
        if let Some(v) = &self.description {
            map.insert(
                "org.opencontainers.image.description".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.author {
            map.insert(
                "org.opencontainers.image.authors".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.license {
            map.insert(
                "org.opencontainers.image.licenses".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }

        // Peko-specific annotations
        if let Some(v) = &self.bundle_type {
            map.insert(
                "dev.pekohub.bundleType".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.extension_type {
            map.insert(
                "dev.pekohub.extensionType".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.tags {
            map.insert(
                "dev.pekohub.tags".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.categories {
            map.insert(
                "dev.pekohub.categories".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.readme {
            map.insert(
                "dev.pekohub.readme".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.hooks {
            map.insert(
                "dev.pekohub.hooks".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.compatibility {
            map.insert(
                "dev.pekohub.compatibility".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.model_providers {
            map.insert(
                "dev.pekohub.modelProviders".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = &self.required_mcp_servers {
            map.insert(
                "dev.pekohub.requiredMcpServers".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }

        // Extension dependencies (Phase D3 round-trip). Emitted as
        // a JSON string (a serialized array of {id, registry_ref}
        // objects) — pekohub's manifest validator expects
        // `dev.pekohub.*` annotations to be strings, not arrays
        // (the `annotations` map is deserialized into a
        // `Record<string, string>` on the backend). Empty arrays
        // are omitted — empty agent has no declared exts and we
        // don't want to bloat the wire format.
        if !self.extensions.is_empty() {
            let arr: Vec<serde_json::Value> = self
                .extensions
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "registry_ref": e.registry_ref,
                    })
                })
                .collect();
            // Serializing a `Vec<{id, registry_ref}>` cannot fail —
            // the values are plain `String` — so unwrap is safe.
            let s = serde_json::to_string(&arr).expect("serialize extensions");
            map.insert(
                "dev.pekohub.extensions".to_string(),
                serde_json::Value::String(s),
            );
        }

        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    }

    /// Extract fields from annotations map for pull-side reconstruction.
    fn apply_annotations(
        &mut self,
        annotations: &Option<serde_json::Map<String, serde_json::Value>>,
    ) {
        if let Some(map) = annotations {
            // Identity fields
            if let Some(v) = map.get("org.peko.name").and_then(|v| v.as_str()) {
                self.name = v.to_string();
            }
            if let Some(v) = map.get("org.peko.version").and_then(|v| v.as_str()) {
                self.version = v.to_string();
            }
            if let Some(v) = map.get("org.peko.kind").and_then(|v| v.as_str()) {
                self.kind = v.to_string();
            }

            // OCI standard annotations
            if let Some(v) = map
                .get("org.opencontainers.image.description")
                .and_then(|v| v.as_str())
            {
                self.description = Some(v.to_string());
            }
            if let Some(v) = map
                .get("org.opencontainers.image.authors")
                .and_then(|v| v.as_str())
            {
                self.author = Some(v.to_string());
            }
            if let Some(v) = map
                .get("org.opencontainers.image.licenses")
                .and_then(|v| v.as_str())
            {
                self.license = Some(v.to_string());
            }
            if let Some(v) = map.get("dev.pekohub.bundleType").and_then(|v| v.as_str()) {
                self.bundle_type = Some(v.to_string());
            }
            if let Some(v) = map
                .get("dev.pekohub.extensionType")
                .and_then(|v| v.as_str())
            {
                self.extension_type = Some(v.to_string());
            }
            if let Some(v) = map.get("dev.pekohub.tags").and_then(|v| v.as_str()) {
                self.tags = Some(v.to_string());
            }
            if let Some(v) = map.get("dev.pekohub.categories").and_then(|v| v.as_str()) {
                self.categories = Some(v.to_string());
            }
            if let Some(v) = map.get("dev.pekohub.readme").and_then(|v| v.as_str()) {
                self.readme = Some(v.to_string());
            }
            if let Some(v) = map.get("dev.pekohub.hooks").and_then(|v| v.as_str()) {
                self.hooks = Some(v.to_string());
            }
            if let Some(v) = map
                .get("dev.pekohub.compatibility")
                .and_then(|v| v.as_str())
            {
                self.compatibility = Some(v.to_string());
            }
            if let Some(v) = map
                .get("dev.pekohub.modelProviders")
                .and_then(|v| v.as_str())
            {
                self.model_providers = Some(v.to_string());
            }
            if let Some(v) = map
                .get("dev.pekohub.requiredMcpServers")
                .and_then(|v| v.as_str())
            {
                self.required_mcp_servers = Some(v.to_string());
            }

            // Restore extension dependencies (Phase D3 round-trip).
            // The annotation may be a JSON string (legacy writes)
            // or a JSON array (this implementation). Tolerate both.
            if let Some(v) = map.get("dev.pekohub.extensions") {
                if let Some(arr) = v.as_array() {
                    self.extensions = arr
                        .iter()
                        .filter_map(|item| {
                            let id = item.get("id")?.as_str()?.to_string();
                            let registry_ref =
                                item.get("registry_ref")?.as_str()?.to_string();
                            Some(ExtensionRef { id, registry_ref })
                        })
                        .collect();
                } else if let Some(s) = v.as_str() {
                    // Legacy / hand-written format: parse the JSON
                    // string and pull the same fields.
                    if let Ok(parsed) =
                        serde_json::from_str::<Vec<serde_json::Value>>(s)
                    {
                        self.extensions = parsed
                            .iter()
                            .filter_map(|item| {
                                let id = item.get("id")?.as_str()?.to_string();
                                let registry_ref =
                                    item.get("registry_ref")?.as_str()?.to_string();
                                Some(ExtensionRef { id, registry_ref })
                            })
                            .collect();
                    }
                }
            }
        }
    }

    /// Serialize to JSON (OCI format).
    ///
    /// Discovery metadata is stored in OCI annotations.
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
    /// Discovery metadata is restored from OCI annotations.
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
        // Internal fields are NOT in wire output
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
            .with_digest("sha256:deadbeef")
            .with_description("A test agent")
            .with_author("alice")
            .with_bundle_type("agent")
            .with_tags("[\"ai\", \"research\"]");
        manifest.add_layer(Layer::new("sha256:layer1", LayerType::Config, 100));

        // Serialize — discovery metadata goes into annotations
        let json = manifest.to_json().unwrap();
        assert!(json.contains("\"annotations\""));
        assert!(json.contains("org.opencontainers.image.description"));
        assert!(json.contains("A test agent"));
        assert!(json.contains("org.opencontainers.image.authors"));
        assert!(json.contains("alice"));
        assert!(json.contains("dev.pekohub.bundleType"));
        assert!(json.contains("agent"));
        assert!(json.contains("dev.pekohub.tags"));
        // tags are stored as a JSON string; check roundtrip via restored value instead
        assert_eq!(
            RegistryManifest::from_json(&json).unwrap().tags,
            Some("[\"ai\", \"research\"]".to_string())
        );
        // Identity and discovery fields are both emitted to annotations
        assert!(json.contains("org.peko.name"));
        assert!(json.contains("org.peko.version"));
        assert!(json.contains("org.peko.kind"));

        // Deserialize — all fields restored from annotations
        let restored = RegistryManifest::from_json(&json).unwrap();
        assert_eq!(restored.name, "test-agent");
        assert_eq!(restored.version, "1.2.3");
        assert_eq!(restored.kind, "agent");
        assert_eq!(restored.description, Some("A test agent".to_string()));
        assert_eq!(restored.author, Some("alice".to_string()));
        assert_eq!(restored.bundle_type, Some("agent".to_string()));
        assert_eq!(restored.tags, Some("[\"ai\", \"research\"]".to_string()));
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
        assert_eq!(manifest.description, None);
        assert_eq!(manifest.layers.len(), 1);
    }

    #[test]
    fn test_flat_annotation_read() {
        // Simulate receiving a manifest with flat OCI + Peko annotations
        let oci_json = r#"{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "config": {
    "mediaType": "application/vnd.peko.config.v1+json",
    "digest": "sha256:config",
    "size": 123
  },
  "layers": [],
  "annotations": {
    "org.opencontainers.image.description": "My extension",
    "org.opencontainers.image.authors": "bob",
    "org.opencontainers.image.licenses": "MIT",
    "dev.pekohub.bundleType": "extension",
    "dev.pekohub.extensionType": "skill",
    "dev.pekohub.tags": "[\"nlp\", \"transformers\"]",
    "dev.pekohub.hooks": "[{\"point\": \"tool.register\"}]"
  }
}"#;
        let manifest = RegistryManifest::from_json(oci_json).unwrap();
        assert_eq!(manifest.description, Some("My extension".to_string()));
        assert_eq!(manifest.author, Some("bob".to_string()));
        assert_eq!(manifest.license, Some("MIT".to_string()));
        assert_eq!(manifest.bundle_type, Some("extension".to_string()));
        assert_eq!(manifest.extension_type, Some("skill".to_string()));
        assert_eq!(
            manifest.tags,
            Some("[\"nlp\", \"transformers\"]".to_string())
        );
        assert_eq!(
            manifest.hooks,
            Some("[{\"point\": \"tool.register\"}]".to_string())
        );
    }
}
