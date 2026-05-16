//! Credentials service - business logic for API key and credential management
//!
//! Provides a clean service-layer API used by both CLI commands and other
//! components. Delegates persistence to `CredentialsStore`.

use crate::commands::GlobalPaths;
use crate::common::credentials_store::{
    load_credentials, save_credentials, Credential, CredentialsStore, RegistryCredential,
};
use anyhow::Result;

/// Service for managing credentials
///
/// Encapsulates load/save/validate operations. All file I/O lives in the
/// underlying `CredentialsStore` module, not in command handlers.
#[derive(Debug, Clone)]
pub struct CredentialsService {
    paths: GlobalPaths,
}

impl CredentialsService {
    /// Create a new credentials service bound to the given paths
    #[must_use]
    pub fn new(paths: GlobalPaths) -> Self {
        Self { paths }
    }

    /// Load the credentials store from disk
    pub fn load(&self) -> Result<CredentialsStore> {
        load_credentials(&self.paths)
    }

    /// Save the credentials store to disk
    pub fn save(&self, store: &CredentialsStore) -> Result<()> {
        save_credentials(&self.paths, store)
    }

    /// Set (or overwrite) a credential for a provider
    pub fn set(&self, provider: &str, api_key: String) -> Result<()> {
        let mut store = self.load()?;
        store.set(provider, api_key);
        self.save(&store)
    }

    /// Remove a credential by provider name
    ///
    /// Returns `true` if a credential was removed.
    pub fn remove(&self, provider: &str) -> Result<bool> {
        let mut store = self.load()?;
        let removed = store.remove(provider);
        if removed {
            self.save(&store)?;
        }
        Ok(removed)
    }

    /// List all configured providers
    pub fn list_providers(&self) -> Result<Vec<String>> {
        let store = self.load()?;
        Ok(store.providers())
    }

    /// Get a credential by provider name
    pub fn get(&self, provider: &str) -> Result<Option<Credential>> {
        let store = self.load()?;
        Ok(store.get(provider).cloned())
    }

    /// Get the API key for a provider (used by agent creation)
    pub fn get_api_key(&self, provider: &str) -> Result<Option<String>> {
        let store = self.load()?;
        Ok(store.get(provider).map(|c| c.api_key.clone()))
    }

    /// Test whether a credential has a valid key format
    ///
    /// Returns `Some(true)` if valid, `Some(false)` if invalid format,
    /// and `None` if the provider is not found.
    pub fn test_provider(&self, provider: &str) -> Result<Option<bool>> {
        let store = self.load()?;
        Ok(store.get(provider).map(|cred| match provider {
            "openai" => cred.api_key.starts_with("sk-"),
            "anthropic" => cred.api_key.starts_with("sk-ant-"),
            _ => cred.api_key.len() > 10,
        }))
    }

    /// Set registry token
    pub fn set_registry_token(
        &self,
        token: String,
        host: String,
        namespace: Option<String>,
    ) -> Result<()> {
        let mut store = self.load()?;
        store.set_registry(token, host, namespace);
        self.save(&store)
    }

    /// Get registry token
    pub fn get_registry_token(&self) -> Result<Option<RegistryCredential>> {
        let store = self.load()?;
        Ok(store.get_registry().cloned())
    }

    /// Clear registry token
    ///
    /// Returns `true` if a token was cleared.
    pub fn clear_registry_token(&self) -> Result<bool> {
        let mut store = self.load()?;
        let cleared = store.clear_registry();
        if cleared {
            self.save(&store)?;
        }
        Ok(cleared)
    }

    /// Return the path to the credentials file
    #[must_use]
    pub fn credentials_path(&self) -> std::path::PathBuf {
        self.paths.config_dir.join("credentials.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Cli;
    use clap::Parser;

    fn temp_paths() -> GlobalPaths {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let temp = std::env::temp_dir().join(format!(
            "PEKO_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&temp);
        let _ = std::fs::create_dir_all(&temp);

        let config_dir = temp.join("config");
        let data_dir = temp.join("data");
        let cache_dir = temp.join("cache");

        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            &config_dir.to_string_lossy(),
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--cache-dir",
            &cache_dir.to_string_lossy(),
            "--user",
            "test",
            "daemon",
            "status",
        ]);
        GlobalPaths::from_cli(&cli)
    }

    #[test]
    fn test_credentials_service_creation() {
        let paths = temp_paths();
        let _service = CredentialsService::new(paths);
    }

    #[test]
    fn test_credentials_service_set_and_get() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths);

        service.set("openai", "sk-test123".to_string()).unwrap();
        let cred = service.get("openai").unwrap().unwrap();
        assert_eq!(cred.provider, "openai");
        assert_eq!(cred.api_key, "sk-test123");
    }

    #[test]
    fn test_credentials_service_get_missing() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths);

        let result = service.get("missing").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_credentials_service_remove() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths);

        service.set("openai", "sk-test".to_string()).unwrap();
        assert!(service.remove("openai").unwrap());
        assert!(!service.remove("openai").unwrap());
    }

    #[test]
    fn test_credentials_service_list_providers() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths);

        service.set("zeta", "sk-z".to_string()).unwrap();
        service.set("alpha", "sk-a".to_string()).unwrap();

        let providers = service.list_providers().unwrap();
        assert_eq!(providers, vec!["alpha", "zeta"]);
    }

    #[test]
    fn test_credentials_service_get_api_key() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths);

        service.set("openai", "sk-secret".to_string()).unwrap();
        assert_eq!(
            service.get_api_key("openai").unwrap(),
            Some("sk-secret".to_string())
        );
        assert_eq!(service.get_api_key("missing").unwrap(), None);
    }

    #[test]
    fn test_credentials_service_test_provider() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths);

        // Valid OpenAI key format
        service.set("openai", "sk-valid".to_string()).unwrap();
        assert_eq!(service.test_provider("openai").unwrap(), Some(true));

        // Invalid OpenAI key format
        service.set("openai", "bad-key".to_string()).unwrap();
        assert_eq!(service.test_provider("openai").unwrap(), Some(false));

        // Valid Anthropic key format
        service
            .set("anthropic", "sk-ant-valid".to_string())
            .unwrap();
        assert_eq!(service.test_provider("anthropic").unwrap(), Some(true));

        // Missing provider
        assert_eq!(service.test_provider("missing").unwrap(), None);
    }

    #[test]
    fn test_credentials_service_load_save_roundtrip() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths);

        service.set("openai", "sk-test".to_string()).unwrap();

        // Load back via a fresh service pointing at the same paths
        let store = service.load().unwrap();
        assert_eq!(store.providers(), vec!["openai"]);
        assert_eq!(store.get("openai").unwrap().api_key, "sk-test");
    }

    #[test]
    fn test_credentials_service_registry_token() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths);

        // Initially no token
        assert!(service.get_registry_token().unwrap().is_none());

        // Set token
        service
            .set_registry_token(
                "ph_abc123".to_string(),
                "pekohub.com".to_string(),
                Some("acme".to_string()),
            )
            .unwrap();

        let token = service.get_registry_token().unwrap().unwrap();
        assert_eq!(token.token, "ph_abc123");
        assert_eq!(token.registry_host, "pekohub.com");
        assert_eq!(token.user_namespace, Some("acme".to_string()));

        // Clear token
        assert!(service.clear_registry_token().unwrap());
        assert!(service.get_registry_token().unwrap().is_none());
        assert!(!service.clear_registry_token().unwrap());
    }
}
