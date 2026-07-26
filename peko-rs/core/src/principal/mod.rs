// Phase F3 (foldback): 11 files moved back into root from the
// `peko-principal` sat (deleted). The sat had no clean downstream
// consumers — only root + cli referenced `crate::principal::*`, and
// the leaf crates (tools-builtin, session, providers, etc.) had ZERO
// direct deps. Reverse the half-extracted mess.
//
// Modules lifted back into root (under `crate::principal::*`):
//   config, peer, memory, agent_prompt, capability_evaluator — pure
//     data types + DTOs (F14.c.1 surface)
//   runtime::{mod, builtin_tools, output_format} — shared const
//     lists + `OutputFormat` enum (F14.c.2a)
//   slash::extension_row — IPC re-type
//
// The runtime-coupled files (manager, context, agent_runner,
// routers, slash dispatcher impl) continue to live alongside the
// `Principal` struct definition here.

pub mod agent_prompt;
pub mod agent_runner;
pub mod capability_evaluator;
pub mod config;
pub mod context;
pub mod extension_store;
pub mod factory;
pub mod manager;
pub mod memory;
pub mod peer;
pub mod router;
pub mod routers;
pub mod runtime;
pub mod slash;

pub use agent_prompt::{load_agent_prompt, AgentPrompt, AgentPromptFrontmatter};
pub use agent_runner::build_agent_config;
pub use capability_evaluator::CapabilityEvaluator;
pub use config::{
    ArtifactKind, AuditLevel, ConsolidationConfig, DelegationGrant, MemoryTier, PrincipalConfig,
    PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
    PrincipalMemoryConfig, PrincipalRoutingConfig, Status, TransportPreference, TtlPolicy,
};
pub use context::PrincipalContext;
pub use extension_store::{
    capability_kind_for_extension_type, ExtensionCatalog, ExtensionCatalogItem,
};
pub use factory::{
    DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalMemoryFactory,
    PrincipalRouterFactory,
};
pub use manager::{PrincipalManager, PrincipalManagerError};
pub use memory::{DefaultPrincipalMemory, MemoryError, PrincipalMemory, SessionArtifact};
pub use peer::{Peer, PeerConfig, PeerError, PeerRegistry};
pub use router::{
    AgentPromptSummary, ChannelContext, ChannelKind, ContextInjection, ContextInjectionKind,
    PrincipalRouter, RouteDecision, RouterContext, RouterError,
};
pub use runtime::{builtin_tools, OutputFormat};
pub use slash::extension_row::PrincipalExtensionRow;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

// Actor identifiers are sourced from `subject` (the canonical actor module) so
// `principal` does not own them and `agents` can reach them without depending
// on `principal` (F3 cycle break).
use peko_subject::{PrincipalDID, PrincipalId};
// Phase 14.c.1/14.c.2a: pure-deps types lifted into `peko-principal`
// (config, peer, memory, factory, agent_prompt, capability_evaluator,
// extension_store). The runtime-coupled files in root that compose
// a `Principal` (manager, context, agent_runner, routers, slash)
// continue to live alongside the `Principal` struct definition here,
// so they reach for the same DTOs.
use peko_quota::QuotaMeter;

/// Runtime representation of a Principal.
pub struct Principal {
    pub id: PrincipalId,
    pub config: RwLock<PrincipalConfig>,
    pub workspace_path: PathBuf,
    pub memory: Arc<dyn PrincipalMemory>,
    pub router: Arc<dyn PrincipalRouter>,
    pub agent_prompts: HashMap<String, AgentPrompt>,
    /// F18 quota meter. Always present; for unquota'd principals the
    /// underlying `QuotaConfig` has `None` for every limit, so every
    /// charge is free. Cloning the `Arc` shares state across the
    /// engine loop, compactor, subagent executor, and CLI status
    /// reads.
    pub quota_meter: Arc<QuotaMeter>,
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

    /// The capabilities for this Principal.
    pub async fn capabilities(&self) -> peko_extension_api::Capabilities {
        self.config.read().await.capabilities.clone()
    }

    /// The exposure level for this Principal.
    pub async fn exposure(&self) -> peko_auth::Exposure {
        self.config.read().await.exposure
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
            preferred_model_id: config.preferred_model_id.clone(),
            capabilities: config.capabilities.clone(),
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
    pub owner: peko_auth::Subject,
    pub description: Option<String>,
    pub exposure: peko_auth::Exposure,
    pub status: Option<crate::principal::config::Status>,
    pub preferred_model_id: Option<String>,
    pub capabilities: peko_extension_api::Capabilities,
    pub agent_prompt_count: usize,
    pub workspace_path: String,
}

fn synthetic_local_did(name: &str, id: &PrincipalId) -> PrincipalDID {
    PrincipalDID(format!("did:peko:local:{name}:{}", id.0))
}

// Same-runtime offline `principal_send` integration test (gated, opt-in via
// `--features test-utils`). Originally `tests/principal_send_offline.rs`;
// moved inline as part of F9 so `peko::daemon::AppState` exposure is no
// longer required just to host this test.
#[cfg(all(test, feature = "test-utils"))]
mod tests;
