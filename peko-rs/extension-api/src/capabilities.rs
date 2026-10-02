//! Capability model — `Capabilities` data shell.
//!
//! ADR-066 D1 deleted the capability **gate**: presence = visibility =
//! executability, and no `tool:*` / `role:*` / `skill:*` / `agent:*` /
//! `principal:write_*` grant check fires anywhere in the runtime. The
//! evaluation surface (`is_granted`, `Capability::matches`,
//! `Capabilities::starter_bundle`) is gone.
//!
//! What remains is a plain data shell — a `Vec` of grant strings — kept
//! only because wire and on-disk shapes still carry it:
//!
//! - `principal.toml`'s legacy `[capabilities] grants = [...]` section
//!   parses into this type (ignored on load; never persisted — see
//!   `peko_core::principal::config`).
//! - IPC DTOs such as `PrincipalSummary.capabilities` serialize it.
//!
//! New code must not read grant strings for authorization. The type
//! folds away entirely in ADR-066 P6.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A capability grant string. Pure data — no matching/evaluation
/// semantics remain (ADR-066 D1).
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

/// A capability grant set. Pure data — the load/evaluate path was
/// deleted in ADR-066 P2; this type only round-trips grant strings on
/// wire/serde shapes that still carry them.
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
}
