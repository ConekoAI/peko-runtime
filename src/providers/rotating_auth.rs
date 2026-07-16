//! Authentication rotation for LLM providers.
//!
//! `RotationState` is attached to a [`Provider`](crate::providers::core::Provider)
//! by `LlmResolver::build_provider` when the vault contains a rotation binding
//! for `provider:{id}:default`. On a 401 response, the provider advances to the
//! next credential in the binding, rebuilds its HTTP client with the new
//! material, and retries the request.
//!
//! The rotation logic lives here; `Provider` itself keeps the same public
//! surface so callers (the agentic loop, metered wrappers, compaction, etc.)
//! do not need to change.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use secrecy::SecretString;

use crate::common::vault::{RotationBinding, Vault};

/// Snapshot of a rotation binding plus the current position.
///
/// `current_index` is shared across cloned `Provider`s so that a retry
/// built via `Provider::rebuild_with_material` continues advancing the
/// same logical cursor.
#[derive(Clone)]
pub struct RotationState {
    vault: Arc<Vault>,
    namespace: String,
    name: String,
    /// Ordered credential ids + materials from the binding.
    credentials: Vec<(String, SecretString)>,
    /// Shared cursor into `credentials`.
    current_index: Arc<AtomicUsize>,
}

impl RotationState {
    /// Build a rotation state from the vault binding for `(namespace, name)`.
    ///
    /// Errors if no binding exists or if the binding references no
    /// resolvable credentials.
    pub fn new(vault: Arc<Vault>, namespace: String, name: String) -> Result<Self> {
        let credentials = vault
            .get_rotation_credentials(&namespace, &name)
            .with_context(|| {
                format!("failed to load rotation credentials for {namespace}:{name}")
            })?;
        if credentials.is_empty() {
            anyhow::bail!("rotation binding for {namespace}:{name} references no credentials");
        }
        Ok(Self {
            vault,
            namespace,
            name,
            credentials,
            current_index: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// The `(namespace, name)` slot this state rotates through.
    #[must_use]
    pub fn slot(&self) -> String {
        RotationBinding::slot_key(&self.namespace, &self.name)
    }

    /// Number of credentials in the rotation.
    #[must_use]
    pub fn len(&self) -> usize {
        self.credentials.len()
    }

    /// Whether the rotation has any credentials.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.credentials.is_empty()
    }

    /// Current cursor position into `credentials`.
    #[must_use]
    pub fn current_index(&self) -> usize {
        self.current_index.load(Ordering::SeqCst)
    }

    /// Material for the current position.
    #[must_use]
    pub fn current_material(&self) -> Option<&SecretString> {
        let idx = self.current_index.load(Ordering::SeqCst);
        self.credentials.get(idx).map(|(_, m)| m)
    }

    /// Advance to the next credential and return its material.
    ///
    /// Wraps around at the end of the list so the next 401 after
    /// exhaustion tries from the top.
    pub fn advance(&self) -> Option<&SecretString> {
        let idx = self.current_index.load(Ordering::SeqCst);
        let next = (idx + 1) % self.credentials.len();
        self.current_index.store(next, Ordering::SeqCst);
        Some(&self.credentials[next].1)
    }

    /// Record a test outcome against the credential at the current index.
    pub fn record_current_test(&self, ok: bool) {
        let idx = self.current_index.load(Ordering::SeqCst);
        if let Some((id, _)) = self.credentials.get(idx) {
            if let Err(e) = self.vault.record_test(id, ok) {
                tracing::warn!(
                    "failed to record {} test outcome for credential {}: {e}",
                    if ok { "ok" } else { "failed" },
                    id
                );
            }
        }
    }
}

/// Returns `true` if `e` represents an HTTP 401 auth failure.
pub fn is_auth_failure(e: &anyhow::Error) -> bool {
    crate::providers::transport::RetryableError::http_status(e) == Some(401)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::vault::{Credential, CredentialKind, RotationBinding, RotationStrategy};
    use secrecy::ExposeSecret;

    fn test_vault_with_binding() -> (tempfile::TempDir, Arc<Vault>) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::for_test(dir.path(), "rotation-test"));

        let c1 = Credential::now(
            "provider:mock",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("key-1".into()),
        );
        let c2 = Credential::now(
            "provider:mock",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("key-2".into()),
        );
        let id1 = c1.id.clone();
        let id2 = c2.id.clone();
        vault.set_credential(&c1).unwrap();
        vault.set_credential(&c2).unwrap();

        vault
            .set_binding(
                &RotationBinding::slot_key("provider:mock", "default"),
                &RotationBinding {
                    strategy: RotationStrategy::RoundRobin,
                    ordered_credential_ids: vec![id1, id2],
                },
            )
            .unwrap();

        (dir, vault)
    }

    #[test]
    fn new_loads_binding_credentials_in_order() {
        let (_dir, vault) = test_vault_with_binding();
        let state = RotationState::new(vault, "provider:mock".into(), "default".into()).unwrap();
        assert_eq!(state.len(), 2);
        assert_eq!(state.current_material().unwrap().expose_secret(), "key-1");
    }

    #[test]
    fn advance_rotates_and_wraps() {
        let (_dir, vault) = test_vault_with_binding();
        let state = RotationState::new(vault, "provider:mock".into(), "default".into()).unwrap();
        state.advance();
        assert_eq!(state.current_material().unwrap().expose_secret(), "key-2");
        state.advance();
        assert_eq!(state.current_material().unwrap().expose_secret(), "key-1");
    }

    #[test]
    fn new_errors_when_binding_missing() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::for_test(dir.path(), "rotation-test"));
        assert!(RotationState::new(vault, "provider:nope".into(), "default".into()).is_err());
    }

    #[test]
    fn is_auth_failure_detects_401_message() {
        let e = anyhow::anyhow!("HTTP error 401: invalid credentials");
        assert!(is_auth_failure(&e));
        let e = anyhow::anyhow!("HTTP error 429: rate limited");
        assert!(!is_auth_failure(&e));
    }
}
