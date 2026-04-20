//! Image Manifest Types
//!
//! Defines the manifest structure for agent images as per `DATA_MODEL` §6.
//! Manifests describe the layers that compose an image and are stored
//! content-addressable by their SHA-256 digest.

use serde::{Deserialize, Serialize};

/// Schema version for image manifests
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// An image manifest describing the layers that compose an agent image
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageManifest {
    /// Schema version (currently 1)
    pub schema_version: u32,
    /// Image name (from config.toml)
    pub name: String,
    /// Image version (from config.toml)
    pub version: String,
    /// Full image reference (e.g., "pekohub.com/agents/researcher:v2.5")
    pub r#ref: String,
    /// Content-addressable digest (sha256:...)
    pub digest: String,
    /// Creation timestamp (ISO 8601)
    pub created_at: String,
    /// Source: "registry" or "local"
    pub source: String,
    /// Layers composing this image
    pub layers: Vec<Layer>,
    /// Base image information (if inheriting)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<BaseInfo>,
    /// Capability dependencies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<CapabilityDeps>,
}

impl ImageManifest {
    /// Create a new manifest with the given name and version
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            name: name.into(),
            version: version.into(),
            r#ref: String::new(),
            digest: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            source: "local".to_string(),
            layers: Vec::new(),
            base: None,
            capabilities: None,
        }
    }

    /// Set the image reference
    pub fn with_ref(mut self, r#ref: impl Into<String>) -> Self {
        self.r#ref = r#ref.into();
        self
    }

    /// Set the digest
    pub fn with_digest(mut self, digest: impl Into<String>) -> Self {
        self.digest = digest.into();
        self
    }

    /// Add a layer to the manifest
    pub fn add_layer(&mut self, layer: Layer) {
        self.layers.push(layer);
    }

    /// Set the base image
    #[must_use]
    pub fn with_base(mut self, base: BaseInfo) -> Self {
        self.base = Some(base);
        self
    }

    /// Set capability dependencies
    #[must_use]
    pub fn with_capabilities(mut self, caps: CapabilityDeps) -> Self {
        self.capabilities = Some(caps);
        self
    }

    /// Calculate total size of all layers
    #[must_use]
    pub fn total_size_bytes(&self) -> u64 {
        self.layers.iter().map(|l| l.size_bytes).sum()
    }

    /// Serialize to JSON string
    pub fn to_json(&self) -> anyhow::Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize manifest: {e}"))
    }

    /// Deserialize from JSON string
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("Failed to parse manifest: {e}"))
    }

    /// Get layer by type
    #[must_use]
    pub fn get_layer(&self, layer_type: LayerType) -> Option<&Layer> {
        self.layers.iter().find(|l| l.layer_type == layer_type)
    }
}

/// A layer in the image (content-addressable)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    /// SHA-256 digest of the layer content
    pub digest: String,
    /// Layer type
    #[serde(rename = "type")]
    pub layer_type: LayerType,
    /// Size in bytes
    pub size_bytes: u64,
    /// File paths included in this layer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    /// Single path (for single-file layers like config)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Layer {
    /// Create a new layer
    pub fn new(digest: impl Into<String>, layer_type: LayerType, size_bytes: u64) -> Self {
        Self {
            digest: digest.into(),
            layer_type,
            size_bytes,
            paths: None,
            path: None,
        }
    }

    /// Set single path
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Set multiple paths
    #[must_use]
    pub fn with_paths(mut self, paths: Vec<String>) -> Self {
        self.paths = Some(paths);
        self
    }

    /// Get all paths as a Vec
    #[must_use]
    pub fn get_paths(&self) -> Vec<&str> {
        if let Some(ref p) = self.path {
            vec![p.as_str()]
        } else if let Some(ref ps) = self.paths {
            ps.iter().map(std::string::String::as_str).collect()
        } else {
            Vec::new()
        }
    }
}

/// Layer types as per `DATA_MODEL` §6.2
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerType {
    /// config.toml only
    Config,
    /// Root .md files (AGENT.md, BOOTSTRAP.md, etc.)
    Markdown,
    /// Files in tools/ directory
    Tools,
    /// Files in projects/ directory
    Projects,
    /// Files in memories/ directory
    Memories,
    /// Files in skills/ directory
    Skills,
    /// mcp.json
    McpConfig,
}

impl LayerType {
    /// Get the directory name for this layer type (if applicable)
    #[must_use]
    pub fn directory(&self) -> Option<&'static str> {
        match self {
            LayerType::Tools => Some("tools"),
            LayerType::Projects => Some("projects"),
            LayerType::Memories => Some("memories"),
            LayerType::Skills => Some("skills"),
            _ => None,
        }
    }

    /// Get the file name for this layer type (if single file)
    #[must_use]
    pub fn filename(&self) -> Option<&'static str> {
        match self {
            LayerType::Config => Some("config.toml"),
            LayerType::McpConfig => Some("mcp.json"),
            _ => None,
        }
    }
}

/// Base image information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseInfo {
    /// Base image reference
    pub r#ref: String,
    /// Base image digest (pinned)
    pub digest: String,
}

impl BaseInfo {
    /// Create new base info
    pub fn new(r#ref: impl Into<String>, digest: impl Into<String>) -> Self {
        Self {
            r#ref: r#ref.into(),
            digest: digest.into(),
        }
    }
}

/// Capability dependencies
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityDeps {
    /// Required tool capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Required skill capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Required MCP capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcps: Option<Vec<String>>,
}

impl CapabilityDeps {
    /// Create empty capability deps
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add tools
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Add skills
    #[must_use]
    pub fn with_skills(mut self, skills: Vec<String>) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Add mcps
    #[must_use]
    pub fn with_mcps(mut self, mcps: Vec<String>) -> Self {
        self.mcps = Some(mcps);
        self
    }
}

/// Content-addressable digest (sha256:...)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageDigest {
    digest: String,
}

impl ImageDigest {
    /// Create a new digest from a hex string (with or without sha256: prefix)
    pub fn new(digest: impl Into<String>) -> anyhow::Result<Self> {
        let mut digest = digest.into();

        // Ensure prefix
        if let Some(hex_part) = digest.strip_prefix("sha256:") {
            // Validate the hex part
            if hex_part.len() != 64 {
                return Err(anyhow::anyhow!(
                    "Invalid digest length: expected sha256: + 64 hex chars"
                ));
            }
        } else {
            // Validate it's a valid hex string
            if digest.len() != 64 {
                return Err(anyhow::anyhow!(
                    "Invalid digest length: expected 64 hex chars, got {}",
                    digest.len()
                ));
            }
            if !digest.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(anyhow::anyhow!("Invalid digest: not a valid hex string"));
            }
            digest = format!("sha256:{digest}");
        }

        Ok(Self { digest })
    }

    /// Create from raw bytes (computes the hash)
    #[must_use]
    pub fn from_bytes(data: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        let hash = format!("sha256:{:x}", hasher.finalize());
        Self { digest: hash }
    }

    /// Get the full digest string (sha256:...)
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.digest
    }

    /// Get just the hex part (without sha256: prefix)
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.digest[7..]
    }

    /// Get the directory name for storage (sha256-abc123...)
    #[must_use]
    pub fn dir_name(&self) -> String {
        self.digest.replace(':', "-")
    }
}

impl std::fmt::Display for ImageDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.digest)
    }
}

impl Serialize for ImageDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.digest)
    }
}

impl<'de> Deserialize<'de> for ImageDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        ImageDigest::new(s).map_err(serde::de::Error::custom)
    }
}

/// Compute SHA-256 digest of data
#[must_use]
pub fn compute_digest(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_digest_new() {
        // Generate exactly 64 hex chars
        let hex = format!("{}{}", "abc123".repeat(10), "abc123")[..64].to_string();
        let digest = ImageDigest::new(&hex).unwrap();
        assert!(digest.as_str().starts_with("sha256:"));
        assert_eq!(digest.hex().len(), 64);
    }

    #[test]
    fn test_image_digest_with_prefix() {
        let hex = "a".repeat(64);
        let digest = ImageDigest::new(format!("sha256:{hex}")).unwrap();
        assert_eq!(digest.hex(), hex);
    }

    #[test]
    fn test_image_digest_from_bytes() {
        let data = b"hello world";
        let digest = ImageDigest::from_bytes(data);
        assert!(digest.as_str().starts_with("sha256:"));
        assert_eq!(digest.hex().len(), 64);
    }

    #[test]
    fn test_manifest_serialization() {
        let mut manifest = ImageManifest::new("test-agent", "1.0.0")
            .with_ref("pekohub.com/test-agent:v1.0.0")
            .with_digest("sha256:abc123");

        manifest.add_layer(
            Layer::new("sha256:config123", LayerType::Config, 512).with_path("config.toml"),
        );

        let json = manifest.to_json().unwrap();
        assert!(json.contains("\"schema_version\": 1"));
        assert!(json.contains("\"name\": \"test-agent\""));
        assert!(json.contains("\"type\": \"config\""));
    }

    #[test]
    fn test_layer_get_paths() {
        let layer1 = Layer::new("sha256:abc", LayerType::Config, 100).with_path("config.toml");
        assert_eq!(layer1.get_paths(), vec!["config.toml"]);

        let layer2 = Layer::new("sha256:def", LayerType::Markdown, 200)
            .with_paths(vec!["AGENT.md".to_string(), "BOOTSTRAP.md".to_string()]);
        assert_eq!(layer2.get_paths(), vec!["AGENT.md", "BOOTSTRAP.md"]);
    }
}
