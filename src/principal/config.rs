use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::auth::{Permission, PermissionGrant};
use crate::principal::capability::Capabilities;
use crate::tunnel::protocol::{InstanceExposure, InstanceStatus};

/// On-disk configuration for a Principal. Deserialized from `principal.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalConfig {
    pub name: String,

    /// Optional stable DID. If omitted, the runtime generates a local DID
    /// from the principal name and id on first creation.
    #[serde(default)]
    pub did: Option<PrincipalDID>,

    #[serde(default)]
    pub owner: crate::auth::Subject,

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

    /// Network exposure level for this Principal.
    #[serde(default)]
    pub exposure: InstanceExposure,

    /// Persisted live status for this Principal's tunnel instance.
    /// `None` means the daemon's runtime state is the source of truth
    /// (typically `Online` when the daemon is up). Set by
    /// `PrincipalSetStatus` to mark the Principal as `Busy`, `Offline`,
    /// etc., across daemon restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<InstanceStatus>,

    /// Explicit permission grants on this Principal.
    ///
    /// These are ownership-style grants used by the auth layer for
    /// cross-runtime `principal_send`, distinct from extension capabilities.
    #[serde(default)]
    pub permissions: Vec<PermissionGrant>,

    /// Optional provider id pinned to this Principal. Overrides the
    /// global catalog default (`peko provider set-default`) for any
    /// LLM call routed through this Principal's root agent. Most
    /// Principals should leave this `None` and inherit the default;
    /// set it when a specific Principal needs a different model —
    /// e.g. an "offline" Principal pinned to a local Ollama while
    /// the rest of the runtime uses Anthropic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_provider_id: Option<String>,

    /// Optional model id pinned alongside `preferred_provider_id`.
    /// Falls back to the provider's `default_model_id` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_model_id: Option<String>,

    /// Transport preference for cross-runtime principal_send.
    /// The principal owns the connection method; callers learn it from
    /// the directory and respect it.
    #[serde(default)]
    pub transport_preference: crate::tunnel::known_runtimes::TransportPreference,
}

/// Thin wrapper around a DID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalDID(pub String);

impl PrincipalDID {
    /// Borrow the inner DID string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for PrincipalDID {
    fn from(s: String) -> Self {
        PrincipalDID(s)
    }
}

impl From<&str> for PrincipalDID {
    fn from(s: &str) -> Self {
        PrincipalDID(s.to_string())
    }
}

impl std::fmt::Display for PrincipalDID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
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
    pub to: crate::auth::Subject,
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

    #[serde(default = "default_context_window_messages")]
    pub context_window_messages: usize,

    #[serde(default = "default_recall_top_k")]
    pub recall_top_k: usize,

    #[serde(default = "default_max_router_iterations")]
    pub max_router_iterations: usize,
}

impl Default for PrincipalRoutingConfig {
    fn default() -> Self {
        Self {
            root_prompt: None,
            context_window_messages: default_context_window_messages(),
            recall_top_k: default_recall_top_k(),
            max_router_iterations: default_max_router_iterations(),
        }
    }
}

fn default_context_window_messages() -> usize {
    50
}

fn default_recall_top_k() -> usize {
    5
}

fn default_max_router_iterations() -> usize {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Existing `principal.toml` files in the wild must keep parsing —
    /// the new provider fields are `#[serde(default)]` so absence ==
    /// `None`.
    #[test]
    fn principal_config_without_provider_fields_parses() {
        let toml = r#"
            name = "legacy"
            exposure = "private"
        "#;
        let cfg: PrincipalConfig = toml::from_str(toml).expect("legacy TOML must parse");
        assert_eq!(cfg.name, "legacy");
        assert_eq!(cfg.preferred_provider_id, None);
        assert_eq!(cfg.preferred_model_id, None);
        assert_eq!(
            cfg.transport_preference,
            crate::tunnel::known_runtimes::TransportPreference::Auto
        );
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
            preferred_provider_id: None,
            preferred_model_id: None,
            transport_preference: crate::tunnel::known_runtimes::TransportPreference::Tunnel,
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            serialized.contains("transport_preference = \"tunnel\""),
            "got: {serialized}"
        );

        let back: PrincipalConfig = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(
            back.transport_preference,
            crate::tunnel::known_runtimes::TransportPreference::Tunnel
        );
    }

    /// The new fields must round-trip losslessly through serde so the
    /// `peko principal set-provider` write path can persist them.
    #[test]
    fn principal_config_provider_fields_roundtrip() {
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
            preferred_provider_id: Some("ollama".into()),
            preferred_model_id: Some("llama3.1".into()),
            transport_preference: crate::tunnel::known_runtimes::TransportPreference::Direct,
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            serialized.contains("preferred_provider_id = \"ollama\""),
            "got: {serialized}"
        );
        assert!(
            serialized.contains("preferred_model_id = \"llama3.1\""),
            "got: {serialized}"
        );
        assert!(
            serialized.contains("transport_preference = \"direct\""),
            "got: {serialized}"
        );

        let back: PrincipalConfig = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(back.preferred_provider_id.as_deref(), Some("ollama"));
        assert_eq!(back.preferred_model_id.as_deref(), Some("llama3.1"));
        assert_eq!(
            back.transport_preference,
            crate::tunnel::known_runtimes::TransportPreference::Direct
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
            preferred_provider_id: None,
            preferred_model_id: None,
            transport_preference: Default::default(),
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        assert!(
            !serialized.contains("preferred_provider_id"),
            "absent hint leaked into TOML: {serialized}"
        );
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
            preferred_provider_id: None,
            preferred_model_id: None,
            transport_preference: Default::default(),
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
            preferred_provider_id: None,
            preferred_model_id: None,
            transport_preference: Default::default(),
        };
        assert!(cfg.capabilities.is_granted(&"tool:Read".into()));
        assert!(cfg
            .capabilities
            .is_granted(&"agent:agency-agents/writer".into()));
        assert!(!cfg.capabilities.is_granted(&"agent:other/writer".into()));
    }
}
