//! OS keychain integration for secure private key storage
//!
//! Provides [`KeychainStorage`] for storing keys in the platform keychain (macOS
//! Keychain, Windows Credential Manager, Linux Secret Service / kwallet), and
//! [`EncryptedKeyStorage`] as an encrypted-at-rest fallback for headless
//! environments where no keychain is available.

use anyhow::{Context, Result};
use secrecy::{ExposeSecret, SecretString};
use std::path::Path;
use tracing::{debug, info, warn};

use crate::portable::crypto::{
    decrypt_with_passphrase, deserialize_encrypted, encrypt_with_passphrase, serialize_encrypted,
};

/// Reference to where a private key is stored
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyStorageRef {
    /// Stored in OS keychain
    Keychain {
        /// The service name used in the keychain entry
        service: String,
        /// The account name (typically the DID)
        account: String,
    },
    /// Stored as encrypted file on disk
    EncryptedFile {
        /// Path to the encrypted key file (relative to identity dir)
        file_name: String,
    },
    /// Legacy plaintext (for migration detection only)
    #[serde(skip)]
    Plaintext,
}

/// Manager for storing and retrieving private keys securely
pub struct KeychainStorage {
    service_name: String,
}

impl KeychainStorage {
    pub const DEFAULT_SERVICE: &'static str = "pekobot-runtime";

    /// Create a new keychain storage with the default service name
    pub fn new() -> Self {
        Self {
            service_name: Self::DEFAULT_SERVICE.to_string(),
        }
    }

    /// Create a new keychain storage with a custom service name
    pub fn with_service(service_name: String) -> Self {
        Self { service_name }
    }

    /// Store a private key in the OS keychain.
    ///
    /// `account` is typically the DID (e.g., `did:key:z6Mk...`).
    /// Returns `KeyStorageRef::Keychain` on success.
    pub fn store_key(&self, account: &str, private_key_b64: &str) -> Result<KeyStorageRef> {
        let entry =
            keyring::Entry::new(&self.service_name, account).with_context(|| {
                format!("Failed to create keychain entry for account: {account}")
            })?;

        entry
            .set_password(private_key_b64)
            .with_context(|| format!("Failed to store key in keychain for account: {account}"))?;

        info!("Stored private key in keychain for account: {}", account);

        Ok(KeyStorageRef::Keychain {
            service: self.service_name.clone(),
            account: account.to_string(),
        })
    }

    /// Retrieve a private key from the OS keychain.
    ///
    /// Returns the base64-encoded private key.
    pub fn retrieve_key(&self, account: &str) -> Result<String> {
        let entry =
            keyring::Entry::new(&self.service_name, account).with_context(|| {
                format!("Failed to create keychain entry for account: {account}")
            })?;

        let password = entry
            .get_password()
            .with_context(|| format!("Failed to retrieve key from keychain for account: {account}"))?;

        debug!("Retrieved private key from keychain for account: {}", account);

        Ok(password)
    }

    /// Delete a key from the OS keychain.
    pub fn delete_key(&self, account: &str) -> Result<()> {
        let entry =
            keyring::Entry::new(&self.service_name, account).with_context(|| {
                format!("Failed to create keychain entry for account: {account}")
            })?;

        entry
            .delete_password()
            .with_context(|| format!("Failed to delete key from keychain for account: {account}"))?;

        info!("Deleted private key from keychain for account: {}", account);

        Ok(())
    }

    /// Check if the OS keychain is available.
    ///
    /// Tries to create a dummy entry with a random account, set a dummy password,
    /// get it back, then delete it. Returns `true` if all steps succeed.
    pub fn is_available(&self) -> bool {
        let probe_account = format!("__pekobot_probe_{}", uuid::Uuid::new_v4());
        let probe_password = "__probe_password__";

        let entry = match keyring::Entry::new(&self.service_name, &probe_account) {
            Ok(e) => e,
            Err(e) => {
                warn!("Keychain probe: failed to create entry: {e}");
                return false;
            }
        };

        if let Err(e) = entry.set_password(probe_password) {
            warn!("Keychain probe: failed to set password: {e}");
            return false;
        }

        match entry.get_password() {
            Ok(p) => {
                if p != probe_password {
                    warn!("Keychain probe: retrieved password does not match");
                    // Still try to clean up
                    let _ = entry.delete_password();
                    return false;
                }
            }
            Err(e) => {
                warn!("Keychain probe: failed to get password: {e}");
                let _ = entry.delete_password();
                return false;
            }
        }

        if let Err(e) = entry.delete_password() {
            warn!("Keychain probe: failed to delete password: {e}");
            // We were able to read/write, so consider it available even if
            // cleanup failed. Some backends don't support deletion.
        }

        true
    }
}

impl Default for KeychainStorage {
    fn default() -> Self {
        Self::new()
    }
}

/// Encrypted file fallback for headless environments.
pub struct EncryptedKeyStorage;

impl EncryptedKeyStorage {
    /// Encrypt a private key with a passphrase and write to a file.
    ///
    /// Returns `KeyStorageRef::EncryptedFile`.
    pub fn store_key(
        file_path: &Path,
        private_key_b64: &str,
        passphrase: &SecretString,
    ) -> Result<KeyStorageRef> {
        let encrypted =
            encrypt_with_passphrase(private_key_b64.as_bytes(), passphrase.expose_secret())
                .context("Failed to encrypt private key with passphrase")?;

        let serialized = serialize_encrypted(&encrypted);

        std::fs::write(file_path, &serialized)
            .with_context(|| format!("Failed to write encrypted key file: {file_path:?}"))?;

        // Set restrictive file permissions on Unix (owner read/write only: 600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(file_path, permissions)
                .with_context(|| "Failed to set encrypted key file permissions")?;
        }

        info!("Stored encrypted private key at: {:?}", file_path);

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("key.enc")
            .to_string();

        Ok(KeyStorageRef::EncryptedFile { file_name })
    }

    /// Read and decrypt a private key from a file.
    ///
    /// Returns the base64-encoded private key.
    pub fn retrieve_key(
        file_path: &Path,
        passphrase: &SecretString,
    ) -> Result<String> {
        let serialized = std::fs::read(file_path)
            .with_context(|| format!("Failed to read encrypted key file: {file_path:?}"))?;

        let encrypted = deserialize_encrypted(&serialized)
            .context("Failed to deserialize encrypted key data")?;

        let decrypted = decrypt_with_passphrase(&encrypted, passphrase.expose_secret())
            .context("Failed to decrypt private key (wrong passphrase?)")?;

        let private_key_b64 = String::from_utf8(decrypted)
            .context("Decrypted key is not valid UTF-8 (base64)")?;

        debug!("Retrieved encrypted private key from: {:?}", file_path);

        Ok(private_key_b64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;
    use tempfile::TempDir;

    // ------------------------------------------------------------------
    // EncryptedKeyStorage tests (no OS keychain required)
    // ------------------------------------------------------------------

    #[test]
    fn test_encrypted_storage_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test_key.enc");
        let private_key = "dGVzdC1wcml2YXRlLWtleQ=="; // base64 "test-private-key"
        let passphrase = SecretString::new("super-secret-passphrase".into());

        let storage_ref =
            EncryptedKeyStorage::store_key(&file_path, private_key, &passphrase).unwrap();

        match &storage_ref {
            KeyStorageRef::EncryptedFile { file_name } => {
                assert_eq!(file_name, "test_key.enc");
            }
            other => panic!("Expected EncryptedFile, got: {:?}", other),
        }

        let retrieved =
            EncryptedKeyStorage::retrieve_key(&file_path, &passphrase).unwrap();
        assert_eq!(retrieved, private_key);
    }

    #[test]
    fn test_encrypted_storage_wrong_passphrase() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test_key.enc");
        let private_key = "dGVzdC1wcml2YXRlLWtleQ==";
        let passphrase = SecretString::new("correct-passphrase".into());

        EncryptedKeyStorage::store_key(&file_path, private_key, &passphrase).unwrap();

        let wrong = SecretString::new("wrong-passphrase".into());
        let result = EncryptedKeyStorage::retrieve_key(&file_path, &wrong);
        assert!(result.is_err());
    }

    #[test]
    fn test_encrypted_storage_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("nonexistent.enc");
        let passphrase = SecretString::new("passphrase".into());

        let result = EncryptedKeyStorage::retrieve_key(&file_path, &passphrase);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Failed to read encrypted key file"));
    }

    #[test]
    fn test_encrypted_storage_file_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test_key.enc");
        let private_key = "dGVzdC1wcml2YXRlLWtleQ==";
        let passphrase = SecretString::new("passphrase".into());

        EncryptedKeyStorage::store_key(&file_path, private_key, &passphrase).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(&file_path).unwrap();
            let mode = metadata.permissions().mode();
            // Should be 0o600 (owner read/write only)
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    // ------------------------------------------------------------------
    // KeyStorageRef serialization tests
    // ------------------------------------------------------------------

    #[test]
    fn test_key_storage_ref_serialization() {
        let keychain_ref = KeyStorageRef::Keychain {
            service: "pekobot-runtime".to_string(),
            account: "did:key:z6Mk...".to_string(),
        };

        let json = serde_json::to_string(&keychain_ref).unwrap();
        let deserialized: KeyStorageRef = serde_json::from_str(&json).unwrap();

        match deserialized {
            KeyStorageRef::Keychain { service, account } => {
                assert_eq!(service, "pekobot-runtime");
                assert_eq!(account, "did:key:z6Mk...");
            }
            other => panic!("Expected Keychain, got: {:?}", other),
        }
    }

    #[test]
    fn test_key_storage_ref_encrypted_file_serialization() {
        let file_ref = KeyStorageRef::EncryptedFile {
            file_name: "keys.enc".to_string(),
        };

        let json = serde_json::to_string(&file_ref).unwrap();
        let deserialized: KeyStorageRef = serde_json::from_str(&json).unwrap();

        match deserialized {
            KeyStorageRef::EncryptedFile { file_name } => {
                assert_eq!(file_name, "keys.enc");
            }
            other => panic!("Expected EncryptedFile, got: {:?}", other),
        }
    }

    #[test]
    fn test_key_storage_ref_plaintext_not_serialized() {
        let plaintext = KeyStorageRef::Plaintext;
        // #[serde(skip)] on a variant in an internally-tagged enum makes
        // serialization fail entirely, which is the desired behaviour —
        // Plaintext must never end up on disk.
        let result = serde_json::to_string(&plaintext);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // KeychainStorage tests (require an actual OS keychain)
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "requires an OS keychain (macOS Keychain, Windows Credential Manager, etc.)"]
    fn test_keychain_storage_roundtrip() {
        let storage = KeychainStorage::with_service("pekobot-test".to_string());
        let account = "did:key:z6MkTestAccount";
        let private_key = "dGVzdC1wcml2YXRlLWtleQ==";

        let storage_ref = storage.store_key(account, private_key).unwrap();
        match &storage_ref {
            KeyStorageRef::Keychain { service, account: acc } => {
                assert_eq!(service, "pekobot-test");
                assert_eq!(acc, account);
            }
            other => panic!("Expected Keychain, got: {:?}", other),
        }

        let retrieved = storage.retrieve_key(account).unwrap();
        assert_eq!(retrieved, private_key);

        storage.delete_key(account).unwrap();
    }

    #[test]
    #[ignore = "requires an OS keychain"]
    fn test_keychain_storage_delete() {
        let storage = KeychainStorage::with_service("pekobot-test".to_string());
        let account = "did:key:z6MkDeleteTest";
        let private_key = "dGVzdC1wcml2YXRlLWtleQ==";

        storage.store_key(account, private_key).unwrap();
        assert!(storage.retrieve_key(account).is_ok());

        storage.delete_key(account).unwrap();
        assert!(storage.retrieve_key(account).is_err());
    }

    #[test]
    #[ignore = "requires an OS keychain"]
    fn test_keychain_storage_is_available() {
        let storage = KeychainStorage::with_service("pekobot-test".to_string());
        assert!(storage.is_available());
    }

    #[test]
    fn test_keychain_storage_default_service() {
        let storage = KeychainStorage::new();
        assert_eq!(storage.service_name, KeychainStorage::DEFAULT_SERVICE);
    }
}
