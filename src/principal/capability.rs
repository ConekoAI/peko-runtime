//! Capability model for Principal authority.
//!
//! A capability is a typed grant such as `tool:Read`, `agent:researcher`,
//! `skill:github_skill`, `filesystem.read:/path`, or `network`.  A Principal's
//! `[capabilities] grants = [...]` array in `principal.toml` is the single
//! source of truth for what the Principal is allowed to do.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

/// A typed capability grant.
///
/// Capabilities are stored as opaque strings so the taxonomy can grow without
/// changing the core type.  Convenience methods parse the `kind` prefix and
/// wildcard suffix for matching.
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

    /// The capability kind, i.e. the part before the first `:`.
    ///
    /// For bare capabilities such as `network` the whole string is returned.
    #[must_use]
    pub fn kind(&self) -> &str {
        self.0.split_once(':').map(|(k, _)| k).unwrap_or(&self.0)
    }

    /// The capability value, i.e. the part after the first `:`.
    ///
    /// For bare capabilities such as `network` an empty string is returned.
    #[must_use]
    pub fn value(&self) -> &str {
        self.0.split_once(':').map(|(_, v)| v).unwrap_or("")
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

/// A Principal's capability grants.
///
/// This is the single human-editable source of truth for what a Principal is
/// allowed to do.  It serializes as `[capabilities] grants = [...]` in
/// `principal.toml`.
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
    #[must_use]
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
    /// This grants the built-in tools and agents needed for basic operation
    /// without handing over unrestricted authority.
    #[must_use]
    pub fn starter_bundle() -> Self {
        Self::with_grants([
            "tool:Read",
            "tool:Write",
            "tool:Edit",
            "tool:Bash",
            "tool:Agent",
            "agent:*",
            "tool:agent_catalog",
            "tool:TaskCreate",
            "tool:TaskList",
            "tool:TaskGet",
            "tool:TaskUpdate",
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_parses_kind_and_value() {
        let cap = Capability::new("tool:Read");
        assert_eq!(cap.kind(), "tool");
        assert_eq!(cap.value(), "Read");

        let bare = Capability::new("network");
        assert_eq!(bare.kind(), "network");
        assert_eq!(bare.value(), "");
    }

    #[test]
    fn exact_match() {
        let grants = Capabilities::with_grants(["tool:Read", "agent:researcher"]);
        assert!(grants.is_granted(&Capability::new("tool:Read")));
        assert!(!grants.is_granted(&Capability::new("tool:Write")));
    }

    #[test]
    fn wildcard_match() {
        let grants = Capabilities::with_grants(["tool:*", "agent:agency-agents/*"]);
        assert!(grants.is_granted(&Capability::new("tool:Read")));
        assert!(grants.is_granted(&Capability::new("tool:Write")));
        assert!(grants.is_granted(&Capability::new("agent:agency-agents/researcher")));
        assert!(!grants.is_granted(&Capability::new("agent:other/researcher")));
    }

    #[test]
    fn starter_bundle_includes_builtins() {
        let caps = Capabilities::starter_bundle();
        assert!(caps.is_granted(&Capability::new("tool:Read")));
        assert!(caps.is_granted(&Capability::new("agent:researcher")));
        assert!(!caps.is_granted(&Capability::new("skill:unknown")));
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
    #[must_use]
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
