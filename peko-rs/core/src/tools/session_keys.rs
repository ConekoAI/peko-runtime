//! `SessionKeys` — per-agent session-key side table (ADR-066 D2).
//!
//! The agent sets its key before each run so tools that need a
//! `parent_session_key` (e.g. `AsyncSpawn`) read the *correct* agent's
//! key. Keyed by agent DID: a single shared table serves every agent in
//! the daemon; per-agent keys prevent concurrent agents from
//! overwriting each other's session key (the bug addressed in
//! issue #68).

use std::collections::HashMap;
use std::sync::Arc;

/// Per-agent session-key table. Cheap to clone (shares the map).
#[derive(Debug, Clone, Default)]
pub struct SessionKeys {
    inner: Arc<std::sync::RwLock<HashMap<String, String>>>,
}

impl SessionKeys {
    /// Create an empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or clear, with `None`) the session key for `agent_id`.
    pub fn set(&self, agent_id: &str, key: Option<String>) {
        let mut guard = self.inner.write().expect("session keys lock poisoned");
        match key {
            Some(k) => {
                guard.insert(agent_id.to_string(), k);
            }
            None => {
                guard.remove(agent_id);
            }
        }
    }

    /// The current session key for `agent_id`, if set.
    #[must_use]
    pub fn get(&self, agent_id: &str) -> Option<String> {
        self.inner
            .read()
            .expect("session keys lock poisoned")
            .get(agent_id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_agent_isolation() {
        let keys = SessionKeys::new();
        keys.set("a", Some("sess-a".to_string()));
        keys.set("b", Some("sess-b".to_string()));
        assert_eq!(keys.get("a").as_deref(), Some("sess-a"));
        assert_eq!(keys.get("b").as_deref(), Some("sess-b"));
        keys.set("a", None);
        assert_eq!(keys.get("a"), None);
        assert_eq!(keys.get("b").as_deref(), Some("sess-b"));
    }
}
