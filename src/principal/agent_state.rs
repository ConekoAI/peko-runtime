//! Per-principal agent state registry.
//!
//! After the root-agent unification refactor (PR #94), the daemon-global
//! `ExtensionCore` is shared across all principals. Agent prompt-section
//! hooks are therefore singletons; they resolve per-principal state from
//! this registry at handle time instead of carrying per-principal
//! allowlists in their own instances.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use crate::principal::PrincipalId;

/// Per-principal agent state.
#[derive(Debug, Clone)]
pub struct AgentState {
    /// Enabled agent names (case-insensitive). Stored normalized to
    /// lowercase.
    pub allowlist: HashSet<String>,
}

impl AgentState {
    /// Build an `AgentState` from the raw allowed extension list.
    pub fn new(allowlist: Vec<String>) -> Self {
        let allowlist = allowlist
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        Self { allowlist }
    }

    /// Case-insensitive membership check.
    pub fn is_enabled(&self, agent_name: &str) -> bool {
        self.allowlist.contains(&agent_name.to_ascii_lowercase())
    }
}

/// Process-wide registry mapping each principal to its agent state.
#[derive(Debug, Default)]
pub struct AgentStateRegistry {
    states: Mutex<std::collections::HashMap<PrincipalId, Arc<AgentState>>>,
}

impl AgentStateRegistry {
    /// Get the global singleton registry.
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<AgentStateRegistry> = OnceLock::new();
        REGISTRY.get_or_init(AgentStateRegistry::default)
    }

    /// Register (or overwrite) a principal's agent state.
    pub async fn register(&self, principal_id: PrincipalId, state: AgentState) {
        let mut states = self
            .states
            .lock()
            .expect("AgentStateRegistry mutex poisoned");
        states.insert(principal_id, Arc::new(state));
    }

    /// Remove a principal's agent state. Idempotent.
    pub async fn unregister(&self, principal_id: &PrincipalId) {
        self.unregister_sync(principal_id);
    }

    /// Synchronous removal used by the RAII guard so cleanup runs even
    /// when `drop` is invoked without a current Tokio runtime.
    fn unregister_sync(&self, principal_id: &PrincipalId) {
        let mut states = self
            .states
            .lock()
            .expect("AgentStateRegistry mutex poisoned");
        states.remove(principal_id);
    }

    /// Get a copy of the principal's agent state, if any.
    pub async fn get(&self, principal_id: &PrincipalId) -> Option<Arc<AgentState>> {
        let states = self
            .states
            .lock()
            .expect("AgentStateRegistry mutex poisoned");
        states.get(principal_id).cloned()
    }

    /// Check whether an agent is enabled for a principal.
    ///
    /// `None` principal is treated as fail-closed: no agent access.
    pub async fn is_agent_enabled(
        &self,
        principal_id: Option<&PrincipalId>,
        agent_name: &str,
    ) -> bool {
        let Some(pid) = principal_id else {
            return false;
        };
        let Some(state) = self.get(pid).await else {
            return false;
        };
        state.is_enabled(agent_name)
    }
}

/// RAII guard that unregisters a principal's agent state on drop.
///
/// Used by the runner so a principal's `AgentState` is cleaned up even
/// if the agentic loop panics or returns early. Cleanup is synchronous so
/// it runs even when `drop` is invoked without a current Tokio runtime.
pub struct AgentStateGuard {
    principal_id: PrincipalId,
}

impl AgentStateGuard {
    /// Create a guard for the given principal.
    #[must_use]
    pub fn new(principal_id: PrincipalId) -> Self {
        Self { principal_id }
    }
}

impl Drop for AgentStateGuard {
    fn drop(&mut self) {
        AgentStateRegistry::global().unregister_sync(&self.principal_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(id: &str) -> PrincipalId {
        PrincipalId(id.to_string())
    }

    #[tokio::test]
    async fn register_get_unregister_round_trip() {
        let registry = AgentStateRegistry::default();
        let state = AgentState::new(vec!["researcher".to_string()]);

        registry.register(pid("p1"), state).await;
        let got = registry.get(&pid("p1")).await.expect("registered state");
        assert!(got.is_enabled("researcher"));

        registry.unregister(&pid("p1")).await;
        assert!(registry.get(&pid("p1")).await.is_none());
    }

    #[tokio::test]
    async fn is_agent_enabled_is_case_insensitive() {
        let registry = AgentStateRegistry::default();
        let state = AgentState::new(vec!["Researcher".to_string()]);
        registry.register(pid("p1"), state).await;

        assert!(
            registry
                .is_agent_enabled(Some(&pid("p1")), "researcher")
                .await
        );
        assert!(
            registry
                .is_agent_enabled(Some(&pid("p1")), "RESEARCHER")
                .await
        );
        assert!(!registry.is_agent_enabled(Some(&pid("p1")), "writer").await);
    }

    #[tokio::test]
    async fn unregister_on_absent_is_noop() {
        let registry = AgentStateRegistry::default();
        registry.unregister(&pid("missing")).await;
        assert!(registry.get(&pid("missing")).await.is_none());
    }

    #[tokio::test]
    async fn no_principal_is_fail_closed() {
        let registry = AgentStateRegistry::default();
        assert!(!registry.is_agent_enabled(None, "researcher").await);
    }

    #[tokio::test]
    async fn unknown_principal_is_fail_closed() {
        let registry = AgentStateRegistry::default();
        assert!(
            !registry
                .is_agent_enabled(Some(&pid("unknown")), "researcher")
                .await
        );
    }
}
