//! PekoHub Credential Management
//!
//! Loads and stores the runtime's PekoHub credentials from disk.
//!
//! The credential file (`pekohub.toml`) only stores public metadata: the
//! tunnel URL and the runtime DID. The private signing key lives in the
//! encrypted vault under the key `tunnel:{runtime_id}`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::common::paths::PathResolver;
use crate::common::vault::Vault;
use secrecy::ExposeSecret;

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

/// On-disk PekoHub credential format
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PekoHubCredential {
    /// WebSocket tunnel URL
    pub url: String,
    /// Runtime DID (did:key format)
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
        let cred: Self = toml::from_str(&content)
            .with_context(|| format!("Failed to parse PekoHub credential: {path:?}"))?;
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

    /// Resolve the private key for this credential from the given vault.
    ///
    /// Returns the base64-encoded private key.
    pub fn resolve_private_key(&self, vault: &Vault) -> Result<String> {
        vault
            .get_tunnel_private_key(&self.runtime_id)
            .map(|s| s.expose_secret().to_string())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No tunnel private key found in vault for {}. \
                     Run `peko tunnel setup` to reconfigure.",
                    self.runtime_id
                )
            })
    }

    /// Convenience: resolve the private key by loading the vault from the
    /// default config directory.
    ///
    /// Prefer the explicit `resolve_private_key(vault)` when the vault path is
    /// known (e.g. in the daemon with custom `--config-dir`).
    pub fn resolve_private_key_default(&self) -> Result<String> {
        let vault = Vault::load(crate::common::paths::default_config_dir().join("vault.enc"))
            .with_context(|| "failed to load vault to resolve tunnel private key")?;
        self.resolve_private_key(&vault)
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

/// Load PekoHub credential from the default location or a custom path.
///
/// Returns `None` if no credential file exists.
pub fn load_pekohub_credential(custom_path: Option<&Path>) -> Result<Option<PekoHubCredential>> {
    let path = custom_path.map_or_else(PekoHubCredential::default_path, PathBuf::from);

    if !path.exists() {
        info!("No PekoHub credential found at: {}", path.display());
        return Ok(None);
    }

    let cred = PekoHubCredential::from_file(&path)?;
    info!("Loaded PekoHub credential for runtime: {}", cred.runtime_id);
    Ok(Some(cred))
}

/// Check if PekoHub credentials exist
#[must_use]
pub fn has_pekohub_credential() -> bool {
    PekoHubCredential::default_path().exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_credential_roundtrip() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pekohub.toml");

        let cred = PekoHubCredential {
            url: "wss://pekohub.org/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
            tls: None,
        };

        cred.save_to_file(&path).unwrap();
        let loaded = PekoHubCredential::from_file(&path).unwrap();

        assert_eq!(loaded.url, cred.url);
        assert_eq!(loaded.runtime_id, cred.runtime_id);

        // Verify the TOML does not contain private_key
        let toml_content = std::fs::read_to_string(&path).unwrap();
        assert!(!toml_content.contains("private_key"));
        assert!(!toml_content.contains("keyring_entry"));
    }

    #[test]
    fn test_load_missing_credential() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("nonexistent.toml");

        let result = load_pekohub_credential(Some(&path));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_tls_config_roundtrip() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pekohub.toml");

        let cred = PekoHubCredential {
            url: "wss://pekohub.org/v1/tunnel".to_string(),
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
        assert_eq!(loaded.runtime_id, cred.runtime_id);
        let tls = loaded.tls.expect("TLS config should be present");
        assert_eq!(tls.ca_path, Some(std::path::PathBuf::from("/etc/peko/ca.pem")));
        assert_eq!(tls.cert_path, Some(std::path::PathBuf::from("/etc/peko/client.crt")));
        assert_eq!(tls.key_path, Some(std::path::PathBuf::from("/etc/peko/client.key")));
        assert_eq!(tls.pinned_cert_sha256, Some("abc123".to_string()));
    }

    #[test]
    fn test_resolve_private_key_with_vault() {
        let temp = TempDir::new().unwrap();
        let vault = Vault::for_test(temp.path(), "tunnel-test");
        let cred = PekoHubCredential {
            url: "wss://pekohub.org/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
            tls: None,
        };

        cred.resolve_private_key(&vault).unwrap_err();

        vault
            .set_tunnel_private_key(&cred.runtime_id, "dHVubmVsLWtleQ==")
            .unwrap();
        let key = cred.resolve_private_key(&vault).unwrap();
        assert_eq!(key, "dHVubmVsLWtleQ==");
    }
}
