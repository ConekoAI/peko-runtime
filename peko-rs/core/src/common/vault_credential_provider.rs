//! Root-side adapter that exposes the in-memory credential vault to
//! `peko-providers` through the [`peko_provider_api::CredentialProvider`]
//! trait.
//!
//! `peko-providers` cannot take a direct dependency on
//! `crate::common::vault::Vault` (which depends on `peko-identity` for
//! encryption); instead it consumes `Arc<dyn CredentialProvider>`,
//! and the daemon-side composition code wraps the live vault in a
//! [`VaultCredentialProvider`] before handing it to
//! `LlmResolver::with_credential_provider`.
//!
//! Phase 6 of the post-migration cleanup.

use std::sync::Arc;

use secrecy::SecretString;

use peko_provider_api::credentials::{
    CredentialError, CredentialMaterial, CredentialProvider, RotationEntry,
};

use crate::common::vault::Vault;

/// `peko-provider-api::CredentialProvider` implementation backed by
/// the in-memory [`Vault`].
///
/// All three trait methods delegate to the underlying vault; the
/// `Vault` is held through an `Arc` so `VaultCredentialProvider`
/// itself is cheap to clone and to share across `LlmResolver` and
/// `RotationState` instances.
#[derive(Clone)]
pub struct VaultCredentialProvider {
    vault: Arc<Vault>,
}

impl VaultCredentialProvider {
    /// Wrap a live [`Vault`] in the trait adapter.
    #[must_use]
    pub fn new(vault: Arc<Vault>) -> Self {
        Self { vault }
    }

    /// Borrow the underlying vault. Used by tests that need the full
    /// vault API (e.g., to seed a credential or rotation binding
    /// before constructing a `VaultCredentialProvider`).
    #[must_use]
    pub fn vault(&self) -> &Arc<Vault> {
        &self.vault
    }
}

impl CredentialProvider for VaultCredentialProvider {
    fn get_credential(&self, id: &str) -> Result<Option<Arc<CredentialMaterial>>, CredentialError> {
        // `Vault::get_credential` never errors — it returns `None` on
        // poisoned-lock / missing-id. Map the success value and wrap
        // in `Ok` since the trait signature demands `Result<_, _>`.
        Ok(self.vault.get_credential(id).map(|c| {
            Arc::new(CredentialMaterial {
                material: c.material,
            })
        }))
    }

    fn load_rotation_credentials(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Vec<RotationEntry>, CredentialError> {
        self.vault
            .get_rotation_credentials(namespace, name)
            .map(|pairs| {
                pairs
                    .into_iter()
                    .map(|(credential_id, material)| RotationEntry {
                        credential_id,
                        material,
                    })
                    .collect()
            })
            .map_err(|e| CredentialError::Backend(format!("{e:#}")))
    }

    fn record_test(&self, credential_id: &str, ok: bool) {
        if let Err(e) = self.vault.record_test(credential_id, ok) {
            tracing::warn!(
                credential_id,
                ok,
                "failed to record {} test outcome: {e:#}",
                if ok { "ok" } else { "failed" }
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::vault::{Credential, CredentialKind, RotationBinding, RotationStrategy};
    use secrecy::ExposeSecret;

    fn test_vault() -> (tempfile::TempDir, Arc<Vault>) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::for_test(dir.path(), "test-passphrase"));
        (dir, vault)
    }

    #[test]
    fn get_credential_returns_material_for_known_id() {
        let (_dir, vault) = test_vault();
        let cred = Credential::now(
            "provider:openai",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("sk-test".into()),
        );
        vault.set_credential(&cred).unwrap();

        let provider = VaultCredentialProvider::new(vault);
        let material = provider
            .get_credential(&cred.id)
            .unwrap()
            .expect("credential must be present");
        assert_eq!(material.material.expose_secret(), "sk-test");
    }

    #[test]
    fn get_credential_returns_none_for_unknown_id() {
        let (_dir, vault) = test_vault();
        let provider = VaultCredentialProvider::new(vault);
        assert!(provider.get_credential("missing").unwrap().is_none());
    }

    #[test]
    fn load_rotation_credentials_returns_empty_when_no_binding() {
        let (_dir, vault) = test_vault();
        let provider = VaultCredentialProvider::new(vault);
        let entries = provider
            .load_rotation_credentials("provider:nope", "default")
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn load_rotation_credentials_returns_ordered_binding_entries() {
        let (_dir, vault) = test_vault();
        let c1 = Credential::now(
            "provider:openai",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("key-1".into()),
        );
        let c2 = Credential::now(
            "provider:openai",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("key-2".into()),
        );
        vault.set_credential(&c1).unwrap();
        vault.set_credential(&c2).unwrap();
        vault
            .set_binding(
                &RotationBinding::slot_key("provider:openai", "default"),
                &RotationBinding {
                    strategy: RotationStrategy::RoundRobin,
                    ordered_credential_ids: vec![c1.id.clone(), c2.id.clone()],
                },
            )
            .unwrap();

        let provider = VaultCredentialProvider::new(vault);
        let entries = provider
            .load_rotation_credentials("provider:openai", "default")
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].credential_id, c1.id);
        assert_eq!(entries[0].material.expose_secret(), "key-1");
        assert_eq!(entries[1].credential_id, c2.id);
        assert_eq!(entries[1].material.expose_secret(), "key-2");
    }

    #[test]
    fn record_test_swallows_missing_credential_errors() {
        let (_dir, vault) = test_vault();
        let provider = VaultCredentialProvider::new(vault);
        // `record_test` is best-effort; must not panic on unknown ids.
        provider.record_test("missing", true);
    }
}
