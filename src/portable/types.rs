//! Portable packaging types
//!
//! Content-addressable digest and layer types for .agent packages.
//! Merged from the former `src/image/` module.

#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};

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

/// Layer types for .agent packages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerType {
    /// Config layer (contains agent.toml)
    Config,
    /// Identity layer (DID document, keys)
    Identity,
    /// Skills layer
    Skills,
    /// Workspace layer
    Workspace,
    /// Sessions layer
    Sessions,
    /// MCP layer
    Mcp,
    /// Team config layer (team.toml, manifest.toml, agent index)
    /// Used for team registry push/pull to enable cross-team agent deduplication.
    TeamConfig,
}

impl LayerType {
    /// Get the directory name for this layer type
    #[must_use]
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn dir_name(&self) -> &'static str {
        match self {
            LayerType::Config => "config",
            LayerType::Identity => "identity",
            LayerType::Skills => "skills",
            LayerType::Workspace => "workspace",
            LayerType::Sessions => "sessions",
            LayerType::Mcp => "mcp",
            LayerType::TeamConfig => "team",
        }
    }
}

/// A content-addressable layer with digest, type, and size
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
}

impl Layer {
    /// Create a new layer
    pub fn new(digest: impl Into<String>, layer_type: LayerType, size_bytes: u64) -> Self {
        Self {
            digest: digest.into(),
            layer_type,
            size_bytes,
            paths: None,
        }
    }

    /// Set file paths
    #[must_use]
    pub fn with_paths(mut self, paths: Vec<String>) -> Self {
        self.paths = Some(paths);
        self
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

/// Layer digest entry (type + digest string)
#[derive(Debug, Clone)]
pub struct LayerDigest {
    /// Layer type
    pub layer_type: LayerType,
    /// Digest string (sha256:...)
    pub digest: String,
}

impl LayerDigest {
    /// Create a new layer digest entry
    pub fn new(layer_type: LayerType, digest: impl Into<String>) -> Self {
        Self {
            layer_type,
            digest: digest.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_digest_new() {
        let hex = "a".repeat(64);
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
    fn test_image_digest_invalid_length() {
        assert!(ImageDigest::new("too_short").is_err());
        assert!(ImageDigest::new("a".repeat(63)).is_err());
        assert!(ImageDigest::new("a".repeat(65)).is_err());
    }

    #[test]
    fn test_layer_type_dir_name() {
        assert_eq!(LayerType::Config.dir_name(), "config");
        assert_eq!(LayerType::Identity.dir_name(), "identity");
        assert_eq!(LayerType::Skills.dir_name(), "skills");
        assert_eq!(LayerType::Workspace.dir_name(), "workspace");
        assert_eq!(LayerType::Sessions.dir_name(), "sessions");
        assert_eq!(LayerType::Mcp.dir_name(), "mcp");
        assert_eq!(LayerType::TeamConfig.dir_name(), "team");
    }

    #[test]
    fn test_compute_digest() {
        let digest = compute_digest(b"test");
        assert!(digest.starts_with("sha256:"));
        assert_eq!(digest.len(), 64 + 7);
    }

    #[test]
    fn test_layer_creation() {
        let layer = Layer::new("sha256:abc123", LayerType::Config, 512)
            .with_paths(vec!["config/agent.toml".to_string()]);
        assert_eq!(layer.digest, "sha256:abc123");
        assert_eq!(layer.layer_type, LayerType::Config);
        assert_eq!(layer.size_bytes, 512);
        assert_eq!(layer.paths.as_ref().unwrap()[0], "config/agent.toml");
    }
}
