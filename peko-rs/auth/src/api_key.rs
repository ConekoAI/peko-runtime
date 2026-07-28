//! API key storage and verification

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use tokio::sync::RwLock;

use super::types::{ApiKeyEntry, ApiKeyScope, ApiKeysFile};
use crate::host::RuntimePaths;

/// Prefix for API keys
const API_KEY_PREFIX: &str = "pkr_";

/// In-memory API key store with async file persistence
#[derive(Clone)]
pub struct ApiKeyStore {
    inner: Arc<RwLock<ApiKeysFile>>,
    path: PathBuf,
}

impl ApiKeyStore {
    /// Load the API key store from disk, or create an empty one.
    ///
    /// Phase 4 migration: takes `&dyn RuntimePaths` instead of
    /// `&PathResolver` so `peko-auth` doesn't depend on
    /// `crate::common::paths`.
    pub fn load(paths: &dyn RuntimePaths) -> anyhow::Result<Self> {
        let path = paths.runtime_dir().join("api_keys.toml");
        let file = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read API keys file: {path:?}"))?;
            toml::from_str(&content).unwrap_or_default()
        } else {
            ApiKeysFile::default()
        };

        Ok(Self {
            inner: Arc::new(RwLock::new(file)),
            path,
        })
    }

    /// Create an empty store at the given path (for testing)
    #[cfg(test)]
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ApiKeysFile::default())),
            path,
        }
    }

    /// Create a new API key.
    ///
    /// Returns the full key (shown once) and the key ID.
    pub async fn create_key(
        &self,
        name: String,
        scopes: Vec<ApiKeyScope>,
    ) -> anyhow::Result<(String, String)> {
        // Generate random bytes without holding a ThreadRng across await points
        let random_bytes = {
            let mut rng = rand::thread_rng();
            let mut bytes = [0u8; 32];
            rng.fill_bytes(&mut bytes);
            bytes
        };

        let full_key = format!("{API_KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(random_bytes));
        let key_id = format!(
            "{API_KEY_PREFIX}{}",
            &full_key[API_KEY_PREFIX.len()..API_KEY_PREFIX.len() + 8]
        );

        // Argon2id PHC string. Salt is freshly generated via OsRng so each
        // key has a unique salt (default Argon2id params: m=19456 KiB,
        // t=2, p=1). The PHC form is self-describing, so verify_key
        // recovers params from the stored hash.
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(full_key.as_bytes(), &salt)
            .map_err(|e| anyhow::anyhow!("argon2 hash_password failed: {e}"))?
            .to_string();

        let entry = ApiKeyEntry {
            id: key_id.clone(),
            hash,
            name,
            created_at: chrono::Utc::now(),
            last_used_at: None,
            scopes,
            enabled: true,
        };

        {
            let mut file = self.inner.write().await;
            file.keys.push(entry);
        }

        self.save().await?;
        Ok((full_key, key_id))
    }

    /// List all API keys (without hashes)
    pub async fn list_keys(&self) -> Vec<ApiKeyEntry> {
        let file = self.inner.read().await;
        file.keys.clone()
    }

    /// Revoke (disable) an API key by ID
    pub async fn revoke_key(&self, key_id: &str) -> anyhow::Result<bool> {
        let mut file = self.inner.write().await;
        if let Some(entry) = file.keys.iter_mut().find(|k| k.id == key_id) {
            entry.enabled = false;
            drop(file);
            self.save().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Delete an API key by ID
    pub async fn delete_key(&self, key_id: &str) -> anyhow::Result<bool> {
        let mut file = self.inner.write().await;
        let before = file.keys.len();
        file.keys.retain(|k| k.id != key_id);
        let removed = file.keys.len() < before;
        drop(file);
        if removed {
            self.save().await?;
        }
        Ok(removed)
    }

    /// Verify an API key.
    ///
    /// Returns the matching entry if valid, or None if invalid/revoked.
    pub async fn verify_key(&self, key: &str) -> Option<ApiKeyEntry> {
        if !key.starts_with(API_KEY_PREFIX) {
            return None;
        }

        let file = self.inner.read().await;

        // Iterate every enabled key and let argon2's PHC parser recover
        // the stored params from `e.hash`. verify_password is
        // constant-time over the parsed hash and runs in O(memory_cost)
        // per candidate. With a small key inventory this is fine; if
        // the runtime ever ships with thousands of keys, gate this
        // with a prefix-lookup index on `e.id`.
        file.keys
            .iter()
            .find(|e| {
                if !e.enabled {
                    return false;
                }
                let parsed = match PasswordHash::new(&e.hash) {
                    Ok(p) => p,
                    Err(_) => return false,
                };
                Argon2::default()
                    .verify_password(key.as_bytes(), &parsed)
                    .is_ok()
            })
            .cloned()
    }

    /// Get a key entry by ID (for updating last_used_at)
    pub async fn get_entry(&self, key_id: &str) -> Option<ApiKeyEntry> {
        let file = self.inner.read().await;
        file.keys.iter().find(|k| k.id == key_id).cloned()
    }

    /// Save the store to disk
    pub async fn save(&self) -> anyhow::Result<()> {
        let file = self.inner.read().await;
        let toml = toml::to_string_pretty(&*file)?;
        drop(file);

        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.path, toml).await?;
        Ok(())
    }

    /// Extract the key ID prefix from a full API key
    #[must_use]
    pub fn extract_key_id(key: &str) -> String {
        if key.starts_with(API_KEY_PREFIX) && key.len() >= API_KEY_PREFIX.len() + 8 {
            format!(
                "{API_KEY_PREFIX}{}",
                &key[API_KEY_PREFIX.len()..API_KEY_PREFIX.len() + 8]
            )
        } else {
            key.to_string()
        }
    }
}

/// API key verifier — thin wrapper around ApiKeyStore
#[derive(Clone)]
pub struct ApiKeyVerifier {
    store: ApiKeyStore,
}

impl ApiKeyVerifier {
    /// Create a new verifier from a store
    #[must_use]
    pub fn new(store: ApiKeyStore) -> Self {
        Self { store }
    }

    /// Verify an API key string.
    ///
    /// Returns the key entry if valid.
    pub async fn verify(&self, key: &str) -> Option<ApiKeyEntry> {
        self.store.verify_key(key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> ApiKeyStore {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("api_keys.toml");
        ApiKeyStore::with_path(path)
    }

    #[tokio::test]
    async fn test_create_and_verify_key() {
        let store = temp_store();
        let (full_key, key_id) = store
            .create_key("Test Key".to_string(), vec![ApiKeyScope::Read])
            .await
            .unwrap();

        assert!(full_key.starts_with("pkr_"));
        assert!(key_id.starts_with("pkr_"));
        assert_eq!(key_id.len(), 4 + 8); // "pkr_" + 8 chars

        let entry = store.verify_key(&full_key).await.unwrap();
        assert_eq!(entry.id, key_id);
        assert_eq!(entry.name, "Test Key");
        assert!(entry.enabled);
    }

    #[tokio::test]
    async fn test_revoke_key() {
        let store = temp_store();
        let (full_key, key_id) = store
            .create_key("Test Key".to_string(), vec![ApiKeyScope::Read])
            .await
            .unwrap();

        assert!(store.verify_key(&full_key).await.is_some());
        assert!(store.revoke_key(&key_id).await.unwrap());
        assert!(store.verify_key(&full_key).await.is_none());
    }

    #[tokio::test]
    async fn test_invalid_key() {
        let store = temp_store();
        assert!(store.verify_key("pkr_invalidkey123").await.is_none());
    }

    #[tokio::test]
    async fn test_list_keys() {
        let store = temp_store();
        store
            .create_key("Key 1".to_string(), vec![ApiKeyScope::Read])
            .await
            .unwrap();
        store
            .create_key("Key 2".to_string(), vec![ApiKeyScope::Write])
            .await
            .unwrap();

        let keys = store.list_keys().await;
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn test_extract_key_id() {
        assert_eq!(
            ApiKeyStore::extract_key_id("pkr_aB3dEf9GhI2jK4lM5nO6pQ7rS8tU0vW1xY2zA3bC4dE"),
            "pkr_aB3dEf9G"
        );
    }

    /// API key hashes must be Argon2id PHC strings, not bare SHA-256 hex
    /// digests. This pins R3 (argon2 migration): if anyone reintroduces a
    /// `sha256:` prefix, this assertion catches it.
    #[tokio::test]
    async fn test_hash_is_argon2id_phc() {
        let store = temp_store();
        let (full_key, _key_id) = store
            .create_key("Argon2 Shape".to_string(), vec![ApiKeyScope::Read])
            .await
            .unwrap();
        let entry = store.verify_key(&full_key).await.expect("fresh key verifies");
        assert!(
            entry.hash.starts_with("$argon2id$"),
            "expected argon2id PHC string, got: {}",
            entry.hash
        );
        assert!(entry.hash.contains("$v=19$"), "PHC string missing v=19");
        assert!(!entry.hash.starts_with("sha256:"));
    }

    /// Verify a tampered PHC string (last char flipped) rejects the key.
    /// Catches "hash stored but verify short-circuited" regressions.
    #[tokio::test]
    async fn test_tampered_hash_rejects() {
        let store = temp_store();
        let (full_key, key_id) = store
            .create_key("Tamper".to_string(), vec![ApiKeyScope::Read])
            .await
            .unwrap();

        // Mutate the stored hash so PHC parse fails or verify fails.
        let mut file = store.inner.write().await;
        let entry = file
            .keys
            .iter_mut()
            .find(|k| k.id == key_id)
            .expect("just-created key");
        // Flip a character near the end of the hash (within the
        // base64-encoded output section).
        let last_char = entry.hash.chars().last().unwrap();
        let replacement = if last_char == 'A' { 'B' } else { 'A' };
        entry.hash.pop();
        entry.hash.push(replacement);
        drop(file);

        assert!(store.verify_key(&full_key).await.is_none());
    }

    /// Different keys must produce different PHC strings (salt uniqueness
    /// is enforced by SaltString::generate(OsRng), so two created keys
    /// cannot share a stored hash).
    #[tokio::test]
    async fn test_two_keys_have_distinct_hashes() {
        let store = temp_store();
        let (_k1, _) = store
            .create_key("K1".to_string(), vec![ApiKeyScope::Read])
            .await
            .unwrap();
        let (_k2, _) = store
            .create_key("K2".to_string(), vec![ApiKeyScope::Read])
            .await
            .unwrap();
        let keys = store.list_keys().await;
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0].hash, keys[1].hash);
    }

    /// Critical path: full API key lifecycle — create → verify → use in CallerContext → permission check
    #[tokio::test]
    async fn test_api_key_e2e_lifecycle() {
        use crate::caller::CallerContext;
        use crate::permissions::{check_permission, Action, Resource};

        let store = temp_store();

        // 1. Create a key with Read + Write scopes
        let (full_key, key_id) = store
            .create_key(
                "E2E Test Key".to_string(),
                vec![ApiKeyScope::Read, ApiKeyScope::Write],
            )
            .await
            .unwrap();

        // 2. Verify via ApiKeyVerifier
        let verifier = ApiKeyVerifier::new(store.clone());
        let entry = verifier.verify(&full_key).await.unwrap();
        assert_eq!(entry.id, key_id);
        assert_eq!(entry.name, "E2E Test Key");

        // 3. Build CallerContext from verified entry
        let caller = CallerContext::from_api_key(entry.id.clone(), entry.scopes.clone());

        // 4. Permission checks
        assert!(check_permission(&caller, &Resource::System, Action::Read).is_ok());
        assert!(check_permission(&caller, &Resource::System, Action::Write).is_ok());
        assert!(check_permission(&caller, &Resource::System, Action::Execute).is_ok());
        assert_eq!(
            check_permission(&caller, &Resource::System, Action::Admin).unwrap_err(),
            crate::permissions::AuthError::PermissionDenied
        );

        // 5. Revoke and verify denied
        assert!(store.revoke_key(&key_id).await.unwrap());
        assert!(verifier.verify(&full_key).await.is_none());
    }
}
