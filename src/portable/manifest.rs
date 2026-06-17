//! Agent manifest for portable agent packages
//!
//! Defines the structure of the manifest.toml file inside .agent packages

use crate::portable::types::ExtensionRef;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Content-addressable layer digests for .agent packages
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentLayers {
    /// Config layer digest (contains agent.toml — single source of truth)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    /// Identity layer digest
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Skills layer digest (deprecated — retained for reading legacy packages).
    ///
    /// Under ADR-037, skills are managed as `skill` extensions and are
    /// recorded in `AgentManifest.extensions`. New exports no longer emit
    /// this layer digest, but it is preserved here so that legacy packages
    /// can still be deserialized and validated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<String>,
    /// Workspace layer digest
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Sessions layer digest
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<String>,
    /// MCP layer digest (deprecated — retained for reading legacy packages).
    ///
    /// Under ADR-037, MCP servers are managed as `mcp` extensions and are
    /// recorded in `AgentManifest.extensions`. New exports no longer emit
    /// this layer digest, but it is preserved here so that legacy packages
    /// can still be deserialized and validated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<String>,
    /// Extensions layer digest (optional composite bundle).
    ///
    /// Contains embedded `.ext` packages for air-gapped sharing.
    /// Only present when the package was created with `--with-extensions`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<String>,
}

/// Agent manifest - defines packaging metadata for a portable agent package
///
/// Contains ONLY packaging metadata (file lists, checksums, layer digests).
/// Agent behavior configuration (tools, MCP, skills, capabilities) lives in
/// agent.toml inside the config layer — the single source of truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Agent metadata
    pub agent: AgentMetadata,
    /// Identity configuration
    pub identity: IdentityConfig,
    /// Content-addressable layer digests
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layers: Option<AgentLayers>,
    /// Extension dependencies required by this agent
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<ExtensionRef>,
    /// Packaging metadata
    pub packaging: PackagingMetadata,
    /// Digital signatures
    pub signatures: Signatures,
}

/// Agent metadata section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    /// Agent name
    pub name: String,
    /// Package version (semver)
    pub version: String,
    /// Human-readable description
    pub description: Option<String>,
    /// Creation timestamp (RFC 3339)
    pub created_at: String,
    /// Export format version
    pub export_format: String,
    /// Agent DID
    pub did: String,
    /// Original runtime version that created this package
    pub peko_version: String,
}

/// Identity configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityConfig {
    /// Key algorithm used (ed25519)
    pub key_algorithm: String,
    /// Whether keys are encrypted
    pub encrypted: bool,
    /// Key derivation function used (if encrypted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kdf: Option<String>,
    /// KDF parameters (salt, memory cost, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kdf_params: Option<HashMap<String, String>>,
}

/// Packaging metadata section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagingMetadata {
    /// List of files in the package (relative paths)
    pub files: Vec<String>,
    /// Checksums for each file (path -> "sha256:...").
    ///
    /// `BTreeMap` is required for signature determinism (issue #14).
    /// `HashMap` iteration order is randomized per-instance; if the
    /// packager and verifier serialize this map in different orders,
    /// the manifest bytes they each produce will not match and the
    /// signature check will fail spuriously. `BTreeMap` sorts by key
    /// and is stable across the serde round-trip.
    pub checksums: BTreeMap<String, String>,
    /// Compression format
    pub compression: String,
    /// Archive format
    pub archive_format: String,
}

/// Digital signatures section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signatures {
    /// Manifest signature (signed by agent's DID key)
    pub manifest: String,
    /// Signature algorithm
    pub algorithm: String,
}

impl AgentManifest {
    /// Create a new manifest with default values
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        did: impl Into<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        let name = name.into();

        Self {
            agent: AgentMetadata {
                name: name.clone(),
                version: version.into(),
                description: None,
                created_at: now,
                export_format: "1.2".to_string(), // ADR-037: extension dependency tracking
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
                checksums: BTreeMap::new(),
                compression: "gzip".to_string(),
                archive_format: "tar".to_string(),
            },
            signatures: Signatures {
                manifest: String::new(),
                algorithm: "ed25519".to_string(),
            },
        }
    }

    /// Serialize to TOML string
    pub fn to_toml(&self) -> anyhow::Result<String> {
        toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize manifest: {e}"))
    }

    /// Deserialize from TOML string
    pub fn from_toml(toml_str: &str) -> anyhow::Result<Self> {
        toml::from_str(toml_str).map_err(|e| anyhow::anyhow!("Failed to parse manifest: {e}"))
    }

    /// Compute checksum for a file
    #[must_use]
    pub fn compute_checksum(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Verify a file against its checksum
    #[must_use]
    pub fn verify_checksum(data: &[u8], expected: &str) -> bool {
        let computed = Self::compute_checksum(data);
        computed == expected
    }

    /// Add a file to the manifest
    pub fn add_file(&mut self, path: impl Into<String>, data: &[u8]) {
        let path = path.into();
        let checksum = Self::compute_checksum(data);
        // Maintain `packaging.files` in sorted order on every insert.
        // This is required for signature determinism (issue #14):
        // the packager signs the manifest, then [`crate::portable::registry::AgentRegistry::export_package`]
        // re-serializes the manifest after a registry round-trip and
        // it sorts the file list — if the original bytes were in
        // insertion order, the re-serialized bytes would differ and
        // signature verification would fail. Keeping the list sorted
        // at all times makes both paths produce identical bytes.
        let pos = self
            .packaging
            .files
            .binary_search(&path)
            .unwrap_or_else(|e| e);
        self.packaging.files.insert(pos, path.clone());
        // BTreeMap insert — see [`PackagingMetadata::checksums`] for
        // why we need sorted keys (signature determinism).
        self.packaging.checksums.insert(path, checksum);
    }

    /// Set encryption configuration
    pub fn set_encrypted(&mut self, kdf: impl Into<String>, params: HashMap<String, String>) {
        self.identity.encrypted = true;
        self.identity.kdf = Some(kdf.into());
        self.identity.kdf_params = Some(params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_creation() {
        let manifest = AgentManifest::new("test-agent", "1.0.0", "did:peko:test");
        assert_eq!(manifest.agent.name, "test-agent");
        assert_eq!(manifest.agent.version, "1.0.0");
        assert_eq!(manifest.agent.did, "did:peko:test");
        assert!(!manifest.identity.encrypted);
        assert!(manifest.layers.is_none());
    }

    #[test]
    fn test_manifest_serialization() {
        let mut manifest = AgentManifest::new("test-agent", "1.0.0", "did:peko:test");
        manifest.add_file("test.txt", b"hello world");

        let toml = manifest.to_toml().unwrap();
        assert!(toml.contains("name = \"test-agent\""));
        assert!(toml.contains("did = \"did:peko:test\""));

        let parsed = AgentManifest::from_toml(&toml).unwrap();
        assert_eq!(parsed.agent.name, "test-agent");
    }

    #[test]
    fn test_checksum() {
        let data = b"test data";
        let checksum = AgentManifest::compute_checksum(data);
        assert!(checksum.starts_with("sha256:"));
        assert!(AgentManifest::verify_checksum(data, &checksum));
        assert!(!AgentManifest::verify_checksum(b"wrong data", &checksum));
    }
}

    #[test]
    fn test_manifest_with_extension_refs_roundtrip() {
        use crate::portable::types::ExtensionRef;

        let mut manifest = AgentManifest::new("test-agent", "1.0.0", "did:peko:test");
        manifest.extensions = vec![
            ExtensionRef {
                id: "docker-skill".to_string(),
                registry_ref: "pekohub.com/extensions/docker-skill:latest".to_string(),
            },
            ExtensionRef {
                id: "filesystem-mcp".to_string(),
                registry_ref: "pekohub.com/extensions/filesystem-mcp:v1.2.0".to_string(),
            },
        ];

        let toml = manifest.to_toml().unwrap();
        assert!(toml.contains("docker-skill"));
        assert!(toml.contains("pekohub.com/extensions/docker-skill:latest"));

        let parsed = AgentManifest::from_toml(&toml).unwrap();
        assert_eq!(parsed.extensions.len(), 2);
        assert_eq!(parsed.extensions[0].id, "docker-skill");
        assert_eq!(
            parsed.extensions[0].registry_ref,
            "pekohub.com/extensions/docker-skill:latest"
        );
    }

    #[test]
    fn test_agent_layers_extensions_roundtrip() {
        let layers = AgentLayers {
            config: Some("sha256:abc".to_string()),
            identity: Some("sha256:def".to_string()),
            skills: None,
            workspace: Some("sha256:ghi".to_string()),
            sessions: None,
            mcp: None,
            extensions: Some("sha256:jkl".to_string()),
        };

        let toml = toml::to_string(&layers).unwrap();
        assert!(toml.contains("extensions"));
        assert!(!toml.contains("skills"));
        assert!(!toml.contains("mcp"));

        let parsed: AgentLayers = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.extensions, Some("sha256:jkl".to_string()));
    }

    #[test]
    fn test_manifest_export_format_is_1_2() {
        let manifest = AgentManifest::new("test-agent", "1.0.0", "did:peko:test");
        assert_eq!(manifest.agent.export_format, "1.2");
    }
