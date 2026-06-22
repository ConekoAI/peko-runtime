//! Unified encrypted vault for runtime secrets.
//!
//! The vault stores all reversible runtime secrets in a single encrypted file
//! at `{config_dir}/vault.enc` (by default `~/.peko/vault.enc`).
//!
//! # Encryption
//!
//! The vault is encrypted with AES-256-GCM. The data-encryption key (DEK) is
//! obtained using one of two methods:
//!
//! 1. **OS keychain (default)** — a random 32-byte DEK is generated on first
//!    use and stored in the OS keychain under service `peko`, account
//!    `vault-key`.
//! 2. **Master passphrase fallback** — when the OS keychain is unavailable
//!    (headless/CI), the DEK is derived from `PEKO_MASTER_PASSPHRASE` using
//!    Argon2id. A vault created this way stores a salt in its envelope and
//!    can only be unlocked with the same passphrase.
//!
//! # Contents
//!
//! The plaintext inside the envelope is a `VaultFile`: a versioned map of
//! typed secret entries. Entries include provider API keys, registry tokens,
//! identity private keys, and tunnel private keys.
//!
//! # Thread safety
//!
//! `Vault` uses interior mutability (`std::sync::RwLock`) so it can be shared
//! across async tasks and implements the `SecretStore` trait used by
//! `LlmResolver`. Mutating methods automatically persist the vault.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use anyhow::{Context, Result};
use argon2::Argon2;
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::info;

/// On-disk vault filename.
pub const VAULT_FILE_NAME: &str = "vault.enc";

/// OS keychain service name for the vault DEK.
pub const KEYCHAIN_SERVICE: &str = "peko";

/// OS keychain account name for the vault DEK.
pub const KEYCHAIN_ACCOUNT: &str = "vault-key";

/// Environment variable used for passphrase-based vault unlock.
pub const MASTER_PASSPHRASE_ENV: &str = "PEKO_MASTER_PASSPHRASE";

/// Current vault file format version.
pub const VAULT_VERSION: u32 = 1;

/// AES-GCM nonce length in bytes.
const NONCE_LENGTH: usize = 12;

/// AES-256 key length in bytes.
const KEY_LENGTH: usize = 32;

/// Test-only fallback passphrase used when the OS keychain is unavailable and
/// `PEKO_MASTER_PASSPHRASE` is not set. This is only compiled into test builds
/// so that unit tests are self-contained in headless environments.
#[cfg(test)]
const TEST_MASTER_PASSPHRASE: &str = "peko-unit-test-passphrase-do-not-use";

/// Argon2id default parameters for passphrase derivation.
const ARGON2_MEMORY_COST: u32 = 65536; // 64 MB
const ARGON2_TIME_COST: u32 = 3;
const ARGON2_PARALLELISM: u32 = 4;

/// Errors specific to vault operations.
#[derive(Debug, Error)]
pub enum VaultError {
    #[error("vault is locked: {0}")]
    Locked(String),

    #[error("vault backend error: {0}")]
    Backend(String),

    #[error("no master passphrase available; set {MASTER_PASSPHRASE_ENV} or use an OS keychain")]
    NoPassphrase,

    #[error("invalid secret entry type for key '{0}'")]
    InvalidEntryType(String),
}

/// Encrypted envelope written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEnvelope {
    pub version: u32,
    /// `None` when the DEK is stored in the OS keychain (raw key mode).
    /// `Some(salt)` when the DEK is derived from a passphrase.
    pub salt: Option<Vec<u8>>,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Plaintext vault contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFile {
    pub version: u32,
    pub entries: HashMap<String, VaultEntry>,
}

impl Default for VaultFile {
    fn default() -> Self {
        Self {
            version: VAULT_VERSION,
            entries: HashMap::new(),
        }
    }
}

/// A typed secret entry in the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VaultEntry {
    /// LLM provider API key.
    ProviderApiKey { provider: String, key: String },
    /// PekoHub registry token.
    RegistryToken {
        host: String,
        token: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    /// Runtime identity private signing key.
    IdentityPrivateKey {
        key_id: String,
        algorithm: String,
        key: String,
    },
    /// PekoHub tunnel private key.
    TunnelPrivateKey { runtime_id: String, key: String },
    /// Generic fallback secret.
    Secret { value: String },
}

/// How the vault DEK was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockMethod {
    Keychain,
    Passphrase,
}

/// In-memory vault state holding the decrypted DEK and, for passphrase-backed
/// vaults, the salt used to derive it.
struct VaultState {
    file: VaultFile,
    dek: Vec<u8>,
    salt: Option<Vec<u8>>,
}

/// Unified encrypted secret vault.
pub struct Vault {
    path: PathBuf,
    inner: std::sync::RwLock<VaultState>,
    unlock_method: UnlockMethod,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("unlock_method", &self.unlock_method)
            .finish()
    }
}

impl Vault {
    /// Load an existing vault or create a new one at the given path.
    ///
    /// Preferentially uses the OS keychain. If the keychain is unavailable
    /// and the vault does not yet exist, falls back to
    /// `PEKO_MASTER_PASSPHRASE`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if path.exists() {
            return Self::load_existing(path);
        }

        Self::create_new(path)
    }

    /// Load an existing passphrase-protected vault using the provided
    /// passphrase, bypassing environment-variable lookup.
    ///
    /// Returns an error if the vault was created in keychain mode.
    pub fn load_with_passphrase(path: impl AsRef<Path>, passphrase: &SecretString) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read vault: {}", path.display()))?;
        let envelope: VaultEnvelope =
            serde_json::from_slice(&bytes).with_context(|| "failed to parse vault envelope")?;

        if envelope.version != VAULT_VERSION {
            anyhow::bail!(
                "unsupported vault version: {} (expected {})",
                envelope.version,
                VAULT_VERSION
            );
        }

        let salt = envelope
            .salt
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("vault is not passphrase-protected"))?;
        let dek = Self::derive_key_from_passphrase(passphrase.expose_secret(), salt)?;
        let plaintext = Self::decrypt(&envelope, &dek)?;
        let file: VaultFile =
            serde_json::from_slice(&plaintext).with_context(|| "failed to parse vault contents")?;

        Ok(Self {
            path,
            inner: std::sync::RwLock::new(VaultState {
                file,
                dek,
                salt: Some(salt.to_vec()),
            }),
            unlock_method: UnlockMethod::Passphrase,
        })
    }

    /// Create a vault in the given directory with the provided master passphrase.
    ///
    /// This is useful for headless/CI environments where the OS keychain is
    /// not available. The passphrase is used directly to derive the DEK.
    pub fn with_passphrase(path: impl AsRef<Path>, passphrase: &SecretString) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (file, dek, salt) = Self::new_file_with_passphrase(passphrase)?;
        let state = VaultState {
            file,
            dek,
            salt: Some(salt.clone()),
        };
        let vault = Self {
            path,
            inner: std::sync::RwLock::new(state),
            unlock_method: UnlockMethod::Passphrase,
        };
        vault.save_envelope(Some(&salt))?;
        info!(
            "Created new passphrase-protected vault at {}",
            vault.path.display()
        );
        Ok(vault)
    }

    /// Create a test vault using a temporary directory and a known passphrase.
    ///
    /// The vault file is created inside the provided directory.
    #[must_use]
    pub fn for_test(dir: &Path, passphrase: &str) -> Self {
        let path = dir.join(VAULT_FILE_NAME);
        Self::with_passphrase(&path, &SecretString::new(passphrase.into()))
            .expect("test vault creation should succeed")
    }

    /// Return the path to the vault file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return how the vault was unlocked.
    #[must_use]
    pub fn unlock_method(&self) -> UnlockMethod {
        self.unlock_method
    }

    // ------------------------------------------------------------------
    // Entry key namespacing
    // ------------------------------------------------------------------

    fn provider_key(provider: &str) -> String {
        format!("provider:{provider}")
    }

    fn registry_key(host: &str) -> String {
        format!("registry:{host}")
    }

    fn identity_key(key_id: &str) -> String {
        format!("identity:{key_id}")
    }

    fn tunnel_key(runtime_id: &str) -> String {
        format!("tunnel:{runtime_id}")
    }

    // ------------------------------------------------------------------
    // Provider API keys
    // ------------------------------------------------------------------

    /// Get a provider API key.
    pub fn get_provider_key(&self, provider: &str) -> Option<SecretString> {
        let inner = self.inner.read().ok()?;
        match inner.file.entries.get(&Self::provider_key(provider))? {
            VaultEntry::ProviderApiKey { key, .. } => Some(SecretString::new(key.clone().into())),
            _ => None,
        }
    }

    /// Store or overwrite a provider API key.
    pub fn set_provider_key(&self, provider: &str, key: &SecretString) -> Result<()> {
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner.file.entries.insert(
                Self::provider_key(provider),
                VaultEntry::ProviderApiKey {
                    provider: provider.to_string(),
                    key: key.expose_secret().to_string(),
                },
            );
        }
        self.save()
    }

    /// Remove a provider API key.
    pub fn delete_provider_key(&self, provider: &str) -> Result<bool> {
        let removed = {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner
                .file
                .entries
                .remove(&Self::provider_key(provider))
                .is_some()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Return all provider ids that have a stored API key.
    #[must_use]
    pub fn list_providers(&self) -> Vec<String> {
        let inner = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut providers: Vec<String> = inner
            .file
            .entries
            .values()
            .filter_map(|e| match e {
                VaultEntry::ProviderApiKey { provider, .. } => Some(provider.clone()),
                _ => None,
            })
            .collect();
        providers.sort();
        providers.dedup();
        providers
    }

    /// Cheap format check for a provider key.
    pub fn test_provider_key(&self, provider: &str) -> Option<bool> {
        let key = self.get_provider_key(provider)?;
        let s = key.expose_secret();
        let ok = match provider {
            "openai" | "azure-openai" | "azure" | "openrouter" | "together" | "fireworks"
            | "groq" | "deepseek" | "xai" | "grok" | "moonshot" | "kimi" => {
                s.starts_with("sk-") || s.len() > 10
            }
            "anthropic" => s.starts_with("sk-ant-") || s.len() > 10,
            "ollama" => true,
            _ => s.len() > 4 && !s.trim().is_empty(),
        };
        Some(ok)
    }

    // ------------------------------------------------------------------
    // Registry token
    // ------------------------------------------------------------------

    /// Get the stored registry token, if any.
    ///
    /// Returns the first registry token found. Callers that know the host can
    /// use [`Self::get_registry_token_for_host`].
    pub fn get_registry_token(&self) -> Option<RegistryToken> {
        let inner = self.inner.read().ok()?;
        inner.file.entries.values().find_map(|e| match e {
            VaultEntry::RegistryToken {
                host,
                token,
                namespace,
            } => Some(RegistryToken {
                host: host.clone(),
                token: token.clone(),
                namespace: namespace.clone(),
            }),
            _ => None,
        })
    }

    /// Get the registry token for a specific host.
    pub fn get_registry_token_for_host(&self, host: &str) -> Option<RegistryToken> {
        let inner = self.inner.read().ok()?;
        match inner.file.entries.get(&Self::registry_key(host))? {
            VaultEntry::RegistryToken {
                host,
                token,
                namespace,
            } => Some(RegistryToken {
                host: host.clone(),
                token: token.clone(),
                namespace: namespace.clone(),
            }),
            _ => None,
        }
    }

    /// Store or overwrite the registry token for a host.
    pub fn set_registry_token(
        &self,
        host: &str,
        token: &str,
        namespace: Option<&str>,
    ) -> Result<()> {
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner.file.entries.insert(
                Self::registry_key(host),
                VaultEntry::RegistryToken {
                    host: host.to_string(),
                    token: token.to_string(),
                    namespace: namespace.map(String::from),
                },
            );
        }
        self.save()
    }

    /// Clear the registry token for a host.
    pub fn clear_registry_token(&self, host: &str) -> Result<bool> {
        let removed = {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner
                .file
                .entries
                .remove(&Self::registry_key(host))
                .is_some()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    // ------------------------------------------------------------------
    // Identity private key
    // ------------------------------------------------------------------

    /// Store a runtime identity private key.
    pub fn set_identity_private_key(&self, key_id: &str, algorithm: &str, key: &str) -> Result<()> {
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner.file.entries.insert(
                Self::identity_key(key_id),
                VaultEntry::IdentityPrivateKey {
                    key_id: key_id.to_string(),
                    algorithm: algorithm.to_string(),
                    key: key.to_string(),
                },
            );
        }
        self.save()
    }

    /// Get a runtime identity private key by key id.
    pub fn get_identity_private_key(&self, key_id: &str) -> Option<SecretString> {
        let inner = self.inner.read().ok()?;
        match inner.file.entries.get(&Self::identity_key(key_id))? {
            VaultEntry::IdentityPrivateKey { key, .. } => {
                Some(SecretString::new(key.clone().into()))
            }
            _ => None,
        }
    }

    /// Remove a runtime identity private key.
    pub fn delete_identity_private_key(&self, key_id: &str) -> Result<bool> {
        let removed = {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner
                .file
                .entries
                .remove(&Self::identity_key(key_id))
                .is_some()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    // ------------------------------------------------------------------
    // Tunnel private key
    // ------------------------------------------------------------------

    /// Store a PekoHub tunnel private key.
    pub fn set_tunnel_private_key(&self, runtime_id: &str, key: &str) -> Result<()> {
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner.file.entries.insert(
                Self::tunnel_key(runtime_id),
                VaultEntry::TunnelPrivateKey {
                    runtime_id: runtime_id.to_string(),
                    key: key.to_string(),
                },
            );
        }
        self.save()
    }

    /// Get a PekoHub tunnel private key by runtime id.
    pub fn get_tunnel_private_key(&self, runtime_id: &str) -> Option<SecretString> {
        let inner = self.inner.read().ok()?;
        match inner.file.entries.get(&Self::tunnel_key(runtime_id))? {
            VaultEntry::TunnelPrivateKey { key, .. } => Some(SecretString::new(key.clone().into())),
            _ => None,
        }
    }

    /// Remove a PekoHub tunnel private key.
    pub fn delete_tunnel_private_key(&self, runtime_id: &str) -> Result<bool> {
        let removed = {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner
                .file
                .entries
                .remove(&Self::tunnel_key(runtime_id))
                .is_some()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    // ------------------------------------------------------------------
    // Generic entry access
    // ------------------------------------------------------------------

    /// Return a reference to a raw vault entry.
    pub fn get_entry(&self, key: &str) -> Option<VaultEntry> {
        let inner = self.inner.read().ok()?;
        inner.file.entries.get(key).cloned()
    }

    /// Remove an arbitrary entry.
    pub fn delete_entry(&self, key: &str) -> Result<bool> {
        let removed = {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner.file.entries.remove(key).is_some()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Return all entry keys in the vault.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        let inner = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut keys: Vec<String> = inner.file.entries.keys().cloned().collect();
        keys.sort();
        keys
    }

    // ------------------------------------------------------------------
    // Persistence
    // ------------------------------------------------------------------

    /// Persist the vault to disk.
    pub fn save(&self) -> Result<()> {
        let inner = self
            .inner
            .read()
            .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
        let salt = inner.salt.clone();
        Self::write_envelope(&self.path, &inner.dek, salt.as_deref(), &inner.file)
    }

    /// Rotate the DEK and re-encrypt the vault.
    ///
    /// Only supported for keychain-backed vaults.
    pub fn rotate_key(&self) -> Result<()> {
        if self.unlock_method != UnlockMethod::Keychain {
            anyhow::bail!("key rotation is only supported for keychain-backed vaults");
        }

        let new_dek = Self::generate_dek();
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            Self::store_dek_in_keychain(&new_dek)?;
            inner.dek = new_dek;
        }
        self.save()?;
        info!("Rotated vault DEK and re-encrypted {}", self.path.display());
        Ok(())
    }

    // ------------------------------------------------------------------
    // SecretStore trait integration
    // ------------------------------------------------------------------

    fn validate_account(
        account: &str,
    ) -> Result<(), crate::common::secret_store::SecretStoreError> {
        if account.is_empty() {
            return Err(
                crate::common::secret_store::SecretStoreError::InvalidAccount(
                    "empty account name".to_string(),
                ),
            );
        }
        if account.len() > 128 {
            return Err(
                crate::common::secret_store::SecretStoreError::InvalidAccount(format!(
                    "account name too long ({} > 128 chars)",
                    account.len()
                )),
            );
        }
        if !account
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        {
            return Err(
                crate::common::secret_store::SecretStoreError::InvalidAccount(format!(
                    "account name '{account}' contains disallowed characters"
                )),
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn load_existing(path: PathBuf) -> Result<Self> {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read vault: {}", path.display()))?;
        let envelope: VaultEnvelope =
            serde_json::from_slice(&bytes).with_context(|| "failed to parse vault envelope")?;

        if envelope.version != VAULT_VERSION {
            anyhow::bail!(
                "unsupported vault version: {} (expected {})",
                envelope.version,
                VAULT_VERSION
            );
        }

        let (dek, unlock_method, salt) = if let Some(salt) = envelope.salt.as_deref() {
            // Passphrase mode.
            let passphrase =
                Self::passphrase_from_env_or_test_fallback().ok_or(VaultError::NoPassphrase)?;
            let dek = Self::derive_key_from_passphrase(passphrase.expose_secret(), salt)?;
            (dek, UnlockMethod::Passphrase, Some(salt.to_vec()))
        } else {
            // Keychain mode.
            let dek = Self::retrieve_dek_from_keychain()?;
            (dek, UnlockMethod::Keychain, None)
        };

        let plaintext = Self::decrypt(&envelope, &dek)?;
        let file: VaultFile =
            serde_json::from_slice(&plaintext).with_context(|| "failed to parse vault contents")?;

        Ok(Self {
            path,
            inner: std::sync::RwLock::new(VaultState { file, dek, salt }),
            unlock_method,
        })
    }

    fn create_new(path: PathBuf) -> Result<Self> {
        // In test builds, never probe or use the OS keychain. Tests run in
        // parallel and may be executed headless, so always derive the DEK from
        // PEKO_MASTER_PASSPHRASE (if set) or the test fallback. This avoids
        // keychain permission dialogs during local `cargo test` runs and keeps
        // CI deterministic.
        #[cfg(test)]
        {
            let passphrase = Self::passphrase_from_env_or_test_fallback()
                .expect("test passphrase fallback is always available");
            Self::with_passphrase(&path, &passphrase)
        }

        #[cfg(not(test))]
        {
            let keychain = crate::identity::keychain::KeychainStorage::with_service(
                KEYCHAIN_SERVICE.to_string(),
            );
            let (file, dek, salt, unlock_method) = if keychain.is_available() {
                // If a DEK already exists in the keychain, reuse it so that a
                // deleted vault file can be recreated without destroying the
                // key needed to decrypt any backups of the old file.
                let dek = match Self::try_retrieve_dek_from_keychain() {
                    Ok(Some(dek)) => dek,
                    Ok(None) => {
                        let dek = Self::generate_dek();
                        Self::store_dek_in_keychain(&dek)?;
                        dek
                    }
                    Err(e) => return Err(e),
                };
                (VaultFile::default(), dek, None, UnlockMethod::Keychain)
            } else {
                let passphrase =
                    Self::passphrase_from_env_or_test_fallback().ok_or(VaultError::NoPassphrase)?;
                let (file, dek, salt) = Self::new_file_with_passphrase(&passphrase)?;
                (file, dek, Some(salt), UnlockMethod::Passphrase)
            };

            let vault = Self {
                path,
                inner: std::sync::RwLock::new(VaultState {
                    file,
                    dek,
                    salt: salt.clone(),
                }),
                unlock_method,
            };
            vault.save_envelope(salt.as_deref())?;
            info!("Created new vault at {}", vault.path.display());
            Ok(vault)
        }
    }

    /// Return the configured master passphrase, if any.
    ///
    /// In test builds, falls back to a hardcoded test passphrase so that unit
    /// tests do not require an OS keychain or environment variable to create a
    /// vault. Production builds only use `PEKO_MASTER_PASSPHRASE`.
    fn passphrase_from_env_or_test_fallback() -> Option<SecretString> {
        std::env::var(MASTER_PASSPHRASE_ENV)
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| SecretString::new(s.into()))
            .or_else(|| {
                #[cfg(test)]
                {
                    Some(SecretString::new(TEST_MASTER_PASSPHRASE.into()))
                }
                #[cfg(not(test))]
                {
                    None
                }
            })
    }

    fn new_file_with_passphrase(
        passphrase: &SecretString,
    ) -> Result<(VaultFile, Vec<u8>, Vec<u8>)> {
        let mut salt = vec![0u8; 32];
        OsRng.fill_bytes(&mut salt);
        let dek = Self::derive_key_from_passphrase(passphrase.expose_secret(), &salt)?;
        Ok((VaultFile::default(), dek, salt))
    }

    fn save_envelope(&self, salt: Option<&[u8]>) -> Result<()> {
        let inner = self
            .inner
            .read()
            .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
        Self::write_envelope(&self.path, &inner.dek, salt, &inner.file)
    }

    fn write_envelope(
        path: &Path,
        dek: &[u8],
        salt: Option<&[u8]>,
        file: &VaultFile,
    ) -> Result<()> {
        let plaintext =
            serde_json::to_vec(file).with_context(|| "failed to serialize vault contents")?;
        let mut nonce = vec![0u8; NONCE_LENGTH];
        OsRng.fill_bytes(&mut nonce);

        let key = Key::<Aes256Gcm>::from_slice(dek);
        let cipher = Aes256Gcm::new(key);
        let nonce_slice = Nonce::from_slice(&nonce);
        let ciphertext = cipher
            .encrypt(nonce_slice, plaintext.as_ref())
            .map_err(|e| anyhow::anyhow!("failed to encrypt vault: {e:?}"))?;

        let envelope = VaultEnvelope {
            version: VAULT_VERSION,
            salt: salt.map(|s| s.to_vec()),
            nonce,
            ciphertext,
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create vault directory: {parent:?}"))?;
        }

        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&envelope)?)
            .with_context(|| format!("failed to write vault temp file: {tmp:?}"))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("failed to finalize vault file: {path:?}"))?;

        #[cfg(unix)]
        {
            let permissions = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, permissions)
                .with_context(|| "failed to set vault file permissions")?;
        }

        Ok(())
    }

    fn decrypt(envelope: &VaultEnvelope, dek: &[u8]) -> Result<Vec<u8>> {
        let key = Key::<Aes256Gcm>::from_slice(dek);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&envelope.nonce);
        cipher
            .decrypt(nonce, envelope.ciphertext.as_ref())
            .map_err(|e| anyhow::anyhow!("failed to decrypt vault (wrong key?): {e:?}").into())
    }

    fn generate_dek() -> Vec<u8> {
        let mut dek = vec![0u8; KEY_LENGTH];
        OsRng.fill_bytes(&mut dek);
        dek
    }

    fn store_dek_in_keychain(dek: &[u8]) -> Result<()> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .with_context(|| "failed to create keychain entry for vault DEK")?;
        let dek_hex = hex::encode(dek);
        entry
            .set_password(&dek_hex)
            .with_context(|| "failed to store vault DEK in OS keychain")?;
        Ok(())
    }

    /// Try to retrieve an existing DEK from the OS keychain.
    ///
    /// Returns `Ok(None)` if no entry exists, `Ok(Some(dek))` if a valid DEK
    /// is found, and propagates any other keychain error.
    fn try_retrieve_dek_from_keychain() -> Result<Option<Vec<u8>>> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .with_context(|| "failed to create keychain entry for vault DEK")?;
        match entry.get_password() {
            Ok(dek_hex) => {
                let dek = hex::decode(&dek_hex)
                    .with_context(|| "vault DEK in keychain is not valid hex")?;
                Ok(Some(dek))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(e)
                .context("failed to retrieve vault DEK from OS keychain")
                .into()),
        }
    }

    fn retrieve_dek_from_keychain() -> Result<Vec<u8>> {
        Self::try_retrieve_dek_from_keychain()?
            .ok_or_else(|| anyhow::anyhow!("no vault DEK found in OS keychain"))
    }

    fn derive_key_from_passphrase(passphrase: &str, salt: &[u8]) -> Result<Vec<u8>> {
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(
                ARGON2_MEMORY_COST,
                ARGON2_TIME_COST,
                ARGON2_PARALLELISM,
                None,
            )
            .map_err(|e| anyhow::anyhow!("invalid Argon2 params: {e}"))?,
        );
        let mut key = vec![0u8; KEY_LENGTH];
        argon2
            .hash_password_into(passphrase.as_bytes(), salt, &mut key)
            .map_err(|e| anyhow::anyhow!("Argon2 derivation failed: {e:?}"))?;
        Ok(key)
    }
}

/// Owned registry token entry.
#[derive(Debug, Clone)]
pub struct RegistryToken {
    pub host: String,
    pub token: String,
    pub namespace: Option<String>,
}

impl crate::common::secret_store::SecretStore for Vault {
    fn get(
        &self,
        account: &str,
    ) -> Result<Option<SecretString>, crate::common::secret_store::SecretStoreError> {
        Self::validate_account(account)?;
        Ok(self.get_provider_key(account))
    }

    fn set(
        &self,
        account: &str,
        secret: &SecretString,
    ) -> Result<(), crate::common::secret_store::SecretStoreError> {
        Self::validate_account(account)?;
        self.set_provider_key(account, secret)
            .map_err(|e| crate::common::secret_store::SecretStoreError::Backend(e.to_string()))
    }

    fn delete(&self, account: &str) -> Result<bool, crate::common::secret_store::SecretStoreError> {
        Self::validate_account(account)?;
        self.delete_provider_key(account)
            .map_err(|e| crate::common::secret_store::SecretStoreError::Backend(e.to_string()))
    }

    fn list_accounts(&self) -> Result<Vec<String>, crate::common::secret_store::SecretStoreError> {
        Ok(self.list_providers())
    }

    fn test_format(
        &self,
        account: &str,
    ) -> Result<Option<bool>, crate::common::secret_store::SecretStoreError> {
        Self::validate_account(account)?;
        Ok(self.test_provider_key(account))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;
    use tempfile::TempDir;

    #[test]
    fn test_passphrase_vault_roundtrip() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        assert_eq!(vault.unlock_method(), UnlockMethod::Passphrase);

        vault
            .set_provider_key("openai", &SecretString::new("sk-test".into()))
            .unwrap();
        let key = vault.get_provider_key("openai").unwrap();
        assert_eq!(key.expose_secret(), "sk-test");

        // Reload from disk using the explicit passphrase.
        let reloaded =
            Vault::load_with_passphrase(vault.path(), &SecretString::new("test-passphrase".into()))
                .unwrap();
        let reloaded_key = reloaded.get_provider_key("openai").unwrap();
        assert_eq!(reloaded_key.expose_secret(), "sk-test");
    }

    #[test]
    fn test_provider_list_and_delete() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set_provider_key("openai", &SecretString::new("sk-a".into()))
            .unwrap();
        vault
            .set_provider_key("anthropic", &SecretString::new("sk-ant-b".into()))
            .unwrap();

        let mut providers = vault.list_providers();
        providers.sort();
        assert_eq!(providers, vec!["anthropic", "openai"]);

        assert!(vault.delete_provider_key("openai").unwrap());
        assert!(vault.get_provider_key("openai").is_none());
        assert!(!vault.delete_provider_key("openai").unwrap());
    }

    #[test]
    fn test_registry_token() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set_registry_token("pekohub.ai", "ph_abc", Some("acme"))
            .unwrap();
        let token = vault.get_registry_token().unwrap();
        assert_eq!(token.host, "pekohub.ai");
        assert_eq!(token.token, "ph_abc");
        assert_eq!(token.namespace, Some("acme".to_string()));

        assert!(vault.clear_registry_token("pekohub.ai").unwrap());
        assert!(vault.get_registry_token().is_none());
    }

    #[test]
    fn test_identity_key_storage() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set_identity_private_key("did:key:z6MkTest#keys-1", "ed25519-raw-base64", "dGVzdA==")
            .unwrap();
        let key = vault
            .get_identity_private_key("did:key:z6MkTest#keys-1")
            .unwrap();
        assert_eq!(key.expose_secret(), "dGVzdA==");
    }

    #[test]
    fn test_tunnel_key_storage() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set_tunnel_private_key("did:key:z6MkTunnel", "dHVubmVsLWtleQ==")
            .unwrap();
        let key = vault.get_tunnel_private_key("did:key:z6MkTunnel").unwrap();
        assert_eq!(key.expose_secret(), "dHVubmVsLWtleQ==");
    }

    #[test]
    fn test_secret_store_trait() {
        use crate::common::secret_store::SecretStore;

        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set("openai", &SecretString::new("sk-trait".into()))
            .unwrap();
        let got = vault.get("openai").unwrap().unwrap();
        assert_eq!(got.expose_secret(), "sk-trait");

        let accounts = vault.list_accounts().unwrap();
        assert_eq!(accounts, vec!["openai"]);

        assert!(vault.delete("openai").unwrap());
        assert!(vault.get("openai").unwrap().is_none());
    }
}
