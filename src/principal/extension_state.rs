//! Per-principal extension state registry.
//!
//! After the root-agent unification refactor (PR #94), the daemon-global
//! `ExtensionCore` is shared across all principals. The `Skill` tool and
//! its prompt-section hooks are therefore singletons; they resolve per-
//! principal state from this extension registry at handle time instead of carrying
//! per-principal allowlists in their own instances.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::principal::PrincipalId;

/// Per-principal extension state.
#[derive(Debug, Clone)]
pub struct ExtensionState {
    /// Enabled extension ids/names (case-insensitive). Stored normalized to
    /// lowercase.
    pub allowlist: HashSet<String>,
    /// Principal workspace root. Used as the cwd for `` !`cmd` `` / `` ```! ``
    /// blocks inside extension/skill bodies.
    pub workspace: PathBuf,
}

impl ExtensionState {
    /// Build an `ExtensionState` from the raw allowed extension list.
    pub fn new(allowlist: Vec<String>, workspace: PathBuf) -> Self {
        let allowlist = allowlist
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        Self {
            allowlist,
            workspace,
        }
    }

    /// Case-insensitive membership check.
    pub fn is_enabled(&self, extension_name: &str) -> bool {
        self.allowlist.contains(&extension_name.to_ascii_lowercase())
    }
}

/// Process-wide registry mapping each principal to its extension state.
#[derive(Debug, Default)]
pub struct ExtensionStateRegistry {
    states: Mutex<std::collections::HashMap<PrincipalId, Arc<ExtensionState>>>,
}

impl ExtensionStateRegistry {
    /// Get the global singleton registry.
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<ExtensionStateRegistry> = OnceLock::new();
        REGISTRY.get_or_init(ExtensionStateRegistry::default)
    }

    /// Register (or overwrite) a principal's extension state.
    pub async fn register(&self, principal_id: PrincipalId, state: ExtensionState) {
        let mut states = self.states.lock().expect("ExtensionStateRegistry mutex poisoned");
        states.insert(principal_id, Arc::new(state));
    }

    /// Remove a principal's extension state. Idempotent.
    pub async fn unregister(&self, principal_id: &PrincipalId) {
        self.unregister_sync(principal_id);
    }

    /// Synchronous removal used by the RAII guard so cleanup runs even
    /// when `drop` is invoked without a current Tokio runtime.
    fn unregister_sync(&self, principal_id: &PrincipalId) {
        let mut states = self.states.lock().expect("ExtensionStateRegistry mutex poisoned");
        states.remove(principal_id);
    }

    /// Get a copy of the principal's extension state, if any.
    pub async fn get(&self, principal_id: &PrincipalId) -> Option<Arc<ExtensionState>> {
        let states = self
            .states
            .lock()
            .expect("ExtensionStateRegistry mutex poisoned");
        states.get(principal_id).cloned()
    }

    /// Check whether an extension is enabled for a principal.
    ///
    /// `None` principal is treated as fail-closed: no extension access.
    pub async fn is_extension_enabled(
        &self,
        principal_id: Option<&PrincipalId>,
        extension_name: &str,
    ) -> bool {
        let Some(pid) = principal_id else {
            return false;
        };
        let Some(state) = self.get(pid).await else {
            return false;
        };
        state.is_enabled(extension_name)
    }
}

/// RAII guard that unregisters a principal's extension state on drop.
///
/// Used by the runner so a principal's `ExtensionState` is cleaned up even
/// if the agentic loop panics or returns early. Cleanup is synchronous so
/// it runs even when `drop` is invoked without a current Tokio runtime.
pub struct ExtensionStateGuard {
    principal_id: PrincipalId,
}

impl ExtensionStateGuard {
    /// Create a guard for the given principal.
    #[must_use]
    pub fn new(principal_id: PrincipalId) -> Self {
        Self { principal_id }
    }
}

impl Drop for ExtensionStateGuard {
    fn drop(&mut self) {
        ExtensionStateRegistry::global().unregister_sync(&self.principal_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pid(id: &str) -> PrincipalId {
        PrincipalId(id.to_string())
    }

    #[tokio::test]
    async fn register_get_unregister_round_trip() {
        let registry = ExtensionStateRegistry::default();
        let state = ExtensionState::new(vec!["docker".to_string()], PathBuf::from("/ws"));

        registry.register(pid("p1"), state).await;
        let got = registry.get(&pid("p1")).await.expect("registered state");
        assert!(got.is_enabled("docker"));

        registry.unregister(&pid("p1")).await;
        assert!(registry.get(&pid("p1")).await.is_none());
    }

    #[tokio::test]
    async fn is_extension_enabled_is_case_insensitive() {
        let registry = ExtensionStateRegistry::default();
        let state = ExtensionState::new(vec!["Docker".to_string()], PathBuf::from("/ws"));
        registry.register(pid("p1"), state).await;

        assert!(registry.is_extension_enabled(Some(&pid("p1")), "docker").await);
        assert!(registry.is_extension_enabled(Some(&pid("p1")), "DOCKER").await);
        assert!(!registry.is_extension_enabled(Some(&pid("p1")), "deploy").await);
    }

    #[tokio::test]
    async fn unregister_on_absent_is_noop() {
        let registry = ExtensionStateRegistry::default();
        registry.unregister(&pid("missing")).await;
        assert!(registry.get(&pid("missing")).await.is_none());
    }

    #[tokio::test]
    async fn no_principal_is_fail_closed() {
        let registry = ExtensionStateRegistry::default();
        assert!(!registry.is_extension_enabled(None, "docker").await);
    }

    #[tokio::test]
    async fn unknown_principal_is_fail_closed() {
        let registry = ExtensionStateRegistry::default();
        assert!(
            !registry
                .is_extension_enabled(Some(&pid("unknown")), "docker")
                .await
        );
    }
}
