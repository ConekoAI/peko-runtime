//! Capability model — `Capabilities` contract.
//!
//! # What `Capabilities` owns (ADR-047 §2.5)
//!
//! `Capabilities` carries the **cross-actor / cross-runtime grants** that
//! survive the post-Phase 3b collapse. Five strings are in scope today
//! (see `CAP_*` constants in `peko-rs/core/src/common/authority.rs`):
//!
//! | String                     | Owner / purpose                                              |
//! |----------------------------|--------------------------------------------------------------|
//! | `principal:write_config`   | Write the principal's `principal.toml`                       |
//! | `principal:write_agents`   | Write the principal's `agents/` directory                    |
//! | `principal:write_identity` | Write the principal's `identity/` directory (DID material)   |
//! | `principal:write_mcps`     | Write the principal's `mcp/` directory                       |
//! | `runtime:write_extensions` | Write the runtime `extensions/` directory (reserved; no IPC) |
//!
//! Every string above is a **runtime gate**: the `RuntimeAuthority`
//! `*_write(Option<&Capabilities>)` accessors fail closed without it
//! (see `assert_capability_granted`). The `principal:write_cron`
//! grant was retired 2026-08-25 when cron became an internal
//! principal tool gated by `tool:Cron{Create,List,Delete}`.
//!
//! # What `Capabilities` does NOT own
//!
//! Per ADR-047 §2.5, the following kinds moved off `Capabilities` and
//! onto the workspace (tool/agent catalog) or simply retired when the
//! `Authority` envelope was deleted in PR-E #1:
//!
//! | Retired kind     | New surface                                                  |
//! |------------------|--------------------------------------------------------------|
//! | `tool:Bash`      | Workspace tool; visible by default                           |
//! | `tool:Write`     | Workspace tool; visible by default                           |
//! | `tool:Edit`      | Workspace tool; visible by default                           |
//! | `network`        | Retired with `Authority` envelope (PR-E #1)                  |
//! | `filesystem.*`   | Retired with `Authority` envelope (PR-E #1)                  |
//! | `tunnel:*`       | Retired with `Authority` envelope (PR-E #1)                  |
//! | `agent:*`        | Subagent dispatch lives on `subagent_capabilities` snapshot  |
//! | `skill:*`        | Workspace-resident; visible by default                       |
//! | `tool:<name>`    | F37 funnel gate; checked in agentic-loop per tool call       |
//!
//! `tool:<name>` and `agent:*` strings **DO still appear** in the
//! `[capabilities]` table of `principal.toml` — they are checked by
//! the F37 agentic-loop funnel / subagent dispatch snapshot, not by
//! this type. They round-trip through `Capabilities` only because
//! the type is still the canonical store for whatever grant strings
//! the principal carries.
//!
//! # ADR-046 high-power classifier — DELETED
//!
//! `Capability::is_high_power` was deleted in Phase 3b alongside the
//! capability-grant IPC handler. ADR-046 §3 still describes the
//! classifier as live; ADR-047 §2.5 documents the replacement
//! ("authority tier widening"). The replacement audit hook was
//! keyed off `[authority]` field widening (network flip,
//! runtime_paths write) and was retired with the `Authority`
//! envelope in PR-E #1 — there is no longer a high-power classifier
//! surface in the runtime.
//!
//! # IPC surface
//!
//! `peko-rs/core/src/ipc/handlers/capability.rs` and the IPC variants
//! `CapabilityGrant` / `CapabilityList` / `CapabilityRevoke` were
//! retired in PR #363 (ADR-047 Phases 7+8, commit `5ad12b6e`). The
//! only IPC path that still mentions capabilities is the wire
//! projection `Vec<String>` (`peko-rs/core/src/ipc/packet.rs`).
//!
//! # Wildcard semantics
//!
//! A grant satisfies a requirement when:
//! - they are identical strings, or
//! - the grant ends in `*` and the requirement starts with the
//!   prefix before the wildcard.
//!
//! In practice the only wildcards in production are `runtime:*` (no
//! caller uses it today) and `principal:*` (no caller uses it
//! today). The `tool:*` wildcard is checked by the F37 funnel on
//! `Vec<String>` projections of `Capabilities`, not through this
//! type's `is_granted`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

/// A typed capability grant.
///
/// `Capability` is a newtype around `String`. The string taxonomy is
/// the canonical contract — see the module doc-comment above for the
/// three strings that are runtime-gated (`principal:write_*`) vs the
/// kinds that retired with the `Authority` envelope (PR-E #1) or
/// live behind the F37 funnel.
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
    /// `principal:write_config`, `principal:write_agents`, and
    /// `principal:write_identity`.
    ///
    /// 2026-08-25: `principal:write_cron` retired. Cron is now an
    /// internal principal tool gated by `tool:Cron{Create,List,Delete}`
    /// grants (workspace-owned, not on `Capabilities`).
    ///
    /// The `tool:*` / `agent:*` / `skill:*` / `network` /
    /// `filesystem.*` / `tunnel:*` grants that used to live here
    /// retired when the `Authority` envelope was deleted in PR-E
    /// #1 (network/filesystem/tunnel) or when those concerns moved
    /// to the per-principal workspace (tool/agent catalog).
    /// Workspace tools are principal-owned and visible by default;
    /// no capability gating is required to enumerate them in
    /// `available_tools`.
    #[must_use]
    pub fn starter_bundle() -> Self {
        Self::with_grants([
            "principal:write_config",
            "principal:write_agents",
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
