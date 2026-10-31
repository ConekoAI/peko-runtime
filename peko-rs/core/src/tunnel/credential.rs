//! PekoHub registration metadata.
//!
//! `pekohub.toml` records only where this runtime is registered: the tunnel
//! URL and optional TLS settings. The runtime is known to Hub by its one
//! identity (ADR-081 D2) — the vault-held `did:key` in
//! `runtime/identity.toml` — which signs the tunnel handshake, registration,
//! and every cross-runtime envelope. There is no separate tunnel key.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::common::paths::PathResolver;

/// Optional TLS configuration for the PekoHub WebSocket tunnel.
///
/// When absent, the tunnel uses the default rustls/WebPKI trust store.
/// When present, the runtime can be configured to use a custom CA,
/// present a client certificate (mTLS), and/or pin the hub certificate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelTlsConfig {
    /// Path to a PEM-encoded custom CA certificate.
    pub ca_path: Option<PathBuf>,
    /// Path to a PEM-encoded client certificate for mTLS.
    pub cert_path: Option<PathBuf>,
    /// Path to a PEM-encoded private key for the client certificate.
    pub key_path: Option<PathBuf>,
    /// Base64-encoded SHA-256 fingerprint of the expected hub
    /// end-entity certificate (SPKI pin).
    pub pinned_cert_sha256: Option<String>,
}

/// Where this runtime is registered with PekoHub.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PekoHubCredential {
    /// WebSocket tunnel URL
    pub url: String,
    /// The runtime identity DID (ADR-081 D2). Never stored here: callers
    /// set it from `RuntimeIdentity` when loading.
    #[serde(skip)]
    pub runtime_id: String,
    /// Optional TLS configuration for the tunnel connection.
    #[serde(default)]
    pub tls: Option<TunnelTlsConfig>,
}

impl PekoHubCredential {
    /// Load credential from the given path
    ///
    /// # Errors
    /// Returns error if file cannot be read or parsed
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read PekoHub credential: {path:?}"))?;
        let mut cred: Self = toml::from_str(&content).with_context(|| {
            format!(
                "Failed to parse PekoHub credential {path:?}; a credential from \
                 before the single runtime identity (ADR-081) must be replaced: \
                 run `peko runtime unregister` then `peko runtime register`"
            )
        })?;
        cred.url = official_hub_url(&cred.url);
        Ok(cred)
    }

    /// Save credential to the given path
    ///
    /// # Errors
    /// Returns error if file cannot be written
    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {parent:?}"))?;
        }
        let toml = toml::to_string_pretty(self)
            .with_context(|| "Failed to serialize PekoHub credential")?;
        std::fs::write(path, toml)
            .with_context(|| format!("Failed to write PekoHub credential: {path:?}"))?;

        // Set restrictive permissions (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)?;
        }

        Ok(())
    }

    /// Get the default credential file path
    ///
    /// Path: `{config_dir}/runtime/pekohub.toml` where `{config_dir}` is the
    /// `PEKO_HOME` env var (if set) or `~/.peko`.
    #[must_use]
    pub fn default_path() -> PathBuf {
        PathResolver::default().pekohub_config()
    }

    /// Get the credential file path for a given config directory.
    #[must_use]
    pub fn path_for_config_dir(config_dir: &Path) -> PathBuf {
        PathResolver::with_dirs(
            config_dir.to_path_buf(),
            config_dir.join("data"),
            config_dir.join("cache"),
        )
        .pekohub_config()
    }
}

/// Derive the hub HTTP origin from a tunnel WebSocket URL
/// (`wss://host/v1/tunnel` → `https://host`). Used to locate the
/// hub's JWKS endpoint and as the expected bridge-token issuer.
#[must_use]
pub fn hub_origin(tunnel_url: &str) -> Option<String> {
    let (rest, scheme) = if let Some(rest) = tunnel_url.strip_prefix("wss://") {
        (rest, "https://")
    } else if let Some(rest) = tunnel_url.strip_prefix("ws://") {
        (rest, "http://")
    } else {
        return None;
    };
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}{authority}"))
}

/// Load the PekoHub registration for the runtime identified by
/// `runtime_did`, from the default location or a custom path.
///
/// Returns `None` if this runtime is not registered.
pub fn load_pekohub_credential(
    custom_path: Option<&Path>,
    runtime_did: &str,
) -> Result<Option<PekoHubCredential>> {
    let path = custom_path.map_or_else(PekoHubCredential::default_path, PathBuf::from);

    if !path.exists() {
        info!("No PekoHub credential found at: {}", path.display());
        return Ok(None);
    }

    let mut cred = PekoHubCredential::from_file(&path)?;
    cred.runtime_id = runtime_did.to_string();
    info!(
        "Loaded PekoHub registration for runtime: {}",
        cred.runtime_id
    );
    Ok(Some(cred))
}

/// The official hub moved from `pekohub.org` to `pekohub.ai`, and the
/// old name now redirects. A tunnel can't follow a redirect, and the
/// hub signs bridge tokens as the new origin, so a registration made
/// against the old name is read as the new one.
#[must_use]
pub fn official_hub_url(url: &str) -> String {
    for scheme in ["wss://", "https://"] {
        if let Some(rest) = url.strip_prefix(&format!("{scheme}pekohub.org")) {
            if rest.is_empty() || rest.starts_with('/') || rest.starts_with(':') {
                return format!("{scheme}pekohub.ai{rest}");
            }
        }
    }
    url.to_string()
}

/// Check if PekoHub credentials exist
#[must_use]
pub fn has_pekohub_credential() -> bool {
    PekoHubCredential::default_path().exists()
}

/// The origin of the hub this runtime is registered with
/// (`wss://pekohub.ai/v1/tunnel` → `https://pekohub.ai`), or `None`
/// when it isn't registered or the credential can't be read.
#[must_use]
pub fn registered_hub_origin() -> Option<String> {
    load_pekohub_credential(None, "")
        .ok()
        .flatten()
        .and_then(|credential| hub_origin(&credential.url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_registration_on_the_old_hub_name_reads_as_the_new_one() {
        assert_eq!(
            official_hub_url("wss://pekohub.org/v1/tunnel"),
            "wss://pekohub.ai/v1/tunnel"
        );
        assert_eq!(
            official_hub_url("https://pekohub.org"),
            "https://pekohub.ai"
        );
        // Other hubs, and names that only start the same, are left alone.
        assert_eq!(
            official_hub_url("ws://localhost:3000/v1/tunnel"),
            "ws://localhost:3000/v1/tunnel"
        );
        assert_eq!(
            official_hub_url("wss://pekohub.org.example/v1/tunnel"),
            "wss://pekohub.org.example/v1/tunnel"
        );
    }

    #[test]
    fn hub_origin_derives_http_origins_from_tunnel_urls() {
        assert_eq!(
            hub_origin("wss://pekohub.org/v1/tunnel").as_deref(),
            Some("https://pekohub.org")
        );
        assert_eq!(
            hub_origin("ws://localhost:4000/v1/tunnel").as_deref(),
            Some("http://localhost:4000")
        );
        assert_eq!(hub_origin("https://not-a-ws-url"), None);
        assert_eq!(hub_origin("wss://"), None);
    }

    #[test]
    fn test_credential_roundtrip() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pekohub.toml");

        let cred = PekoHubCredential {
            url: "wss://pekohub.ai/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
            tls: None,
        };

        cred.save_to_file(&path).unwrap();
        let loaded = load_pekohub_credential(Some(&path), "did:key:z6MkRuntime")
            .unwrap()
            .unwrap();

        assert_eq!(loaded.url, cred.url);
        // ADR-081 D2: the runtime is known by its identity, never by a
        // stored tunnel DID.
        assert_eq!(loaded.runtime_id, "did:key:z6MkRuntime");
        let toml_content = std::fs::read_to_string(&path).unwrap();
        assert!(!toml_content.contains("did:key"));
        assert!(!toml_content.contains("private_key"));
    }

    #[test]
    fn test_load_missing_credential() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("nonexistent.toml");

        let result = load_pekohub_credential(Some(&path), "did:key:z6MkRuntime");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_tls_config_roundtrip() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pekohub.toml");

        let cred = PekoHubCredential {
            url: "wss://pekohub.ai/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
            tls: Some(TunnelTlsConfig {
                ca_path: Some(std::path::PathBuf::from("/etc/peko/ca.pem")),
                cert_path: Some(std::path::PathBuf::from("/etc/peko/client.crt")),
                key_path: Some(std::path::PathBuf::from("/etc/peko/client.key")),
                pinned_cert_sha256: Some("abc123".to_string()),
            }),
        };

        cred.save_to_file(&path).unwrap();
        let loaded = PekoHubCredential::from_file(&path).unwrap();

        assert_eq!(loaded.url, cred.url);
        let tls = loaded.tls.expect("TLS config should be present");
        assert_eq!(
            tls.ca_path,
            Some(std::path::PathBuf::from("/etc/peko/ca.pem"))
        );
        assert_eq!(
            tls.cert_path,
            Some(std::path::PathBuf::from("/etc/peko/client.crt"))
        );
        assert_eq!(
            tls.key_path,
            Some(std::path::PathBuf::from("/etc/peko/client.key"))
        );
        assert_eq!(tls.pinned_cert_sha256, Some("abc123".to_string()));
    }

    /// A credential carrying the retired tunnel DID is refused with the
    /// way out, not silently reused.
    #[test]
    fn legacy_tunnel_credentials_are_refused() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pekohub.toml");
        std::fs::write(
            &path,
            "url = \"wss://hub/v1/tunnel\"\nruntime_id = \"did:key:zTunnel\"\n",
        )
        .unwrap();
        let err = format!("{:#}", PekoHubCredential::from_file(&path).unwrap_err());
        assert!(err.contains("peko runtime register"), "{err}");
    }
}
