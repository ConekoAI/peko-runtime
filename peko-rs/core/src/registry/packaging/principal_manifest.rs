//! Principal manifest for portable `.principal` packages
//!
//! Mirrors the shape of the agent manifest but names the top-level metadata
//! section `principal` and uses principal-specific layer names
//! (`agents`, `memory`) in addition to the shared `config`, `identity`,
//! `sessions`, and `extensions` layers.

use crate::registry::packaging::manifest::{IdentityConfig, PackagingMetadata, Signatures};
use crate::registry::packaging::types::ExtensionRef;
use serde::{Deserialize, Serialize};

/// Content-addressable layer digests for `.principal` packages.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrincipalLayers {
    /// Config layer digest (`config/principal.toml`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    /// Identity layer digest (`identity/did.json`, `identity/keys.enc`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Agent prompt layer digest (`agents/*.md`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<String>,
    /// Memory layer digest (`memory/`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// Session history layer digest (`sessions/`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<String>,
    /// Embedded extension packages layer digest (`extensions/*.ext`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<String>,
}

/// Principal manifest - packaging metadata for a portable Principal package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalManifest {
    /// Principal metadata
    pub principal: PrincipalMetadata,
    /// Identity configuration
    pub identity: IdentityConfig,
    /// Content-addressable layer digests
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layers: Option<PrincipalLayers>,
    /// Extension dependencies required by this Principal
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<ExtensionRef>,
    /// Packaging metadata
    pub packaging: PackagingMetadata,
    /// Digital signatures
    pub signatures: Signatures,
}

/// Principal metadata section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalMetadata {
    /// Principal name
    pub name: String,
    /// Package version (semver)
    pub version: String,
    /// Human-readable description
    pub description: Option<String>,
    /// Creation timestamp (RFC 3339)
    pub created_at: String,
    /// Export format version
    pub export_format: String,
    /// Principal DID
    pub did: String,
    /// Original runtime version that created this package
    pub peko_version: String,
}

impl PrincipalManifest {
    /// Create a new manifest with default values.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        did: impl Into<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        let name = name.into();

        Self {
            principal: PrincipalMetadata {
                name: name.clone(),
                version: version.into(),
                description: None,
                created_at: now,
                export_format: "1.0".to_string(),
                did: did.into(),
                peko_version: crate::VERSION.to_string(),
            },
            identity: IdentityConfig {
                key_algorithm: "ed25519".to_string(),
                encrypted: false,
                kdf: None,
                kdf_params: None,
            },
            layers: None,
            extensions: Vec::new(),
            packaging: PackagingMetadata {
                files: Vec::new(),
                checksums: std::collections::BTreeMap::new(),
                compression: "gzip".to_string(),
                archive_format: "tar".to_string(),
            },
            signatures: Signatures {
                manifest: String::new(),
                algorithm: "ed25519".to_string(),
            },
        }
    }

    /// Serialize to TOML string.
    pub fn to_toml(&self) -> anyhow::Result<String> {
        toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize principal manifest: {e}"))
    }

    /// Deserialize from TOML string.
    pub fn from_toml(toml_str: &str) -> anyhow::Result<Self> {
        toml::from_str(toml_str)
            .map_err(|e| anyhow::anyhow!("Failed to parse principal manifest: {e}"))
    }

    /// Compute checksum for a file.
    #[must_use]
    pub fn compute_checksum(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Add a file to the manifest (sorted for signature determinism).
    pub fn add_file(&mut self, path: impl Into<String>, data: &[u8]) {
        let path = path.into();
        let checksum = Self::compute_checksum(data);
        let pos = self
            .packaging
            .files
            .binary_search(&path)
            .unwrap_or_else(|e| e);
        self.packaging.files.insert(pos, path.clone());
        self.packaging.checksums.insert(path, checksum);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_principal_manifest_creation() {
        let manifest = PrincipalManifest::new("test-principal", "1.0.0", "did:peko:test");
        assert_eq!(manifest.principal.name, "test-principal");
        assert_eq!(manifest.principal.version, "1.0.0");
        assert_eq!(manifest.principal.did, "did:peko:test");
        assert!(manifest.layers.is_none());
    }

    #[test]
    fn test_principal_manifest_serialization() {
        let mut manifest = PrincipalManifest::new("test-principal", "1.0.0", "did:peko:test");
        manifest.add_file("config/principal.toml", b"[principal]\nname = \"test\"");

        let toml = manifest.to_toml().unwrap();
        assert!(toml.contains("name = \"test-principal\""));
        assert!(toml.contains("did = \"did:peko:test\""));

        let parsed = PrincipalManifest::from_toml(&toml).unwrap();
        assert_eq!(parsed.principal.name, "test-principal");
    }

    #[test]
    fn test_principal_layers_roundtrip() {
        let layers = PrincipalLayers {
            config: Some("sha256:abc".to_string()),
            identity: Some("sha256:def".to_string()),
            agents: Some("sha256:ghi".to_string()),
            memory: None,
            sessions: None,
            extensions: Some("sha256:jkl".to_string()),
        };

        let toml = toml::to_string(&layers).unwrap();
        assert!(toml.contains("agents"));
        assert!(!toml.contains("memory"));

        let parsed: PrincipalLayers = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.agents, Some("sha256:ghi".to_string()));
    }
}
