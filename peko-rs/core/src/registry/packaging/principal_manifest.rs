//! Principal manifest for portable `.principal` packages
//!
//! Mirrors the shape of the agent manifest but names the top-level metadata
//! section `principal` and uses principal-specific layer names
//! (`agents`, `memory`) in addition to the shared `config`, `identity`,
//! `sessions`, and `plugins` layers. The legacy `extensions` layer is
//! retained for reading pre-Phase-7 packages.

use crate::registry::packaging::manifest::{IdentityConfig, PackagingMetadata, Signatures};
use crate::registry::packaging::types::ExtensionRef;
use serde::{Deserialize, Serialize};

/// What a `.principal` package carries (ADR-056).
///
/// The two modes implement the packaging/portability axis, which is
/// deliberately decoupled from the Local/Shared storage-tier axis
/// (access semantics — `common::paths`):
///
/// - [`ExportMode::Definition`] — Shared-tier capability-bearing
///   config only (`config/`, `identity/`, `agents/`). This is the
///   historical behavior and the right shape for sharing a principal
///   as a template or pushing it to a registry: sessions, cron, and
///   plans do NOT travel. Sessions can still be requested explicitly
///   via `PrincipalExportOptions::include_sessions`.
/// - [`ExportMode::FullSnapshot`] — everything that constitutes the
///   principal's live existence: the Definition layers plus the
///   identity-bearing Local-tier artifacts (`sessions/`, `cron/`,
///   `plans/`) and the workspace tooling the principal installed
///   (`tools/`, `skills/`, `mcp/`, `hooks/`, `kb/`). Derived Local
///   state (`cache/`, `locks/`, `memory_index.json`) is never
///   packaged — it is rebuilt by the runtime on import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExportMode {
    /// Shared-tier definition only (historical default).
    #[default]
    Definition,
    /// Full live snapshot: definition + identity-bearing local state
    /// + workspace tooling (ADR-056).
    FullSnapshot,
}

/// `skip_serializing_if` helper: the default mode is omitted from the
/// manifest TOML so legacy packages (which predate the field) and new
/// definition-mode packages serialize identically.
#[must_use]
pub fn export_mode_is_default(mode: &ExportMode) -> bool {
    matches!(mode, ExportMode::Definition)
}

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
    /// Cron layer digest (`cron/`) — the principal's authored schedule
    /// (`local/cron/schedule.toml` + run history), ADR-056.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// Plans layer digest (`plans/`) — the principal's authored Plan
    /// DAG storage (`local/plans/`), ADR-056.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plans: Option<String>,
    /// Universal tools layer digest (`tools/<id>/`) — ADR-056.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
    /// Skills layer digest (`skills/<id>/`) — ADR-056 (workspace
    /// tooling; reuses the legacy agent-package layer name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<String>,
    /// MCP layer digest (`mcp/<id>/`) — ADR-056 (workspace tooling;
    /// reuses the legacy agent-package layer name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<String>,
    /// Hooks layer digest (`hooks/<id>/`) — ADR-056.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<String>,
    /// Knowledge base layer digest (`kb/`) — ADR-056 (ADR-055 tree).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb: Option<String>,
    /// Plugins layer digest (`plugins/<plugin-id>/`) — ADR-047 §2.1.
    ///
    /// Replaces the legacy `extensions` layer. New exports emit this
    /// field; legacy packages that declare `extensions` are still
    /// accepted on import.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugins: Option<String>,
    /// Extensions layer digest (`extensions/*.ext`) — legacy.
    ///
    /// Pre-ADR-047 packages populated this field. New exports never emit
    /// it; the unpackager accepts it and routes its content to the same
    /// handler as the `plugins` layer.
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
    /// What the package carries (ADR-056). Defaults to
    /// [`ExportMode::Definition`]; omitted from the TOML when default
    /// so legacy packages parse unchanged.
    #[serde(default, skip_serializing_if = "export_mode_is_default")]
    pub export_mode: ExportMode,
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
            export_mode: ExportMode::default(),
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
            cron: None,
            plans: None,
            tools: None,
            skills: None,
            mcp: None,
            hooks: None,
            kb: None,
            plugins: Some("sha256:pqr".to_string()),
            extensions: Some("sha256:jkl".to_string()),
        };

        let toml = toml::to_string(&layers).unwrap();
        assert!(toml.contains("agents"));
        assert!(toml.contains("plugins"));
        assert!(!toml.contains("memory"));

        let parsed: PrincipalLayers = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.agents, Some("sha256:ghi".to_string()));
        assert_eq!(parsed.plugins, Some("sha256:pqr".to_string()));
        assert_eq!(parsed.extensions, Some("sha256:jkl".to_string()));
    }

    /// Phase 7 (ADR-047 §5): legacy `.principal` packages that declare
    /// `extensions = "sha256:..."` but not `plugins` continue to
    /// deserialize cleanly. The new field defaults to `None`.
    #[test]
    fn test_principal_layers_accepts_legacy_extensions_only() {
        let legacy_toml = r#"
config = "sha256:abc"
identity = "sha256:def"
agents = "sha256:ghi"
extensions = "sha256:jkl"
"#;
        let parsed: PrincipalLayers = toml::from_str(legacy_toml).unwrap();
        assert_eq!(parsed.agents, Some("sha256:ghi".to_string()));
        assert_eq!(parsed.extensions, Some("sha256:jkl".to_string()));
        assert!(parsed.plugins.is_none());
        assert!(parsed.memory.is_none());
        assert!(parsed.sessions.is_none());
    }

    /// Phase 7 (ADR-047 §5): new exports emit `plugins = ...` but skip
    /// the legacy `extensions` field (skip_serializing_if drops `None`s,
    /// and `Default` for the deprecated field is `None`).
    #[test]
    fn test_principal_layers_plugins_only_emits_no_legacy_field() {
        let layers = PrincipalLayers {
            config: Some("sha256:abc".to_string()),
            identity: Some("sha256:def".to_string()),
            agents: Some("sha256:ghi".to_string()),
            memory: None,
            sessions: None,
            cron: None,
            plans: None,
            tools: None,
            skills: None,
            mcp: None,
            hooks: None,
            kb: None,
            plugins: Some("sha256:pqr".to_string()),
            extensions: None,
        };

        let toml = toml::to_string(&layers).unwrap();
        assert!(toml.contains("plugins"));
        assert!(
            !toml.contains("extensions"),
            "legacy field must not be emitted: {toml}"
        );

        let parsed: PrincipalLayers = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.plugins, Some("sha256:pqr".to_string()));
        assert!(parsed.extensions.is_none());
    }

    /// ADR-056: the default export mode is omitted from the manifest
    /// TOML, so pre-ADR-056 packages (no `export_mode` field) parse
    /// unchanged and read as [`ExportMode::Definition`].
    #[test]
    fn test_manifest_without_export_mode_parses_as_definition() {
        let manifest = PrincipalManifest::new("test", "1.0.0", "did:peko:test");
        let toml = manifest.to_toml().unwrap();
        assert!(
            !toml.contains("export_mode"),
            "default mode must be omitted: {toml}"
        );
        let parsed = PrincipalManifest::from_toml(&toml).unwrap();
        assert_eq!(parsed.export_mode, ExportMode::Definition);
    }

    /// ADR-056: a full-snapshot manifest round-trips its mode verbatim.
    #[test]
    fn test_manifest_full_snapshot_mode_roundtrips() {
        let mut manifest = PrincipalManifest::new("test", "1.0.0", "did:peko:test");
        manifest.export_mode = ExportMode::FullSnapshot;
        let toml = manifest.to_toml().unwrap();
        assert!(toml.contains("export_mode = \"full_snapshot\""), "{toml}");
        let parsed = PrincipalManifest::from_toml(&toml).unwrap();
        assert_eq!(parsed.export_mode, ExportMode::FullSnapshot);
    }
}
