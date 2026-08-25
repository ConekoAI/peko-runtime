//! Capability model — ADR-047 §2.5 collapsed surface.
//!
//! Post-Phase 3b, `Capabilities` only carries cross-actor /
//! cross-runtime grants (`principal:*` / `runtime:*`). Filesystem,
//! network, and tunnel authority live on the new `Authority` envelope
//! (see `peko_extension_api::authority`). The legacy `tool:*` /
//! `agent:*` / `skill:*` / `network` / `filesystem.*` / `tunnel:*`
//! grants that the framework used to use for `is_tool_enabled` are
//! gone — workspace tools (Phase 1+2) are principal-owned and
//! visible by default.
//!
//! The grant strings are still typed as `String` so the
//! `principal:*` / `runtime:*` taxonomy can grow without a schema
//! change. Wildcards (`runtime:*`) are matched against the literal
//! grant list at lookup time, matching the previous semantics.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

/// A typed capability grant.
///
/// Post-Phase 3b this is a thin newtype around `String`. `principal:*`
/// and `runtime:*` are the surviving kinds; the `Capability` struct
/// itself stays so the existing `capabilities.is_granted(...)`
/// call sites don't churn through every file in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capability(pub String);

impl Capability {
    /// Create a capability from any string-like value.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the raw capability string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this capability ends in a wildcard (`*`).
    #[must_use]
    pub fn is_wildcard(&self) -> bool {
        self.0.ends_with('*')
    }

    /// Whether this grant satisfies `required`.
    ///
    /// A grant satisfies a requirement when:
    /// - they are identical, or
    /// - the grant ends in `*` and the required capability starts with the
    ///   grant prefix before the wildcard.
    #[must_use]
    pub fn matches(&self, required: &Capability) -> bool {
        let grant = self.as_str();
        let req = required.as_str();

        if grant == req {
            return true;
        }

        if self.is_wildcard() {
            let prefix = &grant[..grant.len() - 1];
            if req.starts_with(prefix) {
                return true;
            }
        }

        false
    }
}

impl<T> From<T> for Capability
where
    T: Into<String>,
{
    fn from(s: T) -> Self {
        Self(s.into())
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A capability grant set.
///
/// This serializes as `[capabilities] grants = [...]` in `principal.toml` and
/// is the single human-editable source of truth for what a Principal is allowed
/// to do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub grants: Vec<Capability>,
}

impl Capabilities {
    /// Create an empty capability set.
    #[must_use]
    pub fn new() -> Self {
        Self { grants: Vec::new() }
    }

    /// Create a capability set from an iterable of string-like values.
    #[must_use]
    pub fn with_grants(grants: impl IntoIterator<Item = impl Into<Capability>>) -> Self {
        Self {
            grants: grants.into_iter().map(Into::into).collect(),
        }
    }

    /// Add a capability grant.
    pub fn push(&mut self, cap: impl Into<Capability>) {
        self.grants.push(cap.into());
    }

    /// Extend with multiple capability grants.
    pub fn extend(&mut self, caps: impl IntoIterator<Item = impl Into<Capability>>) {
        self.grants.extend(caps.into_iter().map(Into::into));
    }

    /// Remove all occurrences of a capability grant.
    pub fn remove(&mut self, cap: &Capability) {
        self.grants.retain(|c| c != cap);
    }

    /// Whether the given exact capability is present.
    #[must_use]
    pub fn contains(&self, cap: &Capability) -> bool {
        self.grants.contains(cap)
    }

    /// Whether the given capability is granted, taking wildcards into account.
    #[must_use]
    pub fn is_granted(&self, required: &Capability) -> bool {
        self.grants.iter().any(|g| g.matches(required))
    }

    /// Whether no grants are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// Number of grants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.grants.len()
    }

    /// Iterate over capability grants.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.grants.iter()
    }

    /// Remove all grants that do not satisfy the predicate.
    pub fn retain(&mut self, mut f: impl FnMut(&Capability) -> bool) {
        self.grants.retain(|c| f(c));
    }

    /// Clone the grants into a `Vec<Capability>`.
    #[must_use]
    pub fn to_vec(&self) -> Vec<Capability> {
        self.grants.clone()
    }

    /// Convert grants to plain strings.
    #[must_use]
    pub fn to_strings(&self) -> Vec<String> {
        self.grants.iter().map(|c| c.to_string()).collect()
    }

    /// Whether the given string grant is present exactly.
    #[must_use]
    pub fn contains_str(&self, grant: &str) -> bool {
        self.grants.iter().any(|c| c.as_str() == grant)
    }

    /// A safe starter bundle for new Principals.
    ///
    /// Post-Phase 3b (ADR-047 §2.5) this only carries the cross-actor /
    /// cross-runtime grants that still live on `Capabilities`:
    /// `principal:write_config`, `principal:write_agents`,
    /// `principal:write_cron`, and `principal:write_identity`.
    ///
    /// The `tool:*` / `agent:*` / `skill:*` / `network` /
    /// `filesystem.*` / `tunnel:*` grants that used to live here
    /// retired when those concerns moved to `Authority`
    /// (filesystem/network/tunnel) or to the per-principal workspace
    /// (tool/agent catalog). Workspace tools are principal-owned
    /// and visible by default; no capability gating is required to
    /// enumerate them in `available_tools`.
    #[must_use]
    pub fn starter_bundle() -> Self {
        Self::with_grants([
            "principal:write_config",
            "principal:write_agents",
            "principal:write_cron",
            // PR 2 (storage review): required by
            // `principal_unpackager::import_identity` so the import
            // path can write the imported DID's identity directory.
            "principal:write_identity",
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let grants = Capabilities::with_grants(["principal:write_config"]);
        assert!(grants.is_granted(&Capability::new("principal:write_config")));
        assert!(!grants.is_granted(&Capability::new("principal:write_agents")));
    }

    #[test]
    fn wildcard_match() {
        let grants = Capabilities::with_grants(["runtime:*"]);
        assert!(grants.is_granted(&Capability::new("runtime:trust")));
        assert!(grants.is_granted(&Capability::new("runtime:write_extensions")));
        assert!(!grants.is_granted(&Capability::new("principal:write_config")));
    }

    /// Post-Phase 3b the starter bundle carries only the cross-actor /
    /// cross-runtime grants. Workspace tools are principal-owned and
    /// visible by default — no per-tool grant is needed.
    #[test]
    fn starter_bundle_carries_only_principal_and_runtime_grants() {
        let caps = Capabilities::starter_bundle();
        for required in [
            "principal:write_config",
            "principal:write_agents",
            "principal:write_cron",
            "principal:write_identity",
        ] {
            assert!(
                caps.is_granted(&Capability::new(required)),
                "starter_bundle must carry {required}"
            );
        }
        for retired in [
            "tool:Read",
            "tool:Write",
            "tool:Edit",
            "tool:Bash",
            "tool:Agent",
            "agent:researcher",
            "skill:unknown",
            "network",
            "tunnel:create",
            "filesystem.read:/etc",
        ] {
            assert!(
                !caps.is_granted(&Capability::new(retired)),
                "starter_bundle must NOT carry {retired} (Phase 3b retired the kind)"
            );
        }
    }
}

/// The set of extension IDs that are active for a Principal under a given
/// capability snapshot.
///
/// An extension is active when it is detected/installed, at least one of its
/// provided capabilities is granted, and all of its `requires` capabilities
/// are satisfied. The active set is computed once per message and threaded
/// through tool execution so the runtime can verify that the owning extension
/// is active before invoking a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveExtensionSet {
    ids: HashSet<String>,
}

impl ActiveExtensionSet {
    /// Create an empty active set.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            ids: HashSet::new(),
        }
    }

    /// Create an active set from an iterable of extension IDs.
    #[must_use]
    pub fn with_ids(ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            ids: ids.into_iter().map(Into::into).collect(),
        }
    }

    /// Insert an extension ID into the active set.
    pub fn insert(&mut self, id: impl Into<String>) {
        self.ids.insert(id.into());
    }

    /// Whether the given extension ID is active.
    #[must_use]
    pub fn is_active(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Iterate over active extension IDs.
    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.ids.iter()
    }

    /// Convert the active set to a sorted vector of strings.
    #[must_use]
    pub fn to_vec(&self) -> Vec<String> {
        let mut v: Vec<String> = self.ids.iter().cloned().collect();
        v.sort();
        v
    }

    /// Whether the active set contains no IDs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[cfg(test)]
mod active_set_tests {
    use super::*;

    #[test]
    fn empty_set_is_inactive() {
        let set = ActiveExtensionSet::empty();
        assert!(!set.is_active("builtin:tool:Read"));
    }

    #[test]
    fn inserted_id_is_active() {
        let mut set = ActiveExtensionSet::empty();
        set.insert("builtin:tool:Read");
        assert!(set.is_active("builtin:tool:Read"));
    }
}
