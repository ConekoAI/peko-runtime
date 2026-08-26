use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use peko_auth::host::PrincipalResourceView;
pub use peko_auth::{Exposure, Permission, PermissionGrant};
use peko_extension_api::Capabilities;
use peko_quota::QuotaConfig;
use peko_subject::PrincipalDID;

/// Persisted live status for a Principal's tunnel instance.
///
/// Principal-owned mirror of `tunnel::protocol::InstanceStatus`; converted at
/// the tunnel edge. `None` at the `PrincipalConfig` level means the daemon's
/// runtime state is the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Online,
    Offline,
    Busy,
    Error,
}

/// Transport preference for cross-runtime principal_send.
///
/// Principal-owned mirror of `tunnel::known_runtimes::TransportPreference`;
/// converted at the tunnel edge, so the persisted config does not depend on
/// tunnel types.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPreference {
    /// Prefer direct if a direct endpoint is configured and trusted,
    /// otherwise fall back to the PekoHub tunnel.
    #[default]
    Auto,
    /// Always use the PekoHub tunnel.
    Tunnel,
    /// Always use the direct endpoint; fail if one is not configured.
    Direct,
}

/// On-disk configuration for a Principal. Deserialized from `principal.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalConfig {
    pub name: String,

    /// Optional stable DID. If omitted, the runtime generates a local DID
    /// from the principal name and id on first creation.
    #[serde(default)]
    pub did: Option<PrincipalDID>,

    #[serde(default)]
    pub owner: peko_auth::Subject,

    #[serde(default)]
    pub identity: PrincipalIdentityConfig,

    #[serde(default)]
    pub intent: PrincipalIntentConfig,

    #[serde(default)]
    pub governance: PrincipalGovernanceConfig,

    #[serde(default)]
    pub memory: PrincipalMemoryConfig,

    #[serde(default)]
    pub routing: PrincipalRoutingConfig,

    /// Capability grants for this Principal.
    ///
    /// On disk this is written as `[capabilities] grants = [...]`.
    /// This is the single source of truth for what the Principal is allowed
    /// to do.
    #[serde(default, rename = "capabilities")]
    pub capabilities: Capabilities,

    // PR-E #1: the `authority: Option<Authority>` field (Phase 3a
    // envelope) and the `resolved_authority()` method that bridged
    // legacy grants onto it are deleted. ADR-047 §2.5 had planned to
    // route the runtime `_write(Option<&Caps>)` accessors onto
    // `Authority`, but no producer of `[authority]` field checks
    // ever landed and the envelope had zero consumers outside the
    // deserialization path. On-disk `[authority]` blocks written by
    // pre-PR-E-#1 builds will be silently ignored going forward —
    // same forward-only behavior as every other pre-launch migration.
    // Anyone who actually used `[authority]` to gate network or
    // tunnel can move that policy into the `Capabilities` grant set
    // (the runtime gate for `principal:write_*` / `runtime:*`
    // survived intact — see `peko_extension_api::Capabilities`).

    /// Network exposure level for this Principal.
    #[serde(default)]
    pub exposure: Exposure,

    /// Persisted live status for this Principal's tunnel instance.
    /// `None` means the daemon's runtime state is the source of truth
    /// (typically `Online` when the daemon is up). Set by
    /// `PrincipalSetStatus` to mark the Principal as `Busy`, `Offline`,
    /// etc., across daemon restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,

    /// Explicit permission grants on this Principal.
    ///
    /// **Authoritative ACL for this runtime (R4).** These
    /// `PermissionGrant` entries are the source of truth for who may
    /// invoke `principal_send` on this Principal and what they may do;
    /// the auth layer (`peko-auth::ownership`) consults this list
    /// first and rejects anything not granted here. PekoHub's
    /// `instances.allowedPrincipals` JSONB column is being dropped
    /// (H4) and the hub-side mirror is NOT pushed down — PekoHub only
    /// knows who may see the instance in the directory, but the
    /// runtime's policy decides who can actually invoke it. Do not
    /// introduce any IPC path that overwrites this list from a
    /// hub-provided value.
    ///
    /// Distinct from `capabilities` (above), which lists the
    /// tools/extensions the Principal itself may use; `permissions`
    /// is the inbound ACL.
    #[serde(default)]
    pub permissions: Vec<PermissionGrant>,

    /// Optional configured model id pinned to this Principal. Every
    /// LLM call routed through this Principal's root agent uses this
    /// model unless overridden per-message (`peko send --model`).
    /// Principals must be created with a model; this field is optional
    /// on disk only to avoid breaking deserialization of pre-launch
    /// configs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_model_id: Option<String>,

    /// Transport preference for cross-runtime principal_send.
    /// The principal owns the connection method; callers learn it from
    /// the directory and respect it.
    #[serde(default)]
    pub transport_preference: TransportPreference,

    /// Optional per-principal token quota (F18). When present,
    /// every LLM call routed through this Principal — root agent,
    /// A2A inbound, subagent spawn — counts against the limits,
    /// which reset on a calendar-aligned UTC cycle (Hourly /
    /// Daily / Weekly / Monthly). When `None`, the Principal is
    /// unquota'd and every call is free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<QuotaConfig>,

    /// Standing named children (agent-session paradigm, Phase 2).
    ///
    /// On disk: `[children.<name>]` tables. Each `<name>` is the
    /// child's slug (per-parent-unique path segment, validated at
    /// load) and maps to `{ subagent_type, description? }`. The
    /// runtime ensures each declared child exists as a `standing`
    /// session under the principal's owner root session at root-agent
    /// run setup (`principal::children::ensure_declared_children`);
    /// the `Agent` tool's `new` action with a matching `name`
    /// attaches to the standing session instead of spawning fresh.
    ///
    /// `BTreeMap` keeps the serialized form stable (sorted by name).
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_children"
    )]
    pub children: BTreeMap<String, ChildDeclaration>,
}

/// A standing named child declared under `[children]` in
/// `principal.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildDeclaration {
    /// Agent type the child runs as when attached/resumed (required;
    /// must be nonempty).
    pub subagent_type: String,
    /// Optional human-readable description; becomes the session title
    /// when the child is created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Deserialize + validate the `[children]` table: every key must pass
/// slug validation (`peko_session::path::validate_slug`) and every
/// entry needs a nonempty `subagent_type`. A missing `subagent_type`
/// key is refused by serde itself (`missing field` error); the checks
/// here cover the key shape and blank values.
fn deserialize_children<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, ChildDeclaration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let map = BTreeMap::<String, ChildDeclaration>::deserialize(deserializer)?;
    for (name, decl) in &map {
        peko_session::path::validate_slug(name).map_err(serde::de::Error::custom)?;
        if decl.subagent_type.trim().is_empty() {
            return Err(serde::de::Error::custom(format!(
                "children.{name}: subagent_type must not be empty"
            )));
        }
    }
    Ok(map)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalIdentityConfig {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalIntentConfig {
    pub goals: Vec<String>,
    pub values: Vec<String>,
    pub preferences: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalGovernanceConfig {
    #[serde(default)]
    pub audit: AuditLevel,
    #[serde(default = "default_max_delegation_depth")]
    pub max_delegation_depth: u32,
    #[serde(default)]
    pub auto_grant_tools: Vec<String>,
    #[serde(default)]
    pub delegations: Vec<DelegationGrant>,
}

fn default_max_delegation_depth() -> u32 {
    3
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditLevel {
    #[default]
    All,
    Commands,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationGrant {
    pub to: peko_auth::Subject,
    pub permissions: Vec<Permission>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalMemoryConfig {
    #[serde(default)]
    pub tier: MemoryTier,
    #[serde(default)]
    pub consolidation: ConsolidationConfig,
    #[serde(default)]
    pub ttl_policy: TtlPolicy,
    #[serde(default)]
    pub include_artifacts: Vec<ArtifactKind>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    #[default]
    Single,
    MultiTier,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsolidationConfig {
    #[serde(default)]
    pub enabled: bool,
    pub interval: Option<String>,
    pub trigger: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtlPolicy {
    pub session: Option<String>,
    pub ephemeral: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Sessions,
    Todos,
    Files,
    Vectors,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalRoutingConfig {
    /// Optional path to a custom root agent prompt Markdown file.
    /// If omitted, the runtime uses the built-in root prompt.
    #[serde(default)]
    pub root_prompt: Option<PathBuf>,

    #[serde(default = "default_recall_top_k")]
    pub recall_top_k: usize,

    #[serde(default = "default_max_router_iterations")]
    pub max_router_iterations: usize,
}

impl Default for PrincipalRoutingConfig {
    fn default() -> Self {
        Self {
            root_prompt: None,
            recall_top_k: default_recall_top_k(),
            max_router_iterations: default_max_router_iterations(),
        }
    }
}

fn default_recall_top_k() -> usize {
    5
}

fn default_max_router_iterations() -> usize {
    5
}

// ---------------------------------------------------------------------------
// PR-E #1: `resolved_authority` — Phase 3a migration shim (ADR-047 §2.5)
// ---------------------------------------------------------------------------
//
// DELETED in this commit. The method returned an `Authority` envelope
// translated from legacy `[[capabilities.grants]]` entries when no
// explicit `[authority]` block was present. After the envelope itself
// was deleted (zero runtime consumers; only the deserialization path
// on `PrincipalConfig` ever produced a value), the migration shim has
// nothing left to translate. Capabilities remain the SoT for the
// `principal:write_*` / `runtime:*` grants (see
// `peko_extension_api::Capabilities`).
//
// impl PrincipalConfig {
//     pub fn resolved_authority(&self) -> Authority { ... }
// }

// ---------------------------------------------------------------------------
// `PrincipalResourceView` impl for `PrincipalConfig`
// ---------------------------------------------------------------------------

/// `PrincipalConfig` exposes the four fields
/// `auth::ownership::principal_resource` needs to build an
/// `auth::Resource::Principal` value.
///
/// ## Why a trait port and not a direct function in principal
///
/// The original code had `auth::ownership::principal_resource(name,
/// &PrincipalConfig)` taking the principal concrete type. That
/// creates a `peko-auth ↔ peko-principal` cycle when both become
/// workspace crates. The trait port in `peko-auth::host` flips the
/// direction — auth declares the contract, principal implements it
/// here.
///
/// ## Orphan rule note
///
/// This impl lives in `peko-principal` (not root) because both
/// `PrincipalResourceView` and `PrincipalConfig` are foreign types
/// from root's perspective. The trait is local to `peko_auth`, the
/// type is local to `peko_principal`, so the impl belongs here.
///
/// Note: this is the *opposite* direction from the `peko-auth →
/// peko-principal` import that used to live in
/// `auth::ownership::Resource::Principal`'s `exposure` field. The
/// `Exposure` enum used to live in `crate::principal::config` and
/// was imported by auth. To break the cycle, `Exposure` was lifted
/// into `peko-auth` (its natural home as part of `Resource`), and
/// `PrincipalConfig.exposure` is now typed as `peko_auth::Exposure`.
impl PrincipalResourceView for PrincipalConfig {
    fn name(&self) -> &str {
        &self.name
    }

    fn owner(&self) -> &peko_auth::Subject {
        &self.owner
    }

    fn permissions(&self) -> &[peko_auth::PermissionGrant] {
        &self.permissions
    }

    fn exposure(&self) -> peko_auth::Exposure {
        self.exposure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Existing `principal.toml` files in the wild must keep parsing —
    /// the new model field is `#[serde(default)]` so absence ==
    /// `None`.
    #[test]
    fn principal_config_without_model_field_parses() {
        let toml = r#"
            name = "legacy"
            exposure = "private"
        "#;
        let cfg: PrincipalConfig = toml::from_str(toml).expect("legacy TOML must parse");
        assert_eq!(cfg.name, "legacy");
        assert_eq!(cfg.preferred_model_id, None);
        assert_eq!(cfg.transport_preference, super::TransportPreference::Auto);
    }

    #[test]
    fn principal_config_transport_preference_roundtrip() {
        let cfg = PrincipalConfig {
            name: "alice".into(),
            did: None,
            owner: Default::default(),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Default::default(),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: super::TransportPreference::Tunnel,
            quota: None,
            children: Default::default(),
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            serialized.contains("transport_preference = \"tunnel\""),
            "got: {serialized}"
        );

        let back: PrincipalConfig = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(
            back.transport_preference,
            super::TransportPreference::Tunnel
        );
    }

    /// The model field must round-trip losslessly through serde so the
    /// `peko principal set-model` write path can persist it.
    #[test]
    fn principal_config_model_field_roundtrip() {
        let cfg = PrincipalConfig {
            name: "alice".into(),
            did: None,
            owner: Default::default(),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Default::default(),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_model_id: Some("ollama-llama3.1".into()),
            transport_preference: super::TransportPreference::Direct,
            quota: None,
            children: Default::default(),
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            serialized.contains("preferred_model_id = \"ollama-llama3.1\""),
            "got: {serialized}"
        );
        assert!(
            serialized.contains("transport_preference = \"direct\""),
            "got: {serialized}"
        );

        let back: PrincipalConfig = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(back.preferred_model_id.as_deref(), Some("ollama-llama3.1"));
        assert_eq!(
            back.transport_preference,
            super::TransportPreference::Direct
        );
    }

    /// Serializing a config with no provider hint must NOT emit the keys —
    /// `skip_serializing_if = "Option::is_none"` keeps the on-disk form clean
    /// for the common case.
    #[test]
    fn principal_config_without_hints_does_not_emit_keys() {
        let cfg = PrincipalConfig {
            name: "bob".into(),
            did: None,
            owner: Default::default(),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Default::default(),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: Default::default(),
            quota: None,
            children: Default::default(),
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            !serialized.contains("preferred_model_id"),
            "absent hint leaked into TOML: {serialized}"
        );
    }

    /// New `principal.toml` files use `[capabilities] grants = [...]`.
    #[test]
    fn principal_config_serializes_capabilities_grants() {
        let cfg = PrincipalConfig {
            name: "alice".into(),
            did: None,
            owner: Default::default(),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Capabilities::with_grants(["tool:Bash", "agent:researcher"]),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: Default::default(),
            quota: None,
            children: Default::default(),
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            serialized.contains("[capabilities]"),
            "expected [capabilities] table, got: {serialized}"
        );
        assert!(
            serialized.contains("grants = ["),
            "expected grants array, got: {serialized}"
        );
        assert!(
            !serialized.contains("allowed_extensions"),
            "legacy key leaked into TOML: {serialized}"
        );
    }

    /// `[capabilities] grants = [...]` parses into the new model.
    #[test]
    fn principal_config_accepts_capabilities_grants() {
        let toml = r#"
            name = "modern"
            exposure = "private"

            [capabilities]
            grants = ["tool:Bash", "agent:researcher"]
        "#;
        let cfg: PrincipalConfig = toml::from_str(toml).expect("modern TOML must parse");
        assert!(cfg.capabilities.is_granted(&"tool:Bash".into()));
        assert!(cfg.capabilities.is_granted(&"agent:researcher".into()));
        assert!(!cfg.capabilities.is_granted(&"tool:Write".into()));
    }

    /// Wildcard grants are expanded at evaluation time, not serialization.
    #[test]
    fn principal_config_wildcard_grants_match() {
        let cfg = PrincipalConfig {
            name: "wildcard".into(),
            did: None,
            owner: Default::default(),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Capabilities::with_grants(["tool:*", "agent:agency-agents/*"]),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: Default::default(),
            quota: None,
            children: Default::default(),
        };
        assert!(cfg.capabilities.is_granted(&"tool:Read".into()));
        assert!(cfg
            .capabilities
            .is_granted(&"agent:agency-agents/writer".into()));
        assert!(!cfg.capabilities.is_granted(&"agent:other/writer".into()));
    }

    /// R4: the inbound ACL contract.
    ///
    /// `PrincipalResourceView::permissions()` must surface the
    /// `permissions` Vec verbatim — auth's `check_permission` reads
    /// exactly this view, so any drift between the on-disk field
    /// and the surfaced slice breaks the inbound ACL silently. Pin
    /// both directions here:
    ///
    /// - An empty `permissions` list means nobody other than the
    ///   owner can invoke the Principal (no implicit grants from
    ///   PekoHub, no Public wildcard by default).
    /// - A `Public` grant opens the Principal to every caller.
    /// - A grant scoped to a specific Subject opens only that
    ///   Subject.
    ///
    /// Combined with the auth-side tests in
    /// `peko-auth::ownership::tests::test_principal_resource_permission_checks`,
    /// this locks the runtime ↔ auth contract.
    #[test]
    fn principal_config_permissions_surfaces_through_view() {
        use peko_auth::Subject;

        let alice = Subject::User("user:alice".to_string());
        let bob = Subject::User("user:bob".to_string());

        // Empty permissions: only owner has access.
        let cfg = PrincipalConfig {
            name: "lockdown".into(),
            did: None,
            owner: alice.clone(),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Default::default(),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: Default::default(),
            quota: None,
            children: Default::default(),
        };
        let view: &dyn peko_auth::host::PrincipalResourceView = &cfg;
        assert_eq!(view.permissions().len(), 0);
        assert_eq!(view.owner(), &alice);

        // Single Public grant: every caller authorized.
        let cfg = PrincipalConfig {
            permissions: vec![peko_auth::PermissionGrant {
                subject: Subject::Public,
                permission: peko_auth::Permission::Chat,
                granted_at: "2026-01-01T00:00:00Z".to_string(),
                granted_by: alice.clone(),
            }],
            ..make_test_config("public", alice.clone())
        };
        let view: &dyn peko_auth::host::PrincipalResourceView = &cfg;
        assert_eq!(view.permissions().len(), 1);
        assert_eq!(view.permissions()[0].subject, Subject::Public);

        // Scoped grant: only Bob gets Chat; other subjects don't.
        let cfg = PrincipalConfig {
            permissions: vec![peko_auth::PermissionGrant {
                subject: bob.clone(),
                permission: peko_auth::Permission::Chat,
                granted_at: "2026-01-01T00:00:00Z".to_string(),
                granted_by: alice.clone(),
            }],
            ..make_test_config("scoped", alice.clone())
        };
        let view: &dyn peko_auth::host::PrincipalResourceView = &cfg;
        assert_eq!(view.permissions().len(), 1);
        assert_eq!(view.permissions()[0].subject, bob);
    }

    /// Build a PrincipalConfig with the given name + owner and
    /// everything else defaulted. Helper for the permissions-view
    /// test so we don't repeat 12-field literals.
    fn make_test_config(name: &str, owner: peko_auth::Subject) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            did: None,
            owner,
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Default::default(),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: Default::default(),
            quota: None,
            children: Default::default(),
        }
    }

    // ─── [children] standing named children (Phase 2) ───────────────

    /// `[children]` parses into the map; absent table ⇒ empty map
    /// (and the key is not emitted on serialize).
    #[test]
    fn children_table_parses_and_defaults_empty() {
        let toml = r#"
            name = "kids"
            exposure = "private"

            [children.memory]
            subagent_type = "archivist"
            description = "Long-term memory curator"

            [children.about-user]
            subagent_type = "profiler"
        "#;
        let cfg: PrincipalConfig = toml::from_str(toml).expect("children TOML must parse");
        assert_eq!(cfg.children.len(), 2);
        let memory = &cfg.children["memory"];
        assert_eq!(memory.subagent_type, "archivist");
        assert_eq!(
            memory.description.as_deref(),
            Some("Long-term memory curator")
        );
        let about = &cfg.children["about-user"];
        assert_eq!(about.subagent_type, "profiler");
        assert_eq!(about.description, None);

        // Absent table ⇒ empty map.
        let cfg: PrincipalConfig = toml::from_str("name = \"plain\"").unwrap();
        assert!(cfg.children.is_empty());
        // …and an empty map is not emitted on serialize.
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            !serialized.contains("[children]"),
            "absent children leaked into TOML: {serialized}"
        );
    }

    /// A `[children]` key that fails slug validation is a structured
    /// load error (no silent acceptance of unusable names).
    #[test]
    fn children_rejects_invalid_names() {
        // Quoted TOML keys so `/` and whitespace survive to the
        // validator (a bare `with/slash` would be a TOML syntax error
        // before our check runs).
        for bad_key in [
            r#"children."with/slash""#,
            "children.' leading'",
            "children.'trailing '",
        ] {
            let toml = format!("name = \"x\"\n\n[{bad_key}]\nsubagent_type = \"t\"\n");
            let err = toml::from_str::<PrincipalConfig>(&toml).unwrap_err();
            assert!(
                err.to_string().contains("invalid slug"),
                "key {bad_key}: {err}"
            );
        }
    }

    /// `subagent_type` is required (missing key ⇒ serde `missing
    /// field`) and must not be blank.
    #[test]
    fn children_requires_nonempty_subagent_type() {
        let missing = "name = \"x\"\n\n[children.memory]\ndescription = \"d\"\n";
        let err = toml::from_str::<PrincipalConfig>(missing).unwrap_err();
        assert!(err.to_string().contains("subagent_type"), "missing: {err}");

        let blank = "name = \"x\"\n\n[children.memory]\nsubagent_type = \"  \"\n";
        let err = toml::from_str::<PrincipalConfig>(blank).unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "blank: {err}"
        );
    }

    /// The `[children]` table round-trips losslessly so config
    /// rewrites (e.g. `peko principal set-model`) don't drop it.
    #[test]
    fn children_roundtrip() {
        let mut cfg = make_test_config("rt", peko_auth::Subject::User("user:a".into()));
        cfg.children.insert(
            "memory".to_string(),
            ChildDeclaration {
                subagent_type: "archivist".to_string(),
                description: Some("curator".to_string()),
            },
        );
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            serialized.contains("[children.memory]"),
            "got: {serialized}"
        );
        let back: PrincipalConfig = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(back.children["memory"].subagent_type, "archivist");
        assert_eq!(
            back.children["memory"].description.as_deref(),
            Some("curator")
        );
    }

    // PR-E #1: 5 tests deleted alongside the `Authority` envelope and
    // `resolved_authority()` method. They were the only callers; the
    // migration shim's contract is now satisfied trivially (no
    // `[authority]` field to migrate, no shim method to exercise).
    //   - authority_table_parses_and_defaults_none
    //   - resolved_authority_prefers_explicit_block
    //   - resolved_authority_migrates_legacy_grants
    //   - resolved_authority_empty_yields_strict_deny
    //   - authority_roundtrip_full
}
