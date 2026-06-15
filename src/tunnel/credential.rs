//! PekoHub Credential Management
//!
//! Loads and stores the runtime's PekoHub credentials from disk.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

/// On-disk PekoHub credential format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PekoHubCredential {
    /// WebSocket tunnel URL
    pub url: String,
    /// Runtime DID (did:key format)
    pub runtime_id: String,
    /// Ed25519 private key (base64-encoded raw 32 bytes)
    pub private_key: String,
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

    /// Get the default credential file path
    ///
    /// Path: `{config_dir}/pekohub.toml` where `{config_dir}` is the
    /// `PEKO_HOME` env var (if set) or `~/.peko` (see
    /// [`crate::common::paths::default_config_dir`]).
    ///
    /// **Why not `dirs::home_dir().join(".peko")`.** `dirs 5.0.1`'s
    /// `home_dir()` on Windows is hard-coded to return the
    /// `FOLDERID_Profile` path, ignoring both `HOME` and
    /// `USERPROFILE` env overrides. That made the runtime's tunnel
    /// startup always look for `pekohub.toml` at
    /// `C:\Users\<user>\.peko\pekohub.toml` regardless of the
    /// `PEKO_HOME` (or per-CLI HOME) the rest of the daemon and CLI
    /// respected — breaking isolated test environments that set
    /// `PEKO_HOME=<tempdir>`. The fix routes through
    /// `default_config_dir()` which respects `PEKO_HOME`. Phase D4's
    /// `permit_owner_can_chat` test is the first end-to-end test
    /// that drove the daemon→tunnel path with `PEKO_HOME` set, and
    /// it surfaced the regression.
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
    fn test_credential_roundtrip() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pekohub.toml");

        let cred = PekoHubCredential {
            url: "wss://pekohub.org/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
            private_key: "base64encodedkey".to_string(),
        };

        cred.save_to_file(&path).unwrap();
        let loaded = PekoHubCredential::from_file(&path).unwrap();

        assert_eq!(loaded.url, cred.url);
        assert_eq!(loaded.runtime_id, cred.runtime_id);
        assert_eq!(loaded.private_key, cred.private_key);
    }

    #[test]
    fn test_load_missing_credential() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("nonexistent.toml");

        let result = load_pekohub_credential(Some(&path));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
