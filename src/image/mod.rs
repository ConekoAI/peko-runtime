//! Agent Image Management
//!
//! This module provides the image/instance distinction with filesystem-first
//! agent definition. Images are immutable, content-addressable snapshots that
//! can be built, stored, and instantiated.
//!
//! ## Architecture
//!
//! - `ImageManifest`: Describes an image with its layers and metadata
//! - `ImageConfig`: Parsed `config.toml` with validation
//! - `ImageBuilder`: Builds images from agent directories
//! - `ImageRegistry`: Content-addressable storage for images and layers
//!
//! ## Storage Layout
//!
//! ```text
//! .pekobot/registry/
//! ├── images/
//! │   ├── sha256:abc123.../manifest.json
//! │   └── sha256:def456.../manifest.json
//! └── layers/
//!     ├── sha256:layer1.../.tar.gz
//!     └── sha256:layer2.../.tar.gz
//! ```

pub mod builder;
pub mod config;
pub mod manifest;
pub mod registry;

pub use builder::{BuildOptions, BuildProgress, ImageBuilder};
pub use config::{AgentConfig, BaseImage, CapabilityConfig, Hook, ProviderConfig, Resources};
pub use manifest::{ImageDigest, ImageManifest, Layer, LayerType};
pub use registry::{ImageRegistry, RegistryConfig};

use std::path::PathBuf;

/// Reference to an image (tag, digest, or path)
#[derive(Debug, Clone)]
pub enum ImageRef {
    /// Full registry reference: "pekohub.com/agents/researcher:v2.5"
    RegistryRef {
        host: String,
        path: String,
        tag: String,
    },
    /// Local tag: "my-agent:v1.0"
    LocalTag { name: String, tag: String },
    /// Exact digest: "sha256:abc123..."
    Digest(ImageDigest),
    /// Filesystem path: "./my-agent/"
    Path(PathBuf),
}

impl ImageRef {
    /// Parse an image reference string
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        // Check for digest prefix
        if let Some(digest) = s.strip_prefix("sha256:") {
            return Ok(Self::Digest(ImageDigest::new(digest)?));
        }

        // Check for path prefix
        if s.starts_with("./") || s.starts_with('/') || s.starts_with("..") {
            return Ok(Self::Path(PathBuf::from(s)));
        }

        // Check for registry ref (contains '/')
        if s.contains('/') {
            // Parse "host/path/to/agent:tag"
            let (ref_part, tag) = s.rsplit_once(':').unwrap_or((s, "latest"));
            let parts: Vec<&str> = ref_part.split('/').collect();
            if parts.len() >= 2 {
                let host = parts[0].to_string();
                let path = parts[1..].join("/");
                return Ok(Self::RegistryRef {
                    host,
                    path,
                    tag: tag.to_string(),
                });
            }
        }

        // Local tag: "name:tag" or "name" (defaults to "latest")
        let (name, tag) = s.rsplit_once(':').unwrap_or((s, "latest"));
        Ok(Self::LocalTag {
            name: name.to_string(),
            tag: tag.to_string(),
        })
    }

    /// Get the display string for this reference
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::RegistryRef { host, path, tag } => format!("{host}/{path}:{tag}"),
            Self::LocalTag { name, tag } => format!("{name}:{tag}"),
            Self::Digest(d) => d.to_string(),
            Self::Path(p) => p.display().to_string(),
        }
    }
}

impl std::str::FromStr for ImageRef {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Unique identifier for an image instance
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageId(pub String);

impl ImageId {
    /// Create a new image ID from a digest string
    pub fn new(digest: impl Into<String>) -> Self {
        Self(digest.into())
    }

    /// Get the string representation
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ImageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_ref_parse_digest() {
        // Generate exactly 64 hex chars for the digest
        let hex = format!("{}{}", "a".repeat(60), "1234");
        let digest = format!("sha256:{hex}");
        let r = ImageRef::parse(&digest).unwrap();
        match r {
            ImageRef::Digest(d) => assert_eq!(d.as_str(), digest),
            _ => panic!("Expected Digest"),
        }
    }

    #[test]
    fn test_image_ref_parse_path() {
        let r = ImageRef::parse("./my-agent").unwrap();
        match r {
            ImageRef::Path(p) => assert_eq!(p.to_str(), Some("./my-agent")),
            _ => panic!("Expected Path"),
        }
    }

    #[test]
    fn test_image_ref_parse_local_tag() {
        let r = ImageRef::parse("my-agent:v1.0").unwrap();
        match r {
            ImageRef::LocalTag { name, tag } => {
                assert_eq!(name, "my-agent");
                assert_eq!(tag, "v1.0");
            }
            _ => panic!("Expected LocalTag"),
        }
    }

    #[test]
    fn test_image_ref_parse_registry() {
        let r = ImageRef::parse("pekohub.com/agents/researcher:v2.5").unwrap();
        match r {
            ImageRef::RegistryRef { host, path, tag } => {
                assert_eq!(host, "pekohub.com");
                assert_eq!(path, "agents/researcher");
                assert_eq!(tag, "v2.5");
            }
            _ => panic!("Expected RegistryRef"),
        }
    }
}
