//! Per-principal skill state registry.
//!
//! After the root-agent unification refactor (PR #94), the daemon-global
//! `ExtensionCore` is shared across all principals. The `Skill` tool and
//! its prompt-section hooks are therefore singletons; they resolve per-
//! principal state from this registry at handle time instead of carrying
//! per-principal allowlists in their own instances.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

use crate::principal::PrincipalId;

/// Per-principal skill state.
#[derive(Debug, Clone)]
pub struct SkillState {
    /// Enabled skill names (case-insensitive). Stored normalized to
    /// lowercase.
    pub allowlist: HashSet<String>,
    /// Principal workspace root. Used as the cwd for `` !`cmd` `` / `` ```! ``
    /// blocks inside skill bodies.
    pub workspace: PathBuf,
}

impl SkillState {
    /// Build a `SkillState` from the raw capability list.
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
    pub fn is_enabled(&self, skill_name: &str) -> bool {
        self.allowlist.contains(&skill_name.to_ascii_lowercase())
    }
}

/// Process-wide registry mapping each principal to its skill state.
#[derive(Debug, Default)]
pub struct SkillStateRegistry {
    states: RwLock<std::collections::HashMap<PrincipalId, Arc<SkillState>>>,
}

impl SkillStateRegistry {
    /// Get the global singleton registry.
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<SkillStateRegistry> = OnceLock::new();
        REGISTRY.get_or_init(SkillStateRegistry::default)
    }

    /// Register (or overwrite) a principal's skill state.
    pub async fn register(&self, principal_id: PrincipalId, state: SkillState) {
        let mut states = self.states.write().await;
        states.insert(principal_id, Arc::new(state));
    }

    /// Remove a principal's skill state. Idempotent.
    pub async fn unregister(&self, principal_id: &PrincipalId) {
        let mut states = self.states.write().await;
        states.remove(principal_id);
    }

    /// Get a copy of the principal's skill state, if any.
    pub async fn get(&self, principal_id: &PrincipalId) -> Option<Arc<SkillState>> {
        let states = self.states.read().await;
        states.get(principal_id).cloned()
    }

    /// Check whether a skill is enabled for a principal.
    ///
    /// `None` principal is treated as fail-closed: no skill access.
    pub async fn is_skill_enabled(
        &self,
        principal_id: Option<&PrincipalId>,
        skill_name: &str,
    ) -> bool {
        let Some(pid) = principal_id else {
            return false;
        };
        let Some(state) = self.get(pid).await else {
            return false;
        };
        state.is_enabled(skill_name)
    }
}

/// RAII guard that unregisters a principal's skill state on drop.
///
/// Used by the runner so a principal's `SkillState` is cleaned up even
/// if the agentic loop panics or returns early.
pub struct SkillStateGuard {
    principal_id: PrincipalId,
}

impl SkillStateGuard {
    /// Create a guard for the given principal.
    #[must_use]
    pub fn new(principal_id: PrincipalId) -> Self {
        Self { principal_id }
    }
}

impl Drop for SkillStateGuard {
    fn drop(&mut self) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let principal_id = self.principal_id.clone();
            handle.spawn(async move {
                SkillStateRegistry::global().unregister(&principal_id).await;
            });
        }
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
        let registry = SkillStateRegistry::default();
        let state = SkillState::new(vec!["docker".to_string()], PathBuf::from("/ws"));

        registry.register(pid("p1"), state).await;
        let got = registry.get(&pid("p1")).await.expect("registered state");
        assert!(got.is_enabled("docker"));

        registry.unregister(&pid("p1")).await;
        assert!(registry.get(&pid("p1")).await.is_none());
    }

    #[tokio::test]
    async fn is_skill_enabled_is_case_insensitive() {
        let registry = SkillStateRegistry::default();
        let state = SkillState::new(vec!["Docker".to_string()], PathBuf::from("/ws"));
        registry.register(pid("p1"), state).await;

        assert!(registry.is_skill_enabled(Some(&pid("p1")), "docker").await);
        assert!(registry.is_skill_enabled(Some(&pid("p1")), "DOCKER").await);
        assert!(!registry.is_skill_enabled(Some(&pid("p1")), "deploy").await);
    }

    #[tokio::test]
    async fn unregister_on_absent_is_noop() {
        let registry = SkillStateRegistry::default();
        registry.unregister(&pid("missing")).await;
        assert!(registry.get(&pid("missing")).await.is_none());
    }

    #[tokio::test]
    async fn no_principal_is_fail_closed() {
        let registry = SkillStateRegistry::default();
        assert!(!registry.is_skill_enabled(None, "docker").await);
    }

    #[tokio::test]
    async fn unknown_principal_is_fail_closed() {
        let registry = SkillStateRegistry::default();
        assert!(!registry.is_skill_enabled(Some(&pid("unknown")), "docker").await);
    }
}
