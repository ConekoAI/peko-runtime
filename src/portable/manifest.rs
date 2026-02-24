//! Agent manifest for portable agent packages
//!
//! Defines the structure of the manifest.toml file inside .agent packages

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Agent manifest - defines metadata for a portable agent package
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    /// Agent metadata
    pub agent: AgentMetadata,
    /// Identity configuration
    pub identity: IdentityConfig,
    /// Memory configuration
    pub memory: MemoryConfig,
    /// Capabilities exported with this agent
    pub capabilities: CapabilitiesConfig,
    /// Tools required by this agent
    pub tools: ToolsConfig,
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
    pub pekobot_version: String,
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

/// Memory configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Memory type (sqlite, json, etc.)
    pub memory_type: String,
    /// Whether memory is encrypted
    pub encrypted: bool,
    /// Size in bytes
    pub size_bytes: u64,
    /// Number of entries (if known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_count: Option<u64>,
}

/// Capabilities configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesConfig {
    /// List of capability names
    pub names: Vec<String>,
    /// Capability versions (name -> version)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub versions: Option<HashMap<String, String>>,
}

/// Tools configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// List of required tool names
    pub required: Vec<String>,
    /// Tool versions (name -> version)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub versions: Option<HashMap<String, String>>,
    /// Optional tools (can be missing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional: Option<Vec<String>>,
}

/// Packaging metadata section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagingMetadata {
    /// List of files in the package (relative paths)
    pub files: Vec<String>,
    /// Checksums for each file (path -> "sha256:...")
    pub checksums: HashMap<String, String>,
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
                export_format: "1.0".to_string(),
                did: did.into(),
                pekobot_version: crate::VERSION.to_string(),
            },
            identity: IdentityConfig {
                key_algorithm: "ed25519".to_string(),
                encrypted: false,
                kdf: None,
                kdf_params: None,
            },
            memory: MemoryConfig {
                memory_type: "sqlite".to_string(),
                encrypted: false,
                size_bytes: 0,
                entry_count: None,
            },
            capabilities: CapabilitiesConfig {
                names: Vec::new(),
                versions: None,
            },
            tools: ToolsConfig {
                required: Vec::new(),
                versions: None,
                optional: None,
            },
            packaging: PackagingMetadata {
                files: Vec::new(),
                checksums: HashMap::new(),
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
        self.packaging.files.push(path.clone());
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
        let manifest = AgentManifest::new("test-agent", "1.0.0", "did:pekobot:test");
        assert_eq!(manifest.agent.name, "test-agent");
        assert_eq!(manifest.agent.version, "1.0.0");
        assert_eq!(manifest.agent.did, "did:pekobot:test");
        assert!(!manifest.identity.encrypted);
    }

    #[test]
    fn test_manifest_serialization() {
        let mut manifest = AgentManifest::new("test-agent", "1.0.0", "did:pekobot:test");
        manifest.capabilities.names = vec!["test".to_string()];
        manifest.add_file("test.txt", b"hello world");

        let toml = manifest.to_toml().unwrap();
        assert!(toml.contains("name = \"test-agent\""));
        assert!(toml.contains("did = \"did:pekobot:test\""));

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
