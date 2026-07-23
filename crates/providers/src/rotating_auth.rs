//! Authentication rotation for LLM providers.
//!
//! `RotationState` is attached to a [`Provider`](crate::core::Provider)
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

use peko_provider_api::credentials::CredentialProvider;

/// Format a rotation slot key from a `(namespace, name)` pair.
///
/// Mirrors the legacy `RotationBinding::slot_key` helper that lived
/// on the root vault; reproduced here as a free function so
/// `peko-providers` does not depend on the root vault type.
fn rotation_slot_key(namespace: &str, name: &str) -> String {
    format!("{namespace}:{name}")
}

/// Snapshot of a rotation binding plus the current position.
///
/// `current_index` is shared across cloned `Provider`s so that a retry
/// built via `Provider::rebuild_with_material` continues advancing the
/// same logical cursor.
#[derive(Clone)]
pub struct RotationState {
    credentials: Arc<dyn CredentialProvider>,
    namespace: String,
    name: String,
    /// Ordered credential ids + materials from the binding.
    credentials_list: Vec<(String, SecretString)>,
    /// Shared cursor into `credentials_list`.
    current_index: Arc<AtomicUsize>,
}

impl RotationState {
    /// Build a rotation state from the vault binding for `(namespace, name)`.
    ///
    /// Errors if no binding exists or if the binding references no
    /// resolvable credentials.
    pub fn new(
        credentials: Arc<dyn CredentialProvider>,
        namespace: String,
        name: String,
    ) -> Result<Self> {
        let credentials_list = credentials
            .load_rotation_credentials(&namespace, &name)
            .with_context(|| format!("failed to load rotation credentials for {namespace}:{name}"))?
            .into_iter()
            .map(|entry| (entry.credential_id, entry.material))
            .collect::<Vec<_>>();
        if credentials_list.is_empty() {
            anyhow::bail!("rotation binding for {namespace}:{name} references no credentials");
        }
        Ok(Self {
            credentials,
            namespace,
            name,
            credentials_list,
            current_index: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// The `(namespace, name)` slot this state rotates through.
    #[must_use]
    pub fn slot(&self) -> String {
        rotation_slot_key(&self.namespace, &self.name)
    }

    /// Number of credentials in the rotation.
    #[must_use]
    pub fn len(&self) -> usize {
        self.credentials_list.len()
    }

    /// Whether the rotation has any credentials.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.credentials_list.is_empty()
    }

    /// Current cursor position into `credentials_list`.
    #[must_use]
    pub fn current_index(&self) -> usize {
        self.current_index.load(Ordering::SeqCst)
    }

    /// Material for the current position.
    #[must_use]
    pub fn current_material(&self) -> Option<&SecretString> {
        let idx = self.current_index.load(Ordering::SeqCst);
        self.credentials_list.get(idx).map(|(_, m)| m)
    }

    /// Advance to the next credential and return its material.
    ///
    /// Wraps around at the end of the list so the next 401 after
    /// exhaustion tries from the top.
    pub fn advance(&self) -> Option<&SecretString> {
        let idx = self.current_index.load(Ordering::SeqCst);
        let next = (idx + 1) % self.credentials_list.len();
        self.current_index.store(next, Ordering::SeqCst);
        Some(&self.credentials_list[next].1)
    }

    /// Record a test outcome against the credential at the current index.
    pub fn record_current_test(&self, ok: bool) {
        let idx = self.current_index.load(Ordering::SeqCst);
        if let Some((id, _)) = self.credentials_list.get(idx) {
            self.credentials.record_test(id, ok);
        }
    }
}

/// Returns `true` if `e` represents an HTTP 401 auth failure.
pub fn is_auth_failure(e: &anyhow::Error) -> bool {
    crate::transport::RetryableError::http_status(e) == Some(401)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_provider_api::credentials::{CredentialMaterial, RotationEntry};
    use secrecy::ExposeSecret;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory `CredentialProvider` used by the rotation tests so
    /// the suite stays self-contained (no root-only vault import).
    #[derive(Default)]
    struct TestCredentialProvider {
        by_id: Mutex<HashMap<String, SecretString>>,
        bindings: Mutex<HashMap<String, Vec<String>>>,
    }

    impl CredentialProvider for TestCredentialProvider {
        fn get_credential(
            &self,
            id: &str,
        ) -> Result<Option<Arc<CredentialMaterial>>, peko_provider_api::credentials::CredentialError>
        {
            Ok(self.by_id.lock().unwrap().get(id).map(|material| {
                Arc::new(CredentialMaterial {
                    material: material.clone(),
                })
            }))
        }

        fn load_rotation_credentials(
            &self,
            namespace: &str,
            name: &str,
        ) -> Result<Vec<RotationEntry>, peko_provider_api::credentials::CredentialError> {
            let slot = rotation_slot_key(namespace, name);
            let bindings = self.bindings.lock().unwrap();
            let Some(order) = bindings.get(&slot) else {
                return Ok(Vec::new());
            };
            let by_id = self.by_id.lock().unwrap();
            Ok(order
                .iter()
                .filter_map(|id| {
                    by_id.get(id).map(|material| RotationEntry {
                        credential_id: id.clone(),
                        material: material.clone(),
                    })
                })
                .collect())
        }

        fn record_test(&self, _credential_id: &str, _ok: bool) {}
    }

    fn test_provider_with_binding() -> (Arc<dyn CredentialProvider>, Vec<String>) {
        let provider = TestCredentialProvider::default();
        {
            let mut by_id = provider.by_id.lock().unwrap();
            by_id.insert("cred-1".into(), SecretString::new("key-1".into()));
            by_id.insert("cred-2".into(), SecretString::new("key-2".into()));
        }
        let ids = vec!["cred-1".into(), "cred-2".into()];
        {
            let mut bindings = provider.bindings.lock().unwrap();
            bindings.insert(rotation_slot_key("provider:mock", "default"), ids.clone());
        }
        let provider: Arc<dyn CredentialProvider> = Arc::new(provider);
        (provider, ids)
    }

    #[test]
    fn new_loads_binding_credentials_in_order() {
        let (provider, _) = test_provider_with_binding();
        let state = RotationState::new(provider, "provider:mock".into(), "default".into()).unwrap();
        assert_eq!(state.len(), 2);
        assert_eq!(state.current_material().unwrap().expose_secret(), "key-1");
    }

    #[test]
    fn advance_rotates_and_wraps() {
        let (provider, _) = test_provider_with_binding();
        let state = RotationState::new(provider, "provider:mock".into(), "default".into()).unwrap();
        state.advance();
        assert_eq!(state.current_material().unwrap().expose_secret(), "key-2");
        state.advance();
        assert_eq!(state.current_material().unwrap().expose_secret(), "key-1");
    }

    #[test]
    fn new_errors_when_binding_missing() {
        let provider: Arc<dyn CredentialProvider> = Arc::new(TestCredentialProvider::default());
        assert!(RotationState::new(provider, "provider:nope".into(), "default".into()).is_err());
    }

    #[test]
    fn is_auth_failure_detects_401_message() {
        let e = anyhow::anyhow!("HTTP error 401: invalid credentials");
        assert!(is_auth_failure(&e));
        let e = anyhow::anyhow!("HTTP error 429: rate limited");
        assert!(!is_auth_failure(&e));
    }
}
