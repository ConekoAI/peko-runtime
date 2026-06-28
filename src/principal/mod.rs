pub mod agent_prompt;
pub mod agent_runner;
pub mod config;
pub mod factory;
pub mod manager;
pub mod memory;
pub mod router;
pub mod routers;

pub use agent_prompt::{load_agent_prompt, AgentPrompt, AgentPromptFrontmatter};
pub use agent_runner::build_agent_config;
pub use config::{
    AgentRole, AuditLevel, ConsolidationConfig, DelegationGrant, MemoryTier, PrincipalCapabilities,
    PrincipalConfig, PrincipalDID, PrincipalGovernanceConfig, PrincipalIdentityConfig,
    PrincipalIntentConfig, PrincipalMemoryConfig, PrincipalRoutingConfig, TtlPolicy,
};
pub use factory::{
    DefaultPrincipalMemory, DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
    PrincipalMemoryFactory, PrincipalRouterFactory,
};
pub use manager::{parse_agent_role, PrincipalManager, PrincipalManagerError};
pub use memory::{
    Artifact, CompactSummary, FileArtifact, MemoryArtifact, MemoryError, PrincipalMemory,
    SessionArtifact, TodoArtifact,
};
pub use router::{
    AgentPromptSummary, ChannelContext, ChannelKind, ContextInjection, ContextInjectionKind,
    PrincipalRouter, RouteDecision, RouterContext, RouterError,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Newtype wrapper for a stable principal identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalId(pub String);

impl PrincipalId {
    pub fn generate() -> Self {
        Self(format!("prin_{}", uuid::Uuid::new_v4().simple()))
    }
}

/// Runtime representation of a Principal.
pub struct Principal {
    pub id: PrincipalId,
    pub config: RwLock<PrincipalConfig>,
    pub workspace_path: PathBuf,
    pub memory: Arc<dyn PrincipalMemory>,
    pub router: Arc<dyn PrincipalRouter>,
    pub agent_prompts: HashMap<String, AgentPrompt>,
}

impl Principal {
    /// The stable DID. New Principals always generate a real identity at
    /// creation time, so this normally returns the configured DID.
    pub async fn did(&self) -> PrincipalDID {
        let config = self.config.read().await;
        config.did.clone().unwrap_or_else(|| {
            // Fallback for Principals created before identity generation was
            // wired. The name is immutable, so this is stable enough.
            synthetic_local_did(&config.name, &self.id)
        })
    }

    /// The Principal name.
    pub async fn name(&self) -> String {
        self.config.read().await.name.clone()
    }

    /// The capabilities (tools, skills, MCPs) available to this Principal.
    pub async fn capabilities(&self) -> PrincipalCapabilities {
        self.config.read().await.capabilities.clone()
    }

    /// The exposure level for this Principal.
    pub async fn exposure(&self) -> crate::tunnel::protocol::InstanceExposure {
        self.config.read().await.exposure.clone()
    }
}

fn synthetic_local_did(name: &str, id: &PrincipalId) -> PrincipalDID {
    PrincipalDID(format!("did:peko:local:{name}:{}", id.0))
}
