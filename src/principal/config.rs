use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::auth::{Permission, PermissionGrant};
use crate::tunnel::protocol::InstanceExposure;

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

    #[serde(default)]
    pub capabilities: PrincipalCapabilities,

    /// Network exposure level for this Principal.
    #[serde(default)]
    pub exposure: InstanceExposure,

    /// Explicit permission grants on this Principal.
    #[serde(default)]
    pub permissions: Vec<PermissionGrant>,
}

/// Thin wrapper around a DID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalDID(pub String);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalCapabilities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcps: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
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
    /// Optional path to a custom supervisor agent prompt Markdown file.
    /// If omitted, the runtime uses the built-in supervisor prompt.
    #[serde(default)]
    pub supervisor_prompt: Option<PathBuf>,

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
            supervisor_prompt: None,
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

/// Role of an agent prompt within a Principal.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    #[default]
    Default,
    Specialist,
    Supervisor,
}
