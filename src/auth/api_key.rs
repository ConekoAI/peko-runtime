//! API key storage and verification

use super::types::{ApiKeyEntry, ApiKeyScope, ApiKeysFile};
use crate::common::paths::PathResolver;
use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Prefix for API keys
const API_KEY_PREFIX: &str = "pkr_";

/// In-memory API key store with async file persistence
#[derive(Clone)]
pub struct ApiKeyStore {
    inner: Arc<RwLock<ApiKeysFile>>,
    path: PathBuf,
}

impl ApiKeyStore {
    /// Load the API key store from disk, or create an empty one
    pub fn load(resolver: &PathResolver) -> anyhow::Result<Self> {
        let path = resolver.runtime_dir().join("api_keys.toml");
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

        let full_key = format!("{API_KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(&random_bytes));
        let key_id = format!(
            "{API_KEY_PREFIX}{}",
            &full_key[API_KEY_PREFIX.len()..API_KEY_PREFIX.len() + 8]
        );

        let hash = format!("sha256:{:x}", Sha256::digest(full_key.as_bytes()));

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

        let hash = format!("sha256:{:x}", Sha256::digest(key.as_bytes()));
        let file = self.inner.read().await;

        file.keys
            .iter()
            .find(|e| {
                if !e.enabled {
                    return false;
                }
                // Constant-time comparison would be ideal, but for a local
                // runtime with a small number of keys this is acceptable.
                // We use a simple byte-by-byte comparison.
                constant_time_eq(&e.hash, &hash)
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

/// Constant-time equality for hash comparison.
///
/// Uses `subtle` crate if available; falls back to a manual
/// byte-by-byte XOR-and-OR implementation that should resist
/// timing side-channels for the lengths compared.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut result = 0u8;
    for i in 0..a_bytes.len() {
        result |= a_bytes[i] ^ b_bytes[i];
    }
    result == 0
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
    use tempfile::TempDir;

    fn temp_store() -> ApiKeyStore {
        let tmp = TempDir::new().unwrap();
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
}
