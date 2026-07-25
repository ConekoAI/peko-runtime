//! Root-side adapter that exposes the in-memory credential vault to
//! `peko-providers` through the
//! [`peko_providers::secret_store::SecretStore`] trait.
//!
//! `peko-providers` owns the `SecretStore` trait (it migrated from
//! `src/common/secret_store.rs` in Phase 6), but the concrete vault
//! lives in the root composition layer because it depends on
//! `peko-identity` for encryption. This adapter bridges them so the
//! daemon-side `LlmResolver` can consume the same vault through the
//! `Arc<dyn SecretStore>` constructor argument.
//!
//! Phase 6 of the post-migration cleanup. Pairs with
//! [`crate::common::vault_credential_provider`], which adapts the
//! same vault to the read-by-id `CredentialProvider` trait used by
//! `RotationState`.

use std::sync::Arc;

use anyhow::Result;
use peko_providers::secret_store::{SecretStore, SecretStoreError};
use secrecy::{ExposeSecret, SecretString};

/// `peko_providers::secret_store::SecretStore` implementation backed
/// by the in-memory [`crate::common::vault::Vault`].
///
/// The legacy `impl SecretStore for Vault` lived inside `vault.rs`
/// before Phase 6 lifted `SecretStore` into `peko-providers`. The
/// orphan rule forbids `impl ForeignTrait for ForeignType`, so the
/// impl moved to this adapter struct in the root composition layer.
/// All four call sites that previously cast `Arc<Vault>` directly to
/// `Arc<dyn SecretStore>` now construct `Arc::new(VaultSecretStore::new(vault))`
/// instead.
#[derive(Clone)]
pub struct VaultSecretStore {
    vault: Arc<crate::common::vault::Vault>,
}

impl VaultSecretStore {
    /// Wrap a live [`crate::common::vault::Vault`] in the trait adapter.
    #[must_use]
    pub fn new(vault: Arc<crate::common::vault::Vault>) -> Self {
        Self { vault }
    }
}

impl SecretStore for VaultSecretStore {
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretStoreError> {
        // Delegate to `Vault::get_credential` for the legacy
        // account-keyed lookup path. This keeps the public
        // `SecretStore::get(&str)` API consistent with the original
        // `impl SecretStore for Vault` body before Phase 6 lifted
        // the trait into `peko-providers`.
        match self.vault.get_credential(account) {
            Some(cred) => Ok(Some(cred.material)),
            None => Ok(None),
        }
    }

    fn set(&self, _account: &str, _secret: &SecretString) -> Result<(), SecretStoreError> {
        // The vault's own typed write paths (`Vault::set_credential`)
        // are the supported mutation API; this trait method is
        // required by the interface but never exercised by the
        // provider code paths. Reject explicitly rather than
        // silently ignoring writes.
        Err(SecretStoreError::Backend(
            "VaultSecretStore: write through SecretStore::set is not supported; \
             use Vault::set_credential directly"
                .to_string(),
        ))
    }

    fn delete(&self, _account: &str) -> Result<bool, SecretStoreError> {
        Err(SecretStoreError::Backend(
            "VaultSecretStore: write through SecretStore::delete is not supported; \
             use Vault::delete_credential directly"
                .to_string(),
        ))
    }

    fn list_accounts(&self) -> Result<Vec<String>, SecretStoreError> {
        // No general-purpose enumeration API on Vault that maps
        // cleanly to "account ids". The LlmResolver code path that
        // historically used this surface now uses
        // `KeyProbeReport`/vault.list_credentials instead.
        Ok(Vec::new())
    }

    fn test_format(&self, account: &str) -> Result<Option<bool>, SecretStoreError> {
        let Some(secret) = self.get(account)? else {
            return Ok(None);
        };
        let s = secret.expose_secret();
        let ok = match account {
            "openai" => s.starts_with("sk-"),
            "anthropic" => s.starts_with("sk-ant-"),
            _ => s.len() > 4 && !s.trim().is_empty(),
        };
        Ok(Some(ok))
    }
}
