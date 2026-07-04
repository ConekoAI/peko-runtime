//! Known Runtimes Registry
//!
//! Local registry of known peer runtimes for multi-host awareness.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};

use crate::common::paths::PathResolver;

/// Trust level for a known runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// This runtime itself
    SelfRuntime,
    /// Explicitly authorized/trusted runtime
    Authorized,
    /// Untrusted or unknown runtime
    Untrusted,
}

/// Transport preference for cross-runtime communication with a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPreference {
    /// Prefer direct if a direct endpoint is configured and trusted,
    /// otherwise fall back to the PekoHub tunnel.
    Auto,
    /// Always use the PekoHub tunnel.
    Tunnel,
    /// Always use the direct endpoint; fail if one is not configured.
    Direct,
}

impl Default for TransportPreference {
    fn default() -> Self {
        Self::Auto
    }
}

/// Per-peer TLS configuration for direct connections.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectTlsConfig {
    /// Custom CA certificate path (PEM) for verifying the peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_path: Option<PathBuf>,
    /// Client certificate path (PEM) for mTLS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_path: Option<PathBuf>,
    /// Client private key path (PEM) for mTLS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_path: Option<PathBuf>,
    /// Expected SHA-256 fingerprint (base64 or hex) of the peer's
    /// end-entity certificate for pinning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_cert_sha256: Option<String>,
    /// Allow plaintext connections for this peer. Only intended for
    /// local testing; ignored when the global `tls_required` is true.
    #[serde(default)]
    pub allow_plaintext: bool,
}

/// A known peer runtime
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownRuntime {
    /// Runtime DID
    pub runtime_id: String,
    /// Human-readable display name
    pub display_name: String,
    /// When this runtime was last seen
    pub last_seen: DateTime<Utc>,
    /// Connection endpoint (if known) — typically the PekoHub tunnel endpoint.
    pub connection_endpoint: Option<String>,
    /// Direct connection endpoint, e.g. `wss://192.168.1.10:11436`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_endpoint: Option<String>,
    /// Preferred transport for this peer.
    #[serde(default)]
    pub transport_preference: TransportPreference,
    /// TLS configuration for direct connections to this peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_tls: Option<DirectTlsConfig>,
    /// Trust level
    pub trust_level: TrustLevel,
}

/// Registry of known runtimes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownRuntimes {
    /// List of known runtimes
    pub runtimes: Vec<KnownRuntime>,
}

impl Default for KnownRuntimes {
    fn default() -> Self {
        Self::new()
    }
}

impl KnownRuntimes {
    /// Create an empty registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtimes: Vec::new(),
        }
    }

    /// Load from disk or create a new empty registry
    pub fn load_or_create(resolver: &PathResolver) -> Result<Self> {
        let registry_path = resolver.runtime_dir().join("known_runtimes.toml");

        if registry_path.exists() {
            let content = fs::read_to_string(&registry_path)
                .with_context(|| format!("Failed to read known runtimes: {registry_path:?}"))?;
            let registry: KnownRuntimes =
                toml::from_str(&content).with_context(|| "Failed to parse known_runtimes.toml")?;
            info!("Loaded known runtimes registry from: {:?}", registry_path);
            return Ok(registry);
        }

        let registry = Self::new();

        // Ensure parent directory exists
        if let Some(parent) = registry_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create runtime directory: {parent:?}"))?;
        }

        let toml = toml::to_string_pretty(&registry)
            .with_context(|| "Failed to serialize known runtimes")?;
        fs::write(&registry_path, toml)
            .with_context(|| format!("Failed to write known runtimes: {registry_path:?}"))?;

        info!(
            "Created empty known runtimes registry at: {:?}",
            registry_path
        );
        Ok(registry)
    }

    /// Save the registry to disk
    pub fn save(&self, resolver: &PathResolver) -> Result<()> {
        let registry_path = resolver.runtime_dir().join("known_runtimes.toml");

        if let Some(parent) = registry_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create runtime directory: {parent:?}"))?;
        }

        let toml =
            toml::to_string_pretty(self).with_context(|| "Failed to serialize known runtimes")?;
        fs::write(&registry_path, toml)
            .with_context(|| format!("Failed to write known runtimes: {registry_path:?}"))?;

        Ok(())
    }

    /// Register a new runtime (or update existing)
    pub fn register(
        &mut self,
        runtime_id: impl Into<String>,
        display_name: impl Into<String>,
        connection_endpoint: Option<String>,
        trust_level: TrustLevel,
    ) {
        self.register_with_direct(
            runtime_id,
            display_name,
            connection_endpoint,
            None,
            TransportPreference::Auto,
            None,
            trust_level,
        );
    }

    /// Register a new runtime (or update existing) with direct-mode fields.
    pub fn register_with_direct(
        &mut self,
        runtime_id: impl Into<String>,
        display_name: impl Into<String>,
        connection_endpoint: Option<String>,
        direct_endpoint: Option<String>,
        transport_preference: TransportPreference,
        direct_tls: Option<DirectTlsConfig>,
        trust_level: TrustLevel,
    ) {
        let runtime_id = runtime_id.into();
        let display_name = display_name.into();

        if let Some(existing) = self
            .runtimes
            .iter_mut()
            .find(|r| r.runtime_id == runtime_id)
        {
            existing.display_name = display_name;
            existing.last_seen = Utc::now();
            existing.connection_endpoint = connection_endpoint;
            existing.direct_endpoint = direct_endpoint;
            existing.transport_preference = transport_preference;
            existing.direct_tls = direct_tls;
            existing.trust_level = trust_level;
            info!("Updated known runtime: {}", runtime_id);
        } else {
            self.runtimes.push(KnownRuntime {
                runtime_id: runtime_id.clone(),
                display_name,
                last_seen: Utc::now(),
                connection_endpoint,
                direct_endpoint,
                transport_preference,
                direct_tls,
                trust_level,
            });
            info!("Registered new runtime: {}", runtime_id);
        }
    }

    /// Set the trust level for a runtime
    pub fn trust(&mut self, runtime_id: &str, trust_level: TrustLevel) -> Result<()> {
        if let Some(runtime) = self
            .runtimes
            .iter_mut()
            .find(|r| r.runtime_id == runtime_id)
        {
            runtime.trust_level = trust_level;
            info!("Set trust level for {} to {:?}", runtime_id, trust_level);
            Ok(())
        } else {
            anyhow::bail!("Runtime not found: {}", runtime_id);
        }
    }

    /// Remove a runtime from the registry
    pub fn remove(&mut self, runtime_id: &str) -> Result<()> {
        let before = self.runtimes.len();
        self.runtimes.retain(|r| r.runtime_id != runtime_id);
        if self.runtimes.len() < before {
            info!("Removed runtime from registry: {}", runtime_id);
            Ok(())
        } else {
            warn!("Tried to remove unknown runtime: {}", runtime_id);
            anyhow::bail!("Runtime not found: {}", runtime_id);
        }
    }

    /// List all known runtimes
    #[must_use]
    pub fn list(&self) -> &[KnownRuntime] {
        &self.runtimes
    }

    /// Find a runtime by ID
    #[must_use]
    pub fn find(&self, runtime_id: &str) -> Option<&KnownRuntime> {
        self.runtimes.iter().find(|r| r.runtime_id == runtime_id)
    }

    /// Find a runtime by ID (mutable)
    pub fn find_mut(&mut self, runtime_id: &str) -> Option<&mut KnownRuntime> {
        self.runtimes
            .iter_mut()
            .find(|r| r.runtime_id == runtime_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_new_runtime() {
        let mut registry = KnownRuntimes::new();
        registry.register(
            "did:key:z6MkA",
            "Runtime A",
            Some("tcp://192.168.1.1:8080".to_string()),
            TrustLevel::Authorized,
        );

        assert_eq!(registry.runtimes.len(), 1);
        assert_eq!(registry.runtimes[0].runtime_id, "did:key:z6MkA");
        assert_eq!(registry.runtimes[0].trust_level, TrustLevel::Authorized);
    }

    #[test]
    fn test_register_update_existing() {
        let mut registry = KnownRuntimes::new();
        registry.register("did:key:z6MkA", "Runtime A", None, TrustLevel::Untrusted);
        registry.register(
            "did:key:z6MkA",
            "Runtime A Updated",
            Some("tcp://host:8080".to_string()),
            TrustLevel::Authorized,
        );

        assert_eq!(registry.runtimes.len(), 1);
        assert_eq!(registry.runtimes[0].display_name, "Runtime A Updated");
        assert_eq!(registry.runtimes[0].trust_level, TrustLevel::Authorized);
        assert_eq!(
            registry.runtimes[0].connection_endpoint,
            Some("tcp://host:8080".to_string())
        );
    }

    #[test]
    fn test_trust_and_remove() {
        let mut registry = KnownRuntimes::new();
        registry.register("did:key:z6MkA", "Runtime A", None, TrustLevel::Untrusted);

        registry
            .trust("did:key:z6MkA", TrustLevel::Authorized)
            .unwrap();
        assert_eq!(registry.runtimes[0].trust_level, TrustLevel::Authorized);

        registry.remove("did:key:z6MkA").unwrap();
        assert!(registry.runtimes.is_empty());
    }

    #[test]
    fn test_remove_unknown() {
        let mut registry = KnownRuntimes::new();
        assert!(registry.remove("did:key:z6MkUnknown").is_err());
    }

    #[test]
    fn test_trust_unknown() {
        let mut registry = KnownRuntimes::new();
        assert!(registry
            .trust("did:key:z6MkUnknown", TrustLevel::Authorized)
            .is_err());
    }

    #[test]
    fn test_find() {
        let mut registry = KnownRuntimes::new();
        registry.register("did:key:z6MkA", "Runtime A", None, TrustLevel::Authorized);

        assert!(registry.find("did:key:z6MkA").is_some());
        assert!(registry.find("did:key:z6MkB").is_none());
    }

    #[test]
    fn test_list() {
        let mut registry = KnownRuntimes::new();
        registry.register("did:key:z6MkA", "Runtime A", None, TrustLevel::Authorized);
        registry.register("did:key:z6MkB", "Runtime B", None, TrustLevel::Untrusted);

        let list = registry.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_register_with_direct() {
        let mut registry = KnownRuntimes::new();
        registry.register_with_direct(
            "did:key:z6MkA",
            "Runtime A",
            Some("wss://hub.peko/runtime-a".to_string()),
            Some("tls://192.168.1.10:11436".to_string()),
            TransportPreference::Direct,
            Some(DirectTlsConfig {
                ca_path: Some(PathBuf::from("/etc/peko/ca.crt")),
                cert_path: Some(PathBuf::from("/etc/peko/client.crt")),
                key_path: Some(PathBuf::from("/etc/peko/client.key")),
                pinned_cert_sha256: Some("abc123".to_string()),
                allow_plaintext: false,
            }),
            TrustLevel::Authorized,
        );

        let runtime = registry.find("did:key:z6MkA").unwrap();
        assert_eq!(runtime.transport_preference, TransportPreference::Direct);
        assert_eq!(
            runtime.direct_endpoint,
            Some("tls://192.168.1.10:11436".to_string())
        );
        assert!(runtime.direct_tls.is_some());
        let tls = runtime.direct_tls.as_ref().unwrap();
        assert_eq!(tls.pinned_cert_sha256.as_deref(), Some("abc123"));
        assert!(!tls.allow_plaintext);
    }

    #[test]
    fn test_serde_roundtrip_with_direct_fields() {
        let mut registry = KnownRuntimes::new();
        registry.register_with_direct(
            "did:key:z6MkA",
            "Runtime A",
            None,
            Some("wss://192.168.1.10:11436".to_string()),
            TransportPreference::Auto,
            Some(DirectTlsConfig {
                ca_path: None,
                cert_path: None,
                key_path: None,
                pinned_cert_sha256: Some("sha256-of-cert".to_string()),
                allow_plaintext: true,
            }),
            TrustLevel::Authorized,
        );

        let toml_str = toml::to_string_pretty(&registry).unwrap();
        let parsed: KnownRuntimes = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.runtimes.len(), 1);
        let runtime = &parsed.runtimes[0];
        assert_eq!(runtime.runtime_id, "did:key:z6MkA");
        assert_eq!(runtime.transport_preference, TransportPreference::Auto);
        assert_eq!(
            runtime.direct_endpoint,
            Some("wss://192.168.1.10:11436".to_string())
        );
        let tls = runtime.direct_tls.as_ref().unwrap();
        assert_eq!(tls.pinned_cert_sha256.as_deref(), Some("sha256-of-cert"));
        assert!(tls.allow_plaintext);
    }

    #[test]
    fn test_serde_migrates_old_format() {
        // Old known_runtimes.toml files do not contain direct-mode fields.
        let old_toml = r#"
[[runtimes]]
runtime_id = "did:key:z6MkA"
display_name = "Runtime A"
last_seen = "2024-01-01T00:00:00Z"
trust_level = "authorized"
"#;
        let parsed: KnownRuntimes = toml::from_str(old_toml).unwrap();
        assert_eq!(parsed.runtimes.len(), 1);
        assert_eq!(
            parsed.runtimes[0].transport_preference,
            TransportPreference::Auto
        );
        assert!(parsed.runtimes[0].direct_endpoint.is_none());
        assert!(parsed.runtimes[0].direct_tls.is_none());
    }
}
