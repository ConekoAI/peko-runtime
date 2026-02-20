//! Secret Manager for Pekobot
//!
//! Provides secure storage for API keys, tokens, and credentials with:
//! - AES-256-GCM encryption at rest
//! - Argon2id key derivation
//! - Global and per-agent scoping
//! - Audit logging
//!
//! ## Example
//!
//! ```rust
//! use pekobot::secrets::{SecretManager, SecretScope, SecretType};
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Open or create secret store
//! let mut manager = SecretManager::new().await?;
//!
//! // Unlock with master password (first time sets it up)
//! manager.unlock("my-master-password").await?;
//!
//! // Store a global secret
//! manager.set(
//!     "OPENAI_API_KEY",
//!     SecretScope::Global,
//!     "sk-...",
//!     SecretType::ApiKey,
//!     None,
//! ).await?;
//!
//! // Retrieve the secret
//! let api_key = manager.get("OPENAI_API_KEY", &SecretScope::Global).await?;
//! # Ok(())
//! # }
//! ```

pub mod crypto;
pub mod resolver;
pub mod store;
pub mod types;

pub use resolver::{ResolveSecrets, SecretResolver};
pub use types::{
    AuditEntry, AuditEvent, SecretAccessControl, SecretEntry, SecretMetadata,
    SecretPermission, SecretScope, SecretType,
};

use crate::secrets::store::SecretStore;
use secrecy::SecretString;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// High-level secret manager interface
pub struct SecretManager {
    /// The underlying secret store
    store: SecretStore,
    /// Path to the store file
    path: PathBuf,
    /// Whether the store uses a master password
    has_master_password: bool,
}

impl SecretManager {
    /// Create or open the default secret store
    pub async fn new() -> anyhow::Result<Self> {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pekobot");
        
        std::fs::create_dir_all(&data_dir)?;
        
        let path = data_dir.join("secrets.db");
        let store = SecretStore::open(&path)?;

        // Check if a master password is configured
        // (We'll implement this check later when we add the keyring integration)
        let has_master_password = false;

        Ok(Self {
            store,
            path,
            has_master_password,
        })
    }

    /// Open a secret store at a specific path
    pub fn open(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let store = SecretStore::open(&path)?;

        Ok(Self {
            store,
            path,
            has_master_password: false,
        })
    }

    /// Check if the store is unlocked
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        self.store.is_unlocked()
    }

    /// Unlock the store with a master password
    ///
    /// If this is the first time and no salt exists, generates a new salt.
    pub async fn unlock(&mut self,
        password: &str,
    ) -> anyhow::Result<()> {
        // For now, use a fixed salt stored alongside the database
        // In production, this should be stored in the OS keychain or derived from a unique device identifier
        let salt_path = self.path.with_extension("salt");
        
        let salt = if salt_path.exists() {
            std::fs::read(&salt_path)?
        } else {
            // Generate new salt
            let mut salt = vec![0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut salt);
            std::fs::write(&salt_path, &salt)?;
            // Set restrictive permissions
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&salt_path, std::fs::Permissions::from_mode(0o600))?;
            }
            salt
        };

        self.store.unlock(password, &salt)?;
        info!("Secret store unlocked");
        
        Ok(())
    }

    /// Lock the store
    pub fn lock(&mut self) {
        self.store.lock();
    }

    /// Store a secret
    pub async fn set(
        &self,
        name: &str,
        scope: SecretScope,
        value: &str,
        secret_type: SecretType,
        metadata: Option<SecretMetadata>,
    ) -> anyhow::Result<SecretEntry> {
        self.store.set(name, &scope, value, secret_type, metadata)
    }

    /// Get a secret value
    pub async fn get(
        &self,
        name: &str,
        scope: &SecretScope,
    ) -> anyhow::Result<Option<String>> {
        self.store.get(name, scope)
    }

    /// Get a secret entry (without value)
    pub async fn get_entry(
        &self,
        name: &str,
        scope: &SecretScope,
    ) -> anyhow::Result<Option<SecretEntry>> {
        self.store.get_entry(name, scope)
    }

    /// List secrets
    pub async fn list(
        &self,
        scope: Option<SecretScope>,
    ) -> anyhow::Result<Vec<SecretEntry>> {
        self.store.list(scope.as_ref())
    }

    /// Delete a secret
    pub async fn delete(
        &self,
        name: &str,
        scope: &SecretScope,
    ) -> anyhow::Result<bool> {
        self.store.delete(name, scope)
    }

    /// Check if an agent has permission to access a secret
    pub async fn check_permission(
        &self,
        secret_name: &str,
        secret_scope: &SecretScope,
        agent_did: Option<&str>,
    ) -> anyhow::Result<SecretPermission> {
        self.store.check_permission(secret_name, secret_scope, agent_did)
    }

    /// Grant permission to an agent for a secret
    pub async fn grant_permission(
        &self,
        secret_name: &str,
        secret_scope: &SecretScope,
        agent_did: Option<&str>,
        permission: SecretPermission,
    ) -> anyhow::Result<SecretAccessControl> {
        self.store.grant_permission(secret_name, secret_scope, agent_did, permission)
    }

    /// Revoke permission from an agent for a secret
    pub async fn revoke_permission(
        &self,
        secret_name: &str,
        secret_scope: &SecretScope,
        agent_did: Option<&str>,
    ) -> anyhow::Result<bool> {
        self.store.revoke_permission(secret_name, secret_scope, agent_did)
    }

    /// Get permissions for a secret
    pub async fn get_permissions(
        &self,
        secret_name: &str,
        secret_scope: &SecretScope,
    ) -> anyhow::Result<Vec<SecretAccessControl>> {
        self.store.get_permissions(secret_name, secret_scope)
    }

    /// Get a secret value with permission check
    pub async fn get_with_permission(
        &self,
        name: &str,
        scope: &SecretScope,
        agent_did: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        self.store.get_with_permission(name, scope, agent_did)
    }

    /// Get the store path
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[tokio::test]
    async fn test_secret_manager_workflow() {
        // Create temporary directory for test store
        let temp_dir = tempfile::tempdir().unwrap();
        let store_path = temp_dir.path().join("test-secrets.db");

        // Create manager
        let mut manager = SecretManager::open(&store_path).unwrap();
        
        // Should be locked initially
        assert!(!manager.is_unlocked());

        // Unlock
        manager.unlock("test-password").await.unwrap();
        assert!(manager.is_unlocked());

        // Store a secret
        let entry = manager.set(
            "TEST_API_KEY",
            SecretScope::Global,
            "sk-test12345",
            SecretType::ApiKey,
            None,
        ).await.unwrap();

        assert_eq!(entry.name, "TEST_API_KEY");
        assert_eq!(entry.secret_type, SecretType::ApiKey);

        // Retrieve
        let value = manager.get("TEST_API_KEY", &SecretScope::Global).await.unwrap();
        assert_eq!(value, Some("sk-test12345".to_string()));

        // Lock
        manager.lock();
        assert!(!manager.is_unlocked());

        // Should fail when locked
        let result = manager.get("TEST_API_KEY", &SecretScope::Global).await;
        assert!(result.is_err());
    }
}
