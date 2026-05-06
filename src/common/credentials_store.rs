//! Credentials store - data model and persistence for API keys and credentials
//!
//! This module provides the low-level data structures and file I/O for credential
//! management. Business logic belongs in `CredentialsService`.

use crate::commands::GlobalPaths;
use anyhow::Result;
use std::collections::HashMap;

/// Credential entry for a single provider
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Credential {
    pub provider: String,
    pub api_key: String,
    pub created_at: String,
}

/// In-memory representation of the credentials file
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct CredentialsStore {
    pub version: u32,
    pub credentials: HashMap<String, Credential>, // key: provider name
}

impl CredentialsStore {
    /// Get a credential by provider name
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<&Credential> {
        self.credentials.get(provider)
    }

    /// Set (or overwrite) a credential for a provider
    pub fn set(&mut self, provider: &str, api_key: String) {
        let credential = Credential {
            provider: provider.to_string(),
            api_key,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.credentials.insert(provider.to_string(), credential);
    }

    /// Remove a credential by provider name
    ///
    /// Returns `true` if a credential was removed.
    pub fn remove(&mut self, provider: &str) -> bool {
        self.credentials.remove(provider).is_some()
    }

    /// Return a sorted list of all provider names
    #[must_use]
    pub fn providers(&self) -> Vec<String> {
        let mut providers: Vec<String> = self
            .credentials
            .values()
            .map(|c| c.provider.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        providers.sort();
        providers
    }
}

/// Load credentials from the standard credentials file
pub fn load_credentials(paths: &GlobalPaths) -> Result<CredentialsStore> {
    let path = paths.config_dir.join("credentials.json");

    if !path.exists() {
        return Ok(CredentialsStore {
            version: 1,
            credentials: HashMap::new(),
        });
    }

    let content = std::fs::read_to_string(&path)?;
    let store: CredentialsStore = serde_json::from_str(&content)?;
    Ok(store)
}

/// Save credentials to the standard credentials file with restricted permissions
pub fn save_credentials(paths: &GlobalPaths, store: &CredentialsStore) -> Result<()> {
    let path = paths.config_dir.join("credentials.json");

    // Ensure config dir exists
    std::fs::create_dir_all(&paths.config_dir)?;

    let content = serde_json::to_string_pretty(store)?;
    std::fs::write(&path, content)?;

    // Set restrictive permissions (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_credentials_store_default() {
        let store = CredentialsStore::default();
        assert_eq!(store.version, 0);
        assert!(store.credentials.is_empty());
    }

    #[test]
    fn test_credentials_store_set_and_get() {
        let mut store = CredentialsStore::default();
        store.set("openai", "sk-test123".to_string());

        let cred = store.get("openai").unwrap();
        assert_eq!(cred.provider, "openai");
        assert_eq!(cred.api_key, "sk-test123");
        assert!(!cred.created_at.is_empty());
    }

    #[test]
    fn test_credentials_store_get_missing() {
        let store = CredentialsStore::default();
        assert!(store.get("missing").is_none());
    }

    #[test]
    fn test_credentials_store_remove() {
        let mut store = CredentialsStore::default();
        store.set("openai", "sk-test".to_string());
        assert!(store.remove("openai"));
        assert!(store.get("openai").is_none());
        assert!(!store.remove("openai"));
    }

    #[test]
    fn test_credentials_store_overwrite() {
        let mut store = CredentialsStore::default();
        store.set("openai", "sk-old".to_string());
        store.set("openai", "sk-new".to_string());

        let cred = store.get("openai").unwrap();
        assert_eq!(cred.api_key, "sk-new");
    }

    #[test]
    fn test_credentials_store_providers_sorted() {
        let mut store = CredentialsStore::default();
        store.set("zeta", "sk-z".to_string());
        store.set("alpha", "sk-a".to_string());
        store.set("beta", "sk-b".to_string());

        let providers = store.providers();
        assert_eq!(providers, vec!["alpha", "beta", "zeta"]);
    }

    #[test]
    fn test_credentials_store_providers_deduplicated() {
        let mut store = CredentialsStore::default();
        // Inserting same provider twice (overwrite) should not duplicate
        store.set("openai", "sk-1".to_string());
        store.set("openai", "sk-2".to_string());

        let providers = store.providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0], "openai");
    }

    #[test]
    fn test_credentials_store_serialization_roundtrip() {
        let mut store = CredentialsStore {
            version: 1,
            credentials: HashMap::new(),
        };
        store.set("openai", "sk-test".to_string());
        store.set("anthropic", "sk-ant-test".to_string());

        let json = serde_json::to_string_pretty(&store).unwrap();
        let deserialized: CredentialsStore = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.version, 1);
        assert_eq!(deserialized.providers(), vec!["anthropic", "openai"]);
        assert_eq!(deserialized.get("openai").unwrap().api_key, "sk-test");
    }
}
