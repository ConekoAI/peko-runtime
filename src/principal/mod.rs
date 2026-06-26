pub mod agent_prompt;
pub mod agent_runner;
pub mod config;
pub mod factory;
pub mod manager;
pub mod memory;
pub mod router;
pub mod routers;

pub use agent_prompt::{load_agent_prompt, AgentPrompt, AgentPromptFrontmatter};
pub use agent_runner::{build_agent_config, run_agent_prompt};
pub use config::{
    AgentRole, AuditLevel, ConsolidationConfig, DelegationGrant, MemoryTier, PrincipalAgentRef,
    PrincipalCapabilities, PrincipalConfig, PrincipalDID, PrincipalGovernanceConfig,
    PrincipalIdentityConfig, PrincipalIntentConfig, PrincipalMemoryConfig, PrincipalRoutingConfig,
    RoutingStrategy, TtlPolicy,
};
pub use factory::{
    DefaultPrincipalMemory, DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
    PrincipalMemoryFactory, PrincipalRouterFactory,
};
pub use manager::{PrincipalManager, PrincipalManagerError};
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
    pub config: PrincipalConfig,
    pub workspace_path: PathBuf,
    pub memory: Arc<dyn PrincipalMemory>,
    pub router: Arc<dyn PrincipalRouter>,
    pub agent_prompts: HashMap<String, AgentPrompt>,
}

impl Principal {
    /// The stable DID, falling back to a synthetic local DID if not configured.
    pub fn did(&self) -> PrincipalDID {
        self.config
            .did
            .clone()
            .unwrap_or_else(|| synthetic_local_did(&self.config.name, &self.id))
    }

    /// Resolve a registered agent prompt by local name.
    pub fn agent_prompt(&self,
        name: &str,
    ) -> Option<&AgentPrompt> {
        self.agent_prompts.get(name)
    }

    /// The capabilities (tools, skills, MCPs) available to this Principal.
    pub fn capabilities(&self) -> &PrincipalCapabilities {
        &self.config.capabilities
    }
}

fn synthetic_local_did(name: &str, id: &PrincipalId) -> PrincipalDID {
    PrincipalDID(format!("did:peko:local:{name}:{}", id.0))
}
