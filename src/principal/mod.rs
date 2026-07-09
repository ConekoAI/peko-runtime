pub mod agent_prompt;
pub mod agent_runner;
pub mod agent_state;
pub mod config;
pub mod context;
pub mod extension_state;
pub mod factory;
pub mod manager;
pub mod memory;
pub mod router;
pub mod routers;
pub mod slash;

pub use agent_prompt::{load_agent_prompt, AgentPrompt, AgentPromptFrontmatter};
pub use agent_runner::build_agent_config;
pub use agent_state::{AgentState, AgentStateGuard, AgentStateRegistry};
pub use config::{
    AllowedExtensions, AuditLevel, ConsolidationConfig, DelegationGrant, MemoryTier,
    PrincipalConfig, PrincipalDID, PrincipalGovernanceConfig, PrincipalIdentityConfig,
    PrincipalIntentConfig, PrincipalMemoryConfig, PrincipalRoutingConfig, TtlPolicy,
};
pub use context::PrincipalContext;
pub use extension_state::{ExtensionState, ExtensionStateGuard, ExtensionStateRegistry};
pub use factory::{
    DefaultPrincipalMemory, DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
    PrincipalMemoryFactory, PrincipalRouterFactory,
};
pub use manager::{PrincipalManager, PrincipalManagerError};
pub use memory::{MemoryError, PrincipalMemory, SessionArtifact};
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

impl std::fmt::Display for PrincipalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
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

    /// The extensions allowed for this Principal.
    pub async fn allowed_extensions(&self) -> AllowedExtensions {
        self.config.read().await.allowed_extensions.clone()
    }

    /// The exposure level for this Principal.
    pub async fn exposure(&self) -> crate::tunnel::protocol::InstanceExposure {
        self.config.read().await.exposure.clone()
    }

    /// Build a lightweight summary for list/show IPC responses.
    ///
    /// The principal-as-single-actor migration made `Principal` the
    /// only actor in the system. IPC list/show surfaces that used to
    /// return per-agent summaries (e.g. `AgentList`) now return
    /// `PrincipalSummary` so the CLI sees the on-disk truth instead
    /// of legacy mirror directories.
    pub async fn summary(&self) -> PrincipalSummary {
        let config = self.config.read().await;
        PrincipalSummary {
            name: config.name.clone(),
            did: self.did().await,
            owner: config.owner.clone(),
            description: config.identity.description.clone(),
            exposure: config.exposure.clone(),
            status: config.status.clone(),
            allowed_extensions: config.allowed_extensions.clone(),
            agent_prompt_count: self.agent_prompts.len(),
            workspace_path: self.workspace_path.display().to_string(),
        }
    }
}

/// Lightweight projection of a `Principal` for IPC list/show responses.
///
/// The runtime formerly returned `Vec<AgentSummary>` from its
/// `AgentList` IPC; after the principal-as-single-actor migration the
/// actor surface is `Principal`. `PrincipalSummary` is the canonical
/// wire-form shape for the post-migration `PrincipalList` /
/// `PrincipalGet` IPC variants — see audit finding C1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalSummary {
    pub name: String,
    pub did: PrincipalDID,
    pub owner: crate::auth::Subject,
    pub description: Option<String>,
    pub exposure: crate::tunnel::protocol::InstanceExposure,
    pub status: Option<crate::tunnel::protocol::InstanceStatus>,
    pub allowed_extensions: AllowedExtensions,
    pub agent_prompt_count: usize,
    pub workspace_path: String,
}

fn synthetic_local_did(name: &str, id: &PrincipalId) -> PrincipalDID {
    PrincipalDID(format!("did:peko:local:{name}:{}", id.0))
}
