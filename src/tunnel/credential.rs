//! PekoHub Credential Management
//!
//! Loads and stores the runtime's PekoHub credentials from disk.
//!
//! The credential file (`pekohub.toml`) no longer stores the raw private key.
//! Instead it stores a `keyring_entry` (the DID) that references the key in the
//! OS keychain. Legacy files that contain `private_key` are auto-migrated on
//! first load.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::identity::keychain::KeychainStorage;

/// On-disk PekoHub credential format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PekoHubCredential {
    /// WebSocket tunnel URL
    pub url: String,
    /// Runtime DID (did:key format)
    pub runtime_id: String,
    /// Keychain entry reference (the DID used as the keychain account name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyring_entry: Option<String>,
    /// Legacy: Ed25519 private key (base64-encoded raw 32 bytes).
    /// This field is read for migration but never written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
}

impl PekoHubCredential {
    /// Load credential from the given path
    ///
    /// Auto-migrates legacy credentials that contain a raw `private_key`:
    /// the key is moved into the OS keychain and the file is rewritten
    /// with `keyring_entry` instead.
    ///
    /// # Errors
    /// Returns error if file cannot be read or parsed
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read PekoHub credential: {path:?}"))?;
        let mut cred: Self = toml::from_str(&content)
            .with_context(|| format!("Failed to parse PekoHub credential: {path:?}"))?;

        // Auto-migrate legacy credentials
        if cred.keyring_entry.is_none() && cred.private_key.is_some() {
            warn!("Legacy PekoHub credential detected at {} — migrating to keychain", path.display());
            cred.migrate_to_keychain(path)?;
        }

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

    /// Migrate a legacy credential (with raw private_key) to the keychain.
    fn migrate_to_keychain(&mut self, path: &Path) -> Result<()> {
        let private_key = self
            .private_key
            .take()
            .ok_or_else(|| anyhow::anyhow!("migrate_to_keychain called without private_key"))?;

        let keychain = KeychainStorage::new();
        let entry = self.runtime_id.clone();

        if keychain.is_available() {
            keychain
                .store_key(&entry, &private_key)
                .with_context(|| "Failed to store migrated key in OS keychain")?;
            info!("Migrated private key to OS keychain for {}", self.runtime_id);
            self.keyring_entry = Some(entry);
            self.private_key = None;
            self.save_to_file(path)
                .with_context(|| "Failed to rewrite credential file after migration")?;
        } else {
            // Keychain unavailable — put the private_key back and skip migration
            warn!("OS keychain unavailable — skipping legacy credential migration for {}", self.runtime_id);
            self.private_key = Some(private_key);
        }

        Ok(())
    }

    /// Resolve the private key for this credential.
    ///
    /// Returns the base64-encoded private key, fetching it from the keychain
    /// if necessary.
    pub fn resolve_private_key(&self) -> Result<String> {
        if let Some(ref entry) = self.keyring_entry {
            let keychain = KeychainStorage::new();
            keychain
                .retrieve_key(entry)
                .with_context(|| format!("Failed to retrieve key from keychain for {entry}"))
        } else if let Some(ref key) = self.private_key {
            // Legacy path (should only happen if migration failed)
            Ok(key.clone())
        } else {
            anyhow::bail!(
                "PekoHub credential has neither keyring_entry nor private_key. \
                 Run `peko tunnel setup` to reconfigure."
            )
        }
    }

    /// Get the default credential file path
    ///
    /// Path: `{config_dir}/pekohub.toml` where `{config_dir}` is the
    /// `PEKO_HOME` env var (if set) or `~/.peko` (see
    /// [`crate::common::paths::default_config_dir`]).
    #[must_use]
    pub fn default_path() -> PathBuf {
        crate::common::paths::default_config_dir().join("pekohub.toml")
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
    fn test_credential_roundtrip_new_format() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pekohub.toml");

        let cred = PekoHubCredential {
            url: "wss://pekohub.org/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
            keyring_entry: Some("did:key:z6MkTest".to_string()),
            private_key: None,
        };

        cred.save_to_file(&path).unwrap();
        let loaded = PekoHubCredential::from_file(&path).unwrap();

        assert_eq!(loaded.url, cred.url);
        assert_eq!(loaded.runtime_id, cred.runtime_id);
        assert_eq!(loaded.keyring_entry, cred.keyring_entry);
        assert!(loaded.private_key.is_none(), "private_key should not be written when None");

        // Verify the TOML does not contain private_key
        let toml_content = std::fs::read_to_string(&path).unwrap();
        assert!(!toml_content.contains("private_key"));
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
    fn test_legacy_credential_deserialization() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pekohub.toml");

        let legacy_toml = r#"
url = "wss://pekohub.org/v1/tunnel"
runtime_id = "did:key:z6MkTest"
private_key = "base64encodedkey"
"#;
        std::fs::write(&path, legacy_toml).unwrap();

        let loaded = PekoHubCredential::from_file(&path).unwrap();
        assert_eq!(loaded.runtime_id, "did:key:z6MkTest");
        // private_key should still be present if keychain unavailable,
        // or None if migration succeeded. Either is valid.
    }
}
