//! Secure key storage for identities
//!
//! Stores identities in the platform-appropriate data directory:
//! - Linux: ~/.local/share/pekobot/identities/
//! - macOS: ~/Library/Application Support/pekobot/identities/
//! - Windows: %APPDATA%\pekobot\identities\

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::identity::did::{DIDScope, Identity};
use crate::identity::keys::KeyPair;

/// Storage format for serialized identities
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIdentity {
    /// DID string
    did: String,
    /// DID document (public)
    document: serde_json::Value,
    /// Encrypted or plaintext private key (base64)
    /// TODO: Add proper encryption
    private_key: String,
    /// Public key (base64)
    public_key: String,
    /// When the identity was created
    created_at: String,
    /// When the identity was last used
    last_used: String,
}

/// Key storage manager
pub struct KeyStorage {
    base_path: PathBuf,
}

impl KeyStorage {
    /// Create new key storage with default path
    pub fn new() -> Result<Self> {
        let base_path = Self::default_storage_path()?;
        Self::with_path(base_path)
    }

    /// Create new key storage with custom path
    pub fn with_path(base_path: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base_path)
            .with_context(|| format!("Failed to create storage directory: {base_path:?}"))?;

        // Set restrictive permissions (owner only: 700)
        #[cfg(unix)]
        {
            let permissions = fs::Permissions::from_mode(0o700);
            fs::set_permissions(&base_path, permissions)
                .with_context(|| "Failed to set directory permissions")?;
        }

        info!("Key storage initialized at: {:?}", base_path);
        Ok(Self { base_path })
    }

    /// Get the default storage path for the platform
    fn default_storage_path() -> Result<PathBuf> {
        let data_dir = dirs::data_dir().context("Could not determine data directory")?;
        Ok(data_dir.join("pekobot").join("identities"))
    }

    /// Generate and store a new identity
    pub fn generate_identity(&self, scope: DIDScope, tenant: Option<&str>) -> Result<Identity> {
        let identity = Identity::generate(scope, tenant).context("Failed to generate identity")?;

        self.store(&identity)?;
        info!("Generated and stored new identity: {}", identity.did);

        Ok(identity)
    }

    /// Store an identity to disk
    pub fn store(&self, identity: &Identity) -> Result<()> {
        let keypair = identity
            .keypair
            .as_ref()
            .context("Identity has no keypair")?;

        let export = keypair.export();

        let stored = StoredIdentity {
            did: identity.did.clone(),
            document: serde_json::to_value(&identity.document)?,
            private_key: export.private_key,
            public_key: export.public_key,
            created_at: identity.document.created.clone(),
            last_used: chrono::Utc::now().to_rfc3339(),
        };

        let file_path = self.identity_path(&identity.did);
        let json = serde_json::to_string_pretty(&stored)?;

        fs::write(&file_path, json)
            .with_context(|| format!("Failed to write identity file: {file_path:?}"))?;

        // Set restrictive file permissions (owner read/write only: 600)
        #[cfg(unix)]
        {
            let permissions = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&file_path, permissions)
                .with_context(|| "Failed to set file permissions")?;
        }

        debug!("Stored identity: {}", identity.did);
        Ok(())
    }

    /// Load an identity from disk
    pub fn load(&self, did: &str) -> Result<Identity> {
        let file_path = self.identity_path(did);

        if !file_path.exists() {
            anyhow::bail!("Identity not found: {did}");
        }

        let json = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read identity file: {file_path:?}"))?;

        let stored: StoredIdentity =
            serde_json::from_str(&json).context("Failed to parse identity file")?;

        // Reconstruct keypair
        let keypair_export = crate::identity::keys::KeyPairExport {
            public_key: stored.public_key,
            private_key: stored.private_key,
        };
        let keypair = KeyPair::import(&keypair_export)?;

        // Reconstruct DID document
        let document = serde_json::from_value(stored.document)?;

        // Update last used
        let mut identity = Identity {
            did: stored.did,
            document,
            keypair: Some(keypair),
        };

        // Update last used time
        identity.document.updated = chrono::Utc::now().to_rfc3339();
        self.store(&identity)?;

        debug!("Loaded identity: {}", identity.did);
        Ok(identity)
    }

    /// List all stored identities
    pub fn list_identities(&self) -> Result<Vec<String>> {
        let mut dids = Vec::new();

        for entry in fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "json") {
                match fs::read_to_string(&path) {
                    Ok(json) => {
                        if let Ok(stored) = serde_json::from_str::<StoredIdentity>(&json) {
                            dids.push(stored.did);
                        }
                    }
                    Err(e) => warn!("Failed to read {:?}: {}", path, e),
                }
            }
        }

        Ok(dids)
    }

    /// Check if an identity exists
    #[must_use]
    pub fn exists(&self, did: &str) -> bool {
        self.identity_path(did).exists()
    }

    /// Delete an identity
    pub fn delete(&self, did: &str) -> Result<()> {
        let file_path = self.identity_path(did);

        if !file_path.exists() {
            anyhow::bail!("Identity not found: {did}");
        }

        fs::remove_file(&file_path)
            .with_context(|| format!("Failed to delete identity file: {file_path:?}"))?;

        info!("Deleted identity: {}", did);
        Ok(())
    }

    /// Get the file path for an identity
    fn identity_path(&self, did: &str) -> PathBuf {
        // Sanitize DID for filename (replace colons with underscores)
        let filename = did.replace(':', "_");
        self.base_path.join(format!("{filename}.json"))
    }

    /// Get the base storage path
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.base_path
    }

    /// Export keys for an identity (for portable packages)
    pub fn export_keys(&self, did: &str) -> Result<crate::identity::keys::KeyPairExport> {
        let identity = self.load(did)?;
        let keypair = identity
            .keypair
            .as_ref()
            .context("Identity has no keypair")?;
        Ok(keypair.export())
    }

    /// Store identity asynchronously
    pub async fn store_identity(&self, identity: &Identity) -> Result<()> {
        // Use spawn_blocking for file operations
        let identity = identity.clone();
        let base_path = self.base_path.clone();

        tokio::task::spawn_blocking(move || {
            let storage = KeyStorage::with_path(base_path)?;
            storage.store(&identity)
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task failed: {e}"))?
    }

    /// Check if identity exists asynchronously
    pub async fn exists_async(&self, did: &str) -> Result<bool> {
        let did = did.to_string();
        let base_path = self.base_path.clone();

        tokio::task::spawn_blocking(move || {
            let storage = KeyStorage::with_path(base_path)?;
            Ok::<_, anyhow::Error>(storage.exists(&did))
        })
        .await
        .map_err(|e| anyhow::anyhow!("Task failed: {e}"))?
    }
}

impl Default for KeyStorage {
    fn default() -> Self {
        Self::new().expect("Failed to initialize key storage")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_and_store() {
        let temp_dir = TempDir::new().unwrap();
        let storage = KeyStorage::with_path(temp_dir.path().to_path_buf()).unwrap();

        let identity = storage
            .generate_identity(DIDScope::Local, Some("test"))
            .unwrap();

        assert!(storage.exists(&identity.did));
        assert!(identity.did.starts_with("did:pekobot:local:test:"));
    }

    #[test]
    fn test_store_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let storage = KeyStorage::with_path(temp_dir.path().to_path_buf()).unwrap();

        let identity = Identity::generate(DIDScope::Public, None).unwrap();
        let original_did = identity.did.clone();

        storage.store(&identity).unwrap();
        let loaded = storage.load(&original_did).unwrap();

        assert_eq!(loaded.did, original_did);
        assert!(loaded.keypair.is_some());

        // Verify keypair works
        let message = b"test message";
        let signature = loaded.keypair.as_ref().unwrap().sign(message);
        assert!(loaded
            .keypair
            .as_ref()
            .unwrap()
            .verify(message, &signature)
            .is_ok());
    }

    #[test]
    fn test_list_identities() {
        let temp_dir = TempDir::new().unwrap();
        let storage = KeyStorage::with_path(temp_dir.path().to_path_buf()).unwrap();

        storage.generate_identity(DIDScope::Public, None).unwrap();
        storage
            .generate_identity(DIDScope::Local, Some("acme"))
            .unwrap();

        let list = storage.list_identities().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_delete_identity() {
        let temp_dir = TempDir::new().unwrap();
        let storage = KeyStorage::with_path(temp_dir.path().to_path_buf()).unwrap();

        let identity = storage.generate_identity(DIDScope::Private, None).unwrap();
        let did = identity.did.clone();

        assert!(storage.exists(&did));
        storage.delete(&did).unwrap();
        assert!(!storage.exists(&did));
    }
}
