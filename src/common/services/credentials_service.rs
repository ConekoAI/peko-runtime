//! Credentials service - registry token access
//!
//! Provides a thin service-layer API over the encrypted vault for the
//! registry token. Other callers should use `crate::common::vault::Vault`
//! directly for provider API keys, identity keys, and tunnel keys.

use crate::commands::GlobalPaths;
use crate::common::vault::Vault;
use anyhow::{Context, Result};

/// Service for accessing the registry token stored in the vault.
#[derive(Debug)]
pub struct CredentialsService {
    vault: Vault,
}

/// Registry credential entry.
#[derive(Debug, Clone)]
pub struct RegistryCredential {
    pub token: String,
    pub registry_host: String,
    pub user_namespace: Option<String>,
}

impl CredentialsService {
    /// Load the credentials service bound to the given paths.
    pub fn new(paths: GlobalPaths) -> Result<Self> {
        let vault = Vault::load(paths.resolver().vault())
            .with_context(|| "failed to load credential vault")?;
        Ok(Self { vault })
    }

    /// Set (or overwrite) registry credentials.
    pub fn set_registry_token(
        &self,
        token: String,
        host: String,
        namespace: Option<String>,
    ) -> Result<()> {
        self.vault
            .set_registry_token(&host, &token, namespace.as_deref())
            .with_context(|| "failed to save registry token")
    }

    /// Get registry credentials.
    pub fn get_registry_token(&self) -> Result<Option<RegistryCredential>> {
        Ok(self.vault.get_registry_token().map(|t| RegistryCredential {
            token: t.token.to_string(),
            registry_host: t.host.to_string(),
            user_namespace: t.namespace.map(String::from),
        }))
    }

    /// Clear registry credentials for the given host.
    ///
    /// Returns `true` if a token was cleared.
    pub fn clear_registry_token(&self, host: &str) -> Result<bool> {
        self.vault
            .clear_registry_token(host)
            .with_context(|| "failed to clear registry token")
    }

    /// Return the path to the vault file.
    #[must_use]
    pub fn vault_path(&self) -> &std::path::Path {
        self.vault.path()
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

        std::env::set_var("PEKO_MASTER_PASSPHRASE", "test-credentials-service");
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
    fn test_registry_token_roundtrip() {
        let paths = temp_paths();
        let service = CredentialsService::new(paths).unwrap();

        assert!(service.get_registry_token().unwrap().is_none());

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

        assert!(service.clear_registry_token("pekohub.com").unwrap());
        assert!(service.get_registry_token().unwrap().is_none());
    }
}
