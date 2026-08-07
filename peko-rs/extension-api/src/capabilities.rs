//! Capability model for extension-framework authority.
//!
//! A capability is a typed grant such as `tool:Read`, `agent:researcher`,
//! `skill:github_skill`, `filesystem.read:/path`, or `network`.  A Principal's
//! `[capabilities] grants = [...]` array in `principal.toml` is the single
//! source of truth for what the Principal is allowed to do, but the types
//! themselves are generic authorization primitives used by the extension
//! framework.

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

    /// ADR-046 trust+audit: classify a capability as "high power"
    /// — grants the user-visible warning after the grant lands.
    ///
    /// Definition (v1, hand-curated):
    /// - `tool:Bash`, `tool:Write`, `tool:Edit` (filesystem/code
    ///   mutation — direct disk writes, no gate)
    /// - `network`, `tunnel:*` (egress authority)
    /// - `filesystem.*` and `fs.*` (filesystem authority outside the
    ///   tool-mediated layer)
    /// - `principal:*`, `runtime:*` (cross-principal / runtime
    ///   identity authority)
    ///
    /// Everything else (tool:Read, skill:*, agent:*) is low-power
    /// and only emits an Info audit event. This is a coarse,
    /// readable threshold — the user is asking for "things the
    /// operator should glance at", not a security model.
    #[must_use]
    pub fn is_high_power(&self) -> bool {
        let s = self.0.as_str();
        if s == "network" {
            return true;
        }
        let kind = self.kind();
        // `tool:`-prefixed capabilities: classify by the tool name
        // (the `value()` half). High-power tools are mutating /
        // shell-execution ones.
        if kind == "tool" {
            return matches!(self.value(), "Bash" | "Write" | "Edit");
        }
        // Everything else: a kind prefix that names a wide
        // authority domain.
        kind.starts_with("filesystem")
            || kind.starts_with("tunnel")
            || kind.starts_with("principal")
            || kind.starts_with("runtime")
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
    /// This grants the built-in tools and agents needed for basic operation
    /// without handing over unrestricted authority.
    ///
    /// **Phase C:** the bundle also carries the write-side capability
    /// prefixes consumed by `peko_core::common::authority::RuntimeAuthority`'s
    /// `_write` accessors — `principal:write_config`, `principal:write_agents`,
    /// and `principal:write_cron` — so a freshly created Principal can write
    /// to its own `principal.toml`, agents dir, and cron schedule without
    /// a separate grant step. Pre-Phase-C on-disk principals that lack
    /// these grants will surface `AuthorityError::CapabilityDenied` on their
    /// first write; that's a one-time user-visible bootstrap (prelaunch,
    /// no backward compat).
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
            // peko_plan DAG family (PR #1+2 wiring). Auto-granted to
            // match `AgentConfig::enable_plan_tools: true` (default) —
            // without these, `is_tool_enabled` filters the 7 plan
            // tools out of the LLM's available_tools at runtime and
            // the principal can't actually plan despite the agent
            // config being on.
            "tool:PlanCreate",
            "tool:PlanList",
            "tool:PlanGet",
            "tool:PlanMarkStep",
            "tool:PlanRecordEvidence",
            "tool:PlanAddStep",
            "tool:PlanClose",
            // Async control family (auto-granted to match the
            // builtin async_control tools). Without these, the model
            // sees the tool descriptions in `available_tools` but
            // `is_tool_enabled` filters them out at dispatch and the
            // principal can't actually call them — the agent would
            // surface "tool not in my toolset" errors when it tries
            // `AsyncSpawn` / `AsyncList` / etc.
            "tool:AsyncSpawn",
            "tool:AsyncOutput",
            "tool:AsyncStatus",
            "tool:AsyncList",
            "tool:AsyncStop",
            // Cron scheduling family (auto-granted to match the
            // `principal:write_cron` grant below). The tools themselves
            // are registered by `ToolRuntime::register_builtins` and
            // backed by the daemon-installed `CronRuntime`; without
            // these `tool:` grants, `is_tool_enabled` filters them out
            // of the LLM's toolset and the principal can't schedule
            // reminders conversationally despite holding the write
            // capability (2026-08-07 field test, Finding 2).
            "tool:CronCreate",
            "tool:CronList",
            "tool:CronDelete",
            "principal:write_config",
            "principal:write_agents",
            "principal:write_cron",
            // PR #339: required by the `CronHistory` IPC handler so a
            // starter principal can read its own cron run history. The
            // cron engine writes the log on the principal's behalf at
            // execution time; the IPC read path gates on this same cap
            // so starter principals work out-of-the-box. Custom
            // principals can revoke it via `principal revoke` to lock
            // history reads down.
            "principal:write_cron_history",
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

    /// ADR-046 trust+audit: pin the v1 high-power classification
    /// so future additions are deliberate. If a new capability
    /// kind crosses the line, this test fails until the operator
    /// confirms the user-visible warning should fire.
    #[test]
    fn is_high_power_classification_v1() {
        // Mutating tools + shell exec are high-power.
        assert!(Capability::new("tool:Bash").is_high_power());
        assert!(Capability::new("tool:Write").is_high_power());
        assert!(Capability::new("tool:Edit").is_high_power());
        // Read-only tool is NOT high-power.
        assert!(!Capability::new("tool:Read").is_high_power());
        // Bare egress / authority-domain prefixes are high-power.
        assert!(Capability::new("network").is_high_power());
        assert!(Capability::new("tunnel:create").is_high_power());
        assert!(Capability::new("filesystem.read:/etc").is_high_power());
        assert!(Capability::new("principal:write_identity").is_high_power());
        assert!(Capability::new("runtime:trust").is_high_power());
        // Skill / agent grants are low-power.
        assert!(!Capability::new("skill:github").is_high_power());
        assert!(!Capability::new("agent:researcher").is_high_power());
        // Capability with no `:` separator and not "network" is
        // low-power (catches the unknown-bare-name case).
        assert!(!Capability::new("something").is_high_power());
    }

    #[test]
    fn starter_bundle_includes_builtins() {
        let caps = Capabilities::starter_bundle();
        assert!(caps.is_granted(&Capability::new("tool:Read")));
        assert!(caps.is_granted(&Capability::new("agent:researcher")));
        assert!(!caps.is_granted(&Capability::new("skill:unknown")));
    }

    /// Auto-grant all 7 peko_plan DAG tools so a fresh principal can
    /// actually plan out of the box. Without these, `is_tool_enabled`
    /// filters them out of the LLM's available_tools list and the
    /// principal can't invoke them despite `AgentConfig::enable_plan_tools`
    /// being true by default.
    #[test]
    fn starter_bundle_includes_plan_tools() {
        let caps = Capabilities::starter_bundle();
        for tool in [
            "PlanCreate",
            "PlanList",
            "PlanGet",
            "PlanMarkStep",
            "PlanRecordEvidence",
            "PlanAddStep",
            "PlanClose",
        ] {
            assert!(
                caps.is_granted(&Capability::new(format!("tool:{tool}"))),
                "starter_bundle must include tool:{tool}"
            );
        }
    }

    /// Auto-grant all 5 async control tools so a fresh principal can
    /// actually background and inspect long-running tasks. Without
    /// these, `is_tool_enabled` filters them out of the LLM's
    /// `available_tools` list despite the framework describing them
    /// in tool descriptions — the model would see them as available
    /// but get refused at dispatch with a "tool not in my toolset"
    /// style error. Surfaced in the 2026-08-02 subagent field test.
    #[test]
    fn starter_bundle_includes_async_tools() {
        let caps = Capabilities::starter_bundle();
        for tool in [
            "AsyncSpawn",
            "AsyncOutput",
            "AsyncStatus",
            "AsyncList",
            "AsyncStop",
        ] {
            assert!(
                caps.is_granted(&Capability::new(format!("tool:{tool}"))),
                "starter_bundle must include tool:{tool}"
            );
        }
    }

    /// Auto-grant the 3 cron scheduling tools so a fresh principal can
    /// schedule reminders conversationally. The tools are registered by
    /// `ToolRuntime::register_builtins` and backed by the daemon's
    /// `CronRuntime`; without these grants `is_tool_enabled` filters
    /// them out despite `principal:write_cron` being granted — the
    /// model then honestly reports "I have no scheduling tool".
    /// Surfaced in the 2026-08-07 cron/session field test.
    #[test]
    fn starter_bundle_includes_cron_tools() {
        let caps = Capabilities::starter_bundle();
        for tool in ["CronCreate", "CronList", "CronDelete"] {
            assert!(
                caps.is_granted(&Capability::new(format!("tool:{tool}"))),
                "starter_bundle must include tool:{tool}"
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
