//! Secure key storage for identities
//!
//! Stores identities in the platform-appropriate data directory:
//! - Linux: ~/.local/share/peko/identities/
//! - macOS: ~/Library/Application Support/peko/identities/
//! - Windows: %APPDATA%\peko\identities\
//!
//! Private keys are stored in the OS keychain (primary) or as encrypted
//! files on disk (fallback). Legacy plaintext identities are auto-migrated
//! on first load.

use anyhow::{Context, Result};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::identity::did::{DIDScope, Identity};
use crate::identity::keychain::{EncryptedKeyStorage, KeyStorageRef, KeychainStorage};
use crate::identity::keys::KeyPair;

/// Storage format for serialized identities
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIdentity {
    /// DID string
    did: String,
    /// DID document (public)
    document: serde_json::Value,
    /// Reference to where the private key is stored securely
    key_storage: KeyStorageRef,
    /// Public key (base64)
    public_key: String,
    /// When the identity was created
    created_at: String,
    /// When the identity was last used
    last_used: String,
}

/// Legacy storage format for migration detection
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyStoredIdentity {
    did: String,
    document: serde_json::Value,
    private_key: String,
    public_key: String,
    created_at: String,
    last_used: String,
}

/// Key storage manager
pub struct KeyStorage {
    base_path: PathBuf,
    /// Optional passphrase for encrypted-file fallback in tests.
    /// When set and the OS keychain is unavailable, this passphrase
    /// is used automatically for EncryptedKeyStorage.
    test_passphrase: Option<SecretString>,
}

impl KeyStorage {
    /// Create new key storage with default path
    pub fn new() -> Result<Self> {
        let base_path = Self::default_storage_path()?;
        let env_passphrase = std::env::var("PEKO_IDENTITY_PASSPHRASE")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| SecretString::new(s.into()));
        Self::with_path_and_passphrase(base_path, env_passphrase)
    }

    /// Create new key storage with custom path
    pub fn with_path(base_path: PathBuf) -> Result<Self> {
        Self::with_path_and_passphrase(base_path, None)
    }

    fn with_path_and_passphrase(base_path: PathBuf, passphrase: Option<SecretString>) -> Result<Self> {
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
        Ok(Self {
            base_path,
            test_passphrase: passphrase,
        })
    }

    /// Create new key storage with a passphrase for fallback encryption.
    ///
    /// This is useful for headless environments where the OS keychain is
    /// unavailable, and for tests that need deterministic encrypted storage.
    pub fn with_passphrase(base_path: PathBuf, passphrase: SecretString) -> Result<Self> {
        fs::create_dir_all(&base_path)
            .with_context(|| format!("Failed to create storage directory: {base_path:?}"))?;
        Ok(Self {
            base_path,
            test_passphrase: Some(passphrase),
        })
    }

    /// Get the default storage path for the platform
    fn default_storage_path() -> Result<PathBuf> {
        Ok(crate::common::paths::default_data_dir().join("identities"))
    }

    /// Generate and store a new identity
    pub fn generate_identity(&self, scope: DIDScope, tenant: Option<&str>) -> Result<Identity> {
        let identity = Identity::generate(scope, tenant).context("Failed to generate identity")?;

        self.store(&identity)?;
        info!("Generated and stored new identity: {}", identity.did);

        Ok(identity)
    }

    /// Store an identity to disk.
    ///
    /// Tries the OS keychain first. If unavailable, falls back to encrypted
    /// file storage using the test passphrase (if set) or prompts for one.
    pub fn store(&self, identity: &Identity) -> Result<()> {
        let keypair = identity
            .keypair
            .as_ref()
            .context("Identity has no keypair")?;

        let export = keypair.export();

        // Try keychain first (unless we're in a test with a fallback passphrase)
        let keychain = KeychainStorage::new();
        let key_storage = if self.test_passphrase.is_some() {
            // Test mode: always use encrypted file fallback for determinism
            let enc_path = self.encrypted_key_path(&identity.did);
            EncryptedKeyStorage::store_key(&enc_path, &export.private_key, self.test_passphrase.as_ref().unwrap())
                .context("Failed to store key in encrypted file (test fallback)")?
        } else if keychain.is_available() {
            keychain
                .store_key(&identity.did, &export.private_key)
                .context("Failed to store key in OS keychain")?
        } else {
            anyhow::bail!(
                "OS keychain is unavailable and no passphrase was provided. \
                 Set PEKO_IDENTITY_PASSPHRASE or use KeyStorage::with_passphrase() for headless environments."
            );
        };

        let stored = StoredIdentity {
            did: identity.did.clone(),
            document: serde_json::to_value(&identity.document)?,
            key_storage,
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

    /// Load an identity from disk.
    ///
    /// Auto-migrates legacy plaintext identities to the keychain on first load.
    pub fn load(&self, did: &str) -> Result<Identity> {
        let file_path = self.identity_path(did);

        if !file_path.exists() {
            anyhow::bail!("Identity not found: {did}");
        }

        let json = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read identity file: {file_path:?}"))?;

        // Try modern format first
        if let Ok(stored) = serde_json::from_str::<StoredIdentity>(&json) {
            return self.load_from_stored(stored, did);
        }

        // Fall back to legacy format and auto-migrate
        if let Ok(legacy) = serde_json::from_str::<LegacyStoredIdentity>(&json) {
            warn!(
                "Loaded legacy plaintext identity for {} — migrating to secure storage",
                did
            );
            return self.migrate_legacy(legacy, &file_path);
        }

        anyhow::bail!("Failed to parse identity file: unrecognized format")
    }

    /// Resolve the private key from a StoredIdentity and reconstruct the Identity.
    fn load_from_stored(
        &self,
        stored: StoredIdentity,
        did: &str,
    ) -> Result<Identity> {
        let private_key_b64 = match &stored.key_storage {
            KeyStorageRef::Keychain { service, account } => {
                let keychain = KeychainStorage::with_service(service.clone());
                keychain
                    .retrieve_key(account)
                    .with_context(|| {
                        format!(
                            "Failed to retrieve key from keychain for {account}. \
                             Is the keychain unlocked?"
                        )
                    })?
            }
            KeyStorageRef::EncryptedFile { file_name } => {
                let enc_path = self.base_path.join(file_name);
                let passphrase = self.test_passphrase.as_ref()
                    .ok_or_else(|| anyhow::anyhow!(
                        "Encrypted identity {did} requires a passphrase. \
                         Set PEKO_IDENTITY_PASSPHRASE or use KeyStorage::with_passphrase()."
                    ))?;
                EncryptedKeyStorage::retrieve_key(&enc_path, passphrase)
                    .with_context(|| format!("Failed to decrypt key for {did}"))?
            }
            KeyStorageRef::Plaintext => {
                anyhow::bail!("Identity {did} has Plaintext key_storage — this should have been migrated")
            }
        };

        let keypair_export = crate::identity::keys::KeyPairExport {
            public_key: stored.public_key,
            private_key: private_key_b64,
        };
        let keypair = KeyPair::import(&keypair_export)?;

        let document = serde_json::from_value(stored.document)?;

        let identity = Identity {
            did: stored.did,
            document,
            keypair: Some(keypair),
        };

        debug!("Loaded identity: {}", identity.did);
        Ok(identity)
    }

    /// Migrate a legacy plaintext identity to secure storage.
    fn migrate_legacy(&self, legacy: LegacyStoredIdentity, file_path: &Path) -> Result<Identity> {
        let did = legacy.did.clone();

        // Test mode: always use encrypted file fallback for determinism
        let keychain = KeychainStorage::new();
        let key_storage = if self.test_passphrase.is_some() {
            let enc_path = self.encrypted_key_path(&did);
            EncryptedKeyStorage::store_key(&enc_path, &legacy.private_key, self.test_passphrase.as_ref().unwrap())
                .context("Failed to migrate key to encrypted file")?
        } else if keychain.is_available() {
            keychain
                .store_key(&did, &legacy.private_key)
                .context("Failed to migrate key to OS keychain")?
        } else {
            anyhow::bail!(
                "Cannot migrate legacy identity {did}: OS keychain unavailable and no passphrase provided"
            );
        };

        // 2. Build the new stored format
        let stored = StoredIdentity {
            did: legacy.did.clone(),
            document: legacy.document,
            key_storage,
            public_key: legacy.public_key.clone(),
            created_at: legacy.created_at,
            last_used: chrono::Utc::now().to_rfc3339(),
        };

        // 3. Best-effort overwrite of the old plaintext file (not a cryptographic wipe;
        // wear-leveling, filesystem journals, and copy-on-write may leave traces).
        if let Ok(metadata) = fs::metadata(file_path) {
            let len = metadata.len() as usize;
            let zeros = vec![0u8; len];
            let _ = fs::write(file_path, &zeros);
        }

        // 4. Write the new secure format
        let json = serde_json::to_string_pretty(&stored)?;
        fs::write(file_path, json)
            .with_context(|| format!("Failed to write migrated identity file: {file_path:?}"))?;

        #[cfg(unix)]
        {
            let permissions = fs::Permissions::from_mode(0o600);
            let _ = fs::set_permissions(file_path, permissions);
        }

        info!("Migrated legacy plaintext identity to secure storage: {did}");

        // 5. Reconstruct and return the Identity
        let keypair_export = crate::identity::keys::KeyPairExport {
            public_key: legacy.public_key,
            private_key: legacy.private_key,
        };
        let keypair = KeyPair::import(&keypair_export)?;
        let document = serde_json::from_value(stored.document)?;

        Ok(Identity {
            did: stored.did,
            document,
            keypair: Some(keypair),
        })
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
                        // Try modern format
                        if let Ok(stored) = serde_json::from_str::<StoredIdentity>(&json) {
                            dids.push(stored.did);
                        } else if let Ok(legacy) = serde_json::from_str::<LegacyStoredIdentity>(&json) {
                            dids.push(legacy.did);
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

    /// Delete an identity (including keychain entry and encrypted key file)
    pub fn delete(&self, did: &str) -> Result<()> {
        let file_path = self.identity_path(did);

        if !file_path.exists() {
            anyhow::bail!("Identity not found: {did}");
        }

        // Try to read the file to find where the key is stored
        if let Ok(json) = fs::read_to_string(&file_path) {
            if let Ok(stored) = serde_json::from_str::<StoredIdentity>(&json) {
                match stored.key_storage {
                    KeyStorageRef::Keychain { service, account } => {
                        let keychain = KeychainStorage::with_service(service);
                        if let Err(e) = keychain.delete_key(&account) {
                            warn!("Failed to delete keychain entry for {account}: {e}");
                        }
                    }
                    KeyStorageRef::EncryptedFile { file_name } => {
                        let enc_path = self.base_path.join(file_name);
                        if let Err(e) = fs::remove_file(&enc_path) {
                            warn!("Failed to delete encrypted key file {enc_path:?}: {e}");
                        }
                    }
                    KeyStorageRef::Plaintext => {}
                }
            }
        }

        fs::remove_file(&file_path)
            .with_context(|| format!("Failed to delete identity file: {file_path:?}"))?;

        info!("Deleted identity: {}", did);
        Ok(())
    }

    /// Get the file path for an identity JSON file
    fn identity_path(&self, did: &str) -> PathBuf {
        let filename = did.replace(':', "_");
        self.base_path.join(format!("{filename}.json"))
    }

    /// Get the file path for an encrypted key file
    fn encrypted_key_path(&self, did: &str) -> PathBuf {
        let filename = did.replace(':', "_");
        self.base_path.join(format!("{filename}.enc"))
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
        let identity = identity.clone();
        let base_path = self.base_path.clone();
        let test_passphrase = self.test_passphrase.clone();

        tokio::task::spawn_blocking(move || {
            let mut storage = KeyStorage::with_path(base_path)?;
            storage.test_passphrase = test_passphrase;
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
    use secrecy::SecretString;
    use tempfile::TempDir;

    #[test]
    fn test_generate_and_store() {
        let temp_dir = TempDir::new().unwrap();
        let passphrase = SecretString::new("test-passphrase".into());
        let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

        let identity = storage
            .generate_identity(DIDScope::Local, Some("test"))
            .unwrap();

        assert!(storage.exists(&identity.did));
        assert!(identity.did.starts_with("did:peko:local:test:"));
    }

    #[test]
    fn test_store_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let passphrase = SecretString::new("test-passphrase".into());
        let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

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
        let passphrase = SecretString::new("test-passphrase".into());
        let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

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
        let passphrase = SecretString::new("test-passphrase".into());
        let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

        let identity = storage.generate_identity(DIDScope::Private, None).unwrap();
        let did = identity.did.clone();

        assert!(storage.exists(&did));
        storage.delete(&did).unwrap();
        assert!(!storage.exists(&did));
    }

    #[test]
    fn test_legacy_migration() {
        let temp_dir = TempDir::new().unwrap();
        let passphrase = SecretString::new("migration-passphrase".into());
        let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

        // Create a legacy plaintext identity file
        let identity = Identity::generate(DIDScope::Public, None).unwrap();
        let did = identity.did.clone();
        let keypair = identity.keypair.as_ref().unwrap();
        let export = keypair.export();

        let legacy = LegacyStoredIdentity {
            did: did.clone(),
            document: serde_json::to_value(&identity.document).unwrap(),
            private_key: export.private_key.clone(),
            public_key: export.public_key.clone(),
            created_at: identity.document.created.clone(),
            last_used: chrono::Utc::now().to_rfc3339(),
        };

        let file_path = storage.identity_path(&did);
        fs::write(&file_path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        // Load should auto-migrate
        let loaded = storage.load(&did).unwrap();
        assert_eq!(loaded.did, did);
        assert!(loaded.keypair.is_some());

        // Verify the file was rewritten without plaintext private_key
        let json = fs::read_to_string(&file_path).unwrap();
        assert!(!json.contains("private_key"));
        // Should contain key_storage reference
        assert!(json.contains("key_storage"));

        // Verify keypair still works after migration
        let message = b"post-migration test";
        let signature = loaded.keypair.as_ref().unwrap().sign(message);
        assert!(loaded.keypair.as_ref().unwrap().verify(message, &signature).is_ok());
    }

    #[test]
    fn test_encrypted_file_fallback() {
        let temp_dir = TempDir::new().unwrap();
        let passphrase = SecretString::new("fallback-passphrase".into());
        let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

        let identity = Identity::generate(DIDScope::Public, None).unwrap();
        let did = identity.did.clone();

        storage.store(&identity).unwrap();

        // On Windows (and other platforms without a working keychain) the
        // encrypted fallback is used, so the .enc file should exist.  If the
        // OS keychain happens to be available (e.g. macOS with an unlocked
        // keychain) the key is stored there instead and no .enc file is created.
        let enc_path = storage.encrypted_key_path(&did);
        let keychain = KeychainStorage::new();
        if !keychain.is_available() {
            assert!(enc_path.exists(), "Encrypted key file should exist when keychain is unavailable");
        }

        // Load should work regardless of where the key ended up
        let loaded = storage.load(&did).unwrap();
        assert_eq!(loaded.did, did);
        assert!(loaded.keypair.is_some());
    }
}
