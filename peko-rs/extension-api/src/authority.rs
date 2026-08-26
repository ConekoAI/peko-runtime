//! Authority model — ADR-047 §2.5.
//!
//! Replaces the previous `Vec<Capability>` + tier grant system with a
//! flat per-tier path + network + tunnel surface. The principal's
//! `[authority]` block in `principal.toml` is the new single source of
//! truth for filesystem, network, and tunnel authority.
//!
//! Per ADR-047 §2.5, only filesystem/network/tunnel grants move into
//! `Authority`. `principal:write_*` and `runtime:write_*` capability
//! grants remain in `Capabilities` for cross-actor / cross-runtime
//! authorization (a peer channel still has to prove a write grant
//! before touching the principal's tier).
//!
//! ## On-disk shape
//!
//! ```toml
//! [authority]
//! local_paths   = ["~/projects/**"]                 # read/write within principal workspace
//! shared_paths  = ["/srv/shared/**"]                # read/write to cross-principal shared area
//! runtime_paths = ["/var/run/peko/**"]              # read/write to runtime-controlled paths
//!
//! network       = "deny" | "allow" | "allow:<host-pattern>"
//! tunnel        = false                             # or true / list of peer DIDs
//! ```
//!
//! ## Phase 3 status (post-PR #363, ADR-047 §5)
//!
//! The IPC capability handler that mutated `[capabilities]` grants was
//! retired in PR #363 (commit `5ad12b6e`). The runtime continues to
//! consult `Capabilities` for write-side gating through the
//! `RuntimeAuthority::shared_*_write` and
//! `runtime_extensions_root_write` accessors; the `[authority]` block
//! is read on load and projected onto filesystem/network/tunnel
//! gates, not onto the per-resource capability checks. The
//! `Capability::is_high_power` classifier referenced below was
//! deleted alongside the IPC handler — ADR-046 §3 / §6 still describe
//! it; the replacement is "authority tier widening" per ADR-047
//! §2.5 (network flip, runtime_paths write).
//!
//! Migration shim: when no `[authority]` block is present, the
//! legacy `[[capabilities.grants]]` entries are translated by
//! [`Authority::from_legacy_capabilities`]. The translation moves
//! `tool:*` / `network` / `filesystem.*` / `tunnel:*` onto
//! `Authority` and leaves `principal:write_*` / `runtime:write_*` in
//! `Capabilities` — see the `Capabilities` module doc-comment for the
//! canonical contract.

use serde::{Deserialize, Serialize};

use crate::capabilities::Capabilities;

/// The principal's authority envelope.
///
/// Fields are all `#[serde(default)]`; an empty `Authority` (the
/// default) means no filesystem authority, no network egress, no
/// tunnel — the strictest posture. Runtime gating today reads
/// `[authority]` for filesystem/network/tunnel decisions and
/// `[capabilities].grants` for the `principal:write_*` /
/// `runtime:write_*` per-resource gate — see
/// `peko-rs/core/src/common/authority.rs` for the gate composition.
/// The original plan to migrate the `principal:write_*` checks onto
/// `Authority` was reversed in ADR-047 §2.5: those grants are
/// cross-actor / cross-runtime audit markers, not tier-path grants.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authority {
    /// Read/write paths within the principal's local tier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_paths: Vec<String>,

    /// Read/write paths within the shared (cross-principal) tier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_paths: Vec<String>,

    /// Read/write paths within the runtime tier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_paths: Vec<String>,

    /// Network egress authority.
    #[serde(default)]
    pub network: NetworkAccess,

    /// Cross-runtime tunnel authority.
    #[serde(default)]
    pub tunnel: TunnelAccess,
}

impl Authority {
    /// Translate the legacy `[[capabilities.grants]]` shape into an
    /// `Authority` envelope.
    ///
    /// Only invoked from `PrincipalConfig::resolved_authority` when
    /// no explicit `[authority]` block is present. The mapping
    /// follows ADR-047 §2.5 verbatim:
    ///
    /// | Legacy grant           | Authority field                  |
    /// |------------------------|----------------------------------|
    /// | `tool:Bash`            | `local_paths += ["**"]`          |
    /// | `tool:Write`           | `local_paths += ["**"]`          |
    /// | `tool:Edit`            | `local_paths += ["**"]`          |
    /// | `network`              | `NetworkAccess("allow")`         |
    /// | `filesystem.read:*`    | `local_paths += <path>`          |
    /// | `filesystem.write:*`   | `local_paths += <path>`          |
    /// | `tunnel:*`             | `TunnelAccess::Bool(true)`       |
    /// | `principal:*`          | (kept in `Capabilities`)         |
    /// | `runtime:*`            | (kept in `Capabilities`)         |
    /// | anything else          | dropped — `tool:Read` etc. need  |
    /// |                        | no authority grant               |
    ///
    /// Returns the migrated `Authority` even when the input grants
    /// are empty — the caller decides whether the empty result is
    /// "explicit deny" or "carry on".
    #[must_use]
    pub fn from_legacy_capabilities(caps: &Capabilities) -> Self {
        let mut authority = Authority::default();
        let mut has_local_paths = false;
        let mut has_network_allow = false;
        let mut has_tunnel = false;

        for cap in caps.iter() {
            let s = cap.as_str();
            match s {
                "tool:Bash" | "tool:Write" | "tool:Edit" => {
                    if !has_local_paths {
                        authority.local_paths.push("**".to_string());
                        has_local_paths = true;
                    }
                }
                "network" => {
                    if !has_network_allow {
                        authority.network = NetworkAccess("allow".to_string());
                        has_network_allow = true;
                    }
                }
                s if s.starts_with("filesystem.read:") || s.starts_with("filesystem.write:") => {
                    let path = s.split_once(':').map(|(_, v)| v).unwrap_or("");
                    if !path.is_empty() && !has_local_paths {
                        authority.local_paths.push(path.to_string());
                    }
                }
                s if s.starts_with("tunnel") && !has_tunnel => {
                    authority.tunnel = TunnelAccess::Bool(true);
                    has_tunnel = true;
                }
                // `principal:*`, `runtime:*`, and other capability
                // kinds stay in `Capabilities`. `tool:Read` and the
                // other tool grants need no authority grant.
                _ => {}
            }
        }

        authority
    }
}

/// Network egress authority.
///
/// On disk the field is a single string:
/// - `"deny"` — no outbound calls (default).
/// - `"allow"` — allow all outbound calls.
/// - `"allow:<host-pattern>"` — allow calls matching the glob.
///
/// The internal representation is the literal string; the runtime
/// switches on `is_deny()` / `is_allow()`. We keep the value as a
/// `String` rather than a strongly-typed enum because the
/// host-pattern form is open-ended user input — making it a string
/// avoids the deserialize round-trip cost and lets the runtime
/// parse the pattern lazily.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NetworkAccess(pub String);

impl Default for NetworkAccess {
    fn default() -> Self {
        NetworkAccess("deny".to_string())
    }
}

impl NetworkAccess {
    /// Construct from a literal `"deny" | "allow" | "allow:<pattern>"` string.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_deny(&self) -> bool {
        self.0 == "deny"
    }

    #[must_use]
    pub fn is_allow(&self) -> bool {
        self.0 == "allow" || self.0.starts_with("allow:")
    }
}

/// Cross-runtime tunnel authority.
///
/// On disk:
/// - `false` (default) — no tunnel.
/// - `true` — any peer allowed.
/// - `["did:peko:...", ...]` — only the listed peer DIDs allowed.
///
/// `untagged` deserialization tries the list variant first (an empty
/// list `[]` only matches the list), then the bool variant (which
/// matches both `false` and `true`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TunnelAccess {
    /// List of allowed peer DIDs.
    Peers(Vec<String>),
    /// `false` = no tunnel (default), `true` = any peer.
    Bool(bool),
}

impl Default for TunnelAccess {
    fn default() -> Self {
        TunnelAccess::Bool(false)
    }
}

impl TunnelAccess {
    /// Whether no tunnel is allowed at all (`false`).
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(self, TunnelAccess::Bool(false))
    }

    /// Whether `peer_did` is allowed under this tunnel policy.
    ///
    /// - `Bool(true)` — every peer is allowed.
    /// - `Bool(false)` — no peer is allowed.
    /// - `Peers(_)` — the DID must be in the list.
    #[must_use]
    pub fn allows(&self, peer_did: &str) -> bool {
        match self {
            TunnelAccess::Bool(true) => true,
            TunnelAccess::Bool(false) => false,
            TunnelAccess::Peers(peers) => peers.iter().any(|p| p == peer_did),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{Capabilities, Capability};

    #[test]
    fn authority_default_is_strict_deny() {
        let a = Authority::default();
        assert!(a.local_paths.is_empty());
        assert!(a.shared_paths.is_empty());
        assert!(a.runtime_paths.is_empty());
        assert!(a.network.is_deny());
        assert!(a.tunnel.is_disabled());
    }

    #[test]
    fn authority_roundtrip_full() {
        let a = Authority {
            local_paths: vec!["~/projects/**".to_string()],
            shared_paths: vec!["/srv/shared/**".to_string()],
            runtime_paths: vec!["/var/run/peko/**".to_string()],
            network: NetworkAccess::new("allow:*.anthropic.com"),
            tunnel: TunnelAccess::Peers(vec!["did:peko:public:alice".to_string()]),
        };
        let s = toml::to_string(&a).expect("serialize");
        let back: Authority = toml::from_str(&s).expect("deserialize");
        assert_eq!(a, back);
    }

    #[test]
    fn authority_deserializes_toml_block() {
        let toml = r#"
            local_paths   = ["~/projects/**"]
            shared_paths  = ["/srv/shared/**"]
            runtime_paths = ["/var/run/peko/**"]
            network       = "allow:*.anthropic.com"
            tunnel        = ["did:peko:public:alice"]
        "#;
        let a: Authority = toml::from_str(toml).expect("deserialize Authority");
        assert_eq!(a.local_paths, vec!["~/projects/**".to_string()]);
        assert_eq!(a.shared_paths, vec!["/srv/shared/**".to_string()]);
        assert_eq!(a.runtime_paths, vec!["/var/run/peko/**".to_string()]);
        assert_eq!(a.network.as_str(), "allow:*.anthropic.com");
        assert!(
            matches!(a.tunnel, TunnelAccess::Peers(ref p) if p == &vec!["did:peko:public:alice".to_string()])
        );
    }

    #[test]
    fn network_access_predicates() {
        assert!(NetworkAccess::new("deny").is_deny());
        assert!(!NetworkAccess::new("deny").is_allow());

        assert!(!NetworkAccess::new("allow").is_deny());
        assert!(NetworkAccess::new("allow").is_allow());

        assert!(!NetworkAccess::new("allow:*.anthropic.com").is_deny());
        assert!(NetworkAccess::new("allow:*.anthropic.com").is_allow());

        // Unknown value: not deny, not allow.
        assert!(!NetworkAccess::new("limited").is_deny());
        assert!(!NetworkAccess::new("limited").is_allow());
    }

    #[test]
    fn tunnel_access_deserializes_three_shapes() {
        // TOML doesn't accept bare `false` / `true` / `[...]` at the
        // document root — wrap each in a one-key table.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            tunnel: TunnelAccess,
        }
        let f: Wrapper = toml::from_str("tunnel = false").expect("deserialize false");
        assert!(matches!(f.tunnel, TunnelAccess::Bool(false)));
        assert!(f.tunnel.is_disabled());

        let t: Wrapper = toml::from_str("tunnel = true").expect("deserialize true");
        assert!(matches!(t.tunnel, TunnelAccess::Bool(true)));
        assert!(!t.tunnel.is_disabled());
        assert!(t.tunnel.allows("did:peko:public:anyone"));

        let p: Wrapper =
            toml::from_str(r#"tunnel = ["did:peko:public:alice", "did:peko:public:bob"]"#)
                .expect("deserialize peer list");
        assert!(matches!(p.tunnel, TunnelAccess::Peers(ref v) if v.len() == 2));
        assert!(p.tunnel.allows("did:peko:public:alice"));
        assert!(p.tunnel.allows("did:peko:public:bob"));
        assert!(!p.tunnel.allows("did:peko:public:carol"));

        // Empty list is `Peers(vec![])` — same behavior as `false`.
        let e: Wrapper = toml::from_str("tunnel = []").expect("deserialize empty list");
        assert!(matches!(e.tunnel, TunnelAccess::Peers(ref v) if v.is_empty()));
        assert!(!e.tunnel.allows("did:peko:public:anyone"));
    }

    /// Round-trip through an `Authority` wrapper so TOML's
    /// document-root constraint is satisfied (a TOML document
    /// must be a table, not a bare value).
    #[test]
    fn authority_tunnel_default_is_disabled() {
        let a = Authority::default();
        assert!(a.tunnel.is_disabled());
        // Round-trip preserves the disabled state.
        let s = toml::to_string(&a).expect("serialize");
        let back: Authority = toml::from_str(&s).expect("deserialize");
        assert!(back.tunnel.is_disabled());
    }

    #[test]
    fn from_legacy_capabilities_bash_write_edit_yields_local_paths() {
        let caps = Capabilities::with_grants(["tool:Bash", "tool:Write", "tool:Edit"]);
        let a = Authority::from_legacy_capabilities(&caps);
        assert_eq!(a.local_paths, vec!["**".to_string()]);
        assert!(a.network.is_deny());
        assert!(a.tunnel.is_disabled());
    }

    #[test]
    fn from_legacy_capabilities_network_yields_allow() {
        let caps = Capabilities::with_grants(["network"]);
        let a = Authority::from_legacy_capabilities(&caps);
        assert!(a.local_paths.is_empty());
        assert_eq!(a.network.as_str(), "allow");
        assert!(a.tunnel.is_disabled());
    }

    #[test]
    fn from_legacy_capabilities_tunnel_yields_bool_true() {
        let caps = Capabilities::with_grants(["tunnel:create"]);
        let a = Authority::from_legacy_capabilities(&caps);
        assert!(a.local_paths.is_empty());
        assert!(a.network.is_deny());
        assert!(matches!(a.tunnel, TunnelAccess::Bool(true)));
    }

    #[test]
    fn from_legacy_capabilities_filesystem_path_extracted() {
        let caps = Capabilities::with_grants(["filesystem.read:/etc/peko"]);
        let a = Authority::from_legacy_capabilities(&caps);
        assert_eq!(a.local_paths, vec!["/etc/peko".to_string()]);
    }

    #[test]
    fn from_legacy_capabilities_principal_and_runtime_grants_ignored() {
        // Per ADR-047 §2.5 these stay in `Capabilities`. The
        // migration shim must NOT pull them into Authority.
        let caps = Capabilities::with_grants(["principal:write_config", "runtime:trust"]);
        let a = Authority::from_legacy_capabilities(&caps);
        assert!(a.local_paths.is_empty());
        assert!(a.network.is_deny());
        assert!(a.tunnel.is_disabled());
    }

    #[test]
    fn from_legacy_capabilities_tool_read_dropped() {
        // `tool:Read` doesn't need an authority grant per ADR-047
        // §2.5 (read-only).
        let caps = Capabilities::with_grants(["tool:Read"]);
        let a = Authority::from_legacy_capabilities(&caps);
        assert!(a.local_paths.is_empty());
    }

    #[test]
    fn from_legacy_capabilities_empty_yields_default() {
        let caps = Capabilities::new();
        let a = Authority::from_legacy_capabilities(&caps);
        assert!(a.local_paths.is_empty());
        assert!(a.network.is_deny());
        assert!(a.tunnel.is_disabled());
    }

    #[test]
    fn from_legacy_capabilities_dedupes_local_paths() {
        // All three Bash/Write/Edit entries collapse to a single
        // `**`. We don't want three duplicate entries.
        let caps = Capabilities::with_grants(["tool:Bash", "tool:Write", "tool:Edit"]);
        let a = Authority::from_legacy_capabilities(&caps);
        assert_eq!(a.local_paths.len(), 1);
    }

    #[test]
    fn authority_ignores_unused_capability_kind() {
        // `Capability::new` round-trips arbitrary strings — make sure
        // the migration shim doesn't crash on a future unknown kind.
        let caps = Capabilities::with_grants(["future:cool_capability"]);
        let a = Authority::from_legacy_capabilities(&caps);
        assert!(a.local_paths.is_empty());
        // Sanity: the Capability value itself is preserved verbatim.
        assert!(caps.contains(&Capability::new("future:cool_capability")));
    }
}
