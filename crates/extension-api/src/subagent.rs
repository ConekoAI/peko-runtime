//! `SpawnCleanupPolicy` — cleanup policy for spawn overlays.
//!
//! Lives in `peko_extension_api` because it sits on the cross-boundary
//! `SubagentMetadata` payload — both `peko_extension_host` (which
//! produces it) and `peko_tools_builtin::messaging` (which consumes it
//! via `AgentTool`) need to reference the same enum. Keeping it in
//! `peko_extension_api` breaks the cycle that arose when both crates
//! tried to depend on each other.
//!
//! `peko_extension_host::SpawnCleanupPolicy` is preserved as a
//! backwards-compat re-export.

use serde::{Deserialize, Serialize};

/// Cleanup policy for spawn overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SpawnCleanupPolicy {
    /// Keep the spawn session after completion.
    #[default]
    Keep,
    /// Delete the spawn session after completion.
    Delete,
}

impl SpawnCleanupPolicy {
    /// Get the policy as a string.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            SpawnCleanupPolicy::Keep => "keep",
            SpawnCleanupPolicy::Delete => "delete",
        }
    }

    /// Parse from string.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "keep" => Some(SpawnCleanupPolicy::Keep),
            "delete" => Some(SpawnCleanupPolicy::Delete),
            _ => None,
        }
    }

    /// Check whether this policy means persist.
    #[must_use]
    pub const fn should_persist(&self) -> bool {
        matches!(self, SpawnCleanupPolicy::Keep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_roundtrip() {
        assert_eq!(SpawnCleanupPolicy::Keep.as_str(), "keep");
        assert_eq!(SpawnCleanupPolicy::Delete.as_str(), "delete");
        assert_eq!(
            SpawnCleanupPolicy::from_str("keep"),
            Some(SpawnCleanupPolicy::Keep)
        );
        assert_eq!(
            SpawnCleanupPolicy::from_str("KEEP"),
            Some(SpawnCleanupPolicy::Keep)
        );
        assert_eq!(
            SpawnCleanupPolicy::from_str("delete"),
            Some(SpawnCleanupPolicy::Delete)
        );
        assert_eq!(SpawnCleanupPolicy::from_str("unknown"), None);
    }

    #[test]
    fn should_persist() {
        assert!(SpawnCleanupPolicy::Keep.should_persist());
        assert!(!SpawnCleanupPolicy::Delete.should_persist());
    }

    #[test]
    fn default_is_keep() {
        assert_eq!(SpawnCleanupPolicy::default(), SpawnCleanupPolicy::Keep);
    }
}
