// Phase F3 (foldback): 11 files moved back into root from the
// `peko-principal` sat (deleted). The sat had no clean downstream
// consumers — only root + cli referenced `crate::principal::*`, and
// the leaf crates (tools-builtin, session, providers, etc.) had ZERO
// direct deps. Reverse the half-extracted mess.
//
// Modules lifted back into root (under `crate::principal::*`):
//   config, peer, memory, agent_prompt, catalog — pure
//     data types + DTOs (F14.c.1 surface; `capability_evaluator`
//     was deleted in ADR-066 P2)
//   runtime::{mod, builtin_tools} — shared const lists (F14.c.2a;
//     `OutputFormat` enum was part of this lift and has since been
//     retired post-slash-removal)
//
// The runtime-coupled files (manager, context, agent_runner,
// routers) continue to live alongside the `Principal` struct
// definition here.

pub mod acting_for_prompt;
pub mod agent_prompt;
pub mod agent_runner;
pub(crate) mod attachments;
pub mod catalog;
pub(crate) mod channel_digest;
pub(crate) mod child_turns;
pub mod config;
pub mod contacts;
pub mod context;
pub(crate) mod conversation;
pub mod default_nodes;
pub mod factory;
pub mod genesis;
pub mod identity_prompt;
pub mod kb;
pub mod manager;
pub mod memory;
pub mod messenger;
pub mod parking;
pub mod peer;
pub mod peer_children;
pub(crate) mod peer_dm;
pub(crate) mod plan_prompt;
pub mod policy_gate;
pub(crate) mod quiet_tick;
pub mod router;
pub mod routers;
pub mod run_failure;
// Phase 4 of `feature/multi-model-subagents` (plan:
// `/Users/rlsn/.claude/plans/goofy-humming-wall.md`).
// `seen_models.json` persistence + the `PrincipalContext::mark_model_seen`
// helper that powers the first-use-per-model audit-warning UX.
pub mod seen_models;
pub(crate) mod trust;

pub use agent_prompt::{load_agent_prompt, AgentPrompt, AgentPromptFrontmatter};
pub use agent_runner::build_agent_config;
pub use catalog::{CatalogEntry, PrincipalCatalog};
pub use config::{
    BootState, ChildDeclaration, PrincipalConfig, PrincipalGovernanceConfig,
    PrincipalIdentityConfig, PrincipalIntentConfig, PrincipalRoutingConfig, Status,
};
pub use factory::{
    DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalMemoryFactory,
    PrincipalRouterFactory,
};
pub use manager::{PrincipalManager, PrincipalManagerError};
pub use memory::{DefaultPrincipalMemory, MemoryError, PrincipalMemory, SessionArtifact};
pub use messenger::{
    global_messenger, set_global_messenger, PeerMessenger, PrincipalPeerMessenger,
};
pub use peer::{Peer, PeerConfig, PeerError, PeerRegistry};
pub use router::{
    AgentPromptSummary, ChannelContext, ChannelKind, ContextInjection, ContextInjectionKind,
    PrincipalRouter, RouteDecision, RouterContext, RouterError,
};

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
// (config, peer, memory, factory, agent_prompt,
// catalog — formerly `extension_store`). The runtime-coupled files in root that compose
// a `Principal` (manager, context, agent_runner, routers) continue
// to live alongside the `Principal` struct definition here, so they
// reach for the same DTOs.
use peko_quota::QuotaMeter;

/// Runtime representation of a Principal.
pub struct Principal {
    pub mcp: Arc<crate::extensions::mcp::service::PrincipalMcpRuntime>,
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
    /// Plan DAG port (Phase 12+, PR #1 of the wiring sequence).
    /// Per-principal storage of multi-step plans. Held as
    /// `Arc<dyn PlanPort>` (NOT concrete `Arc<PlanStorage>`) so
    /// future impls — in-memory for tests, network-backed for
    /// distributed deployments — slot in without rewriting field
    /// types. Constructed lazily when the principal is
    /// created/loaded, from
    /// `path_resolver.principal_layout(name).local.plans_dir`.
    ///
    /// Reached via `principal.plan_port`; no separate `AppState`
    /// plumbing needed (plans are per-principal, not runtime-global).
    pub plan_port: Arc<dyn peko_plan::PlanPort>,
    /// `principal.toml` as it was (modified time, length) when `config`
    /// last matched it. [`Self::reload_config_if_changed`] re-reads the
    /// file only when this differs.
    config_file_stamp: std::sync::Mutex<Option<ConfigFileStamp>>,
    /// The last failed run of each of its sessions (ADR-089 D5): shown
    /// beside the conversation, never written into it.
    pub run_failures: run_failure::RunFailures,
    /// Its runs parked by a disruption, waiting to continue (ADR-089
    /// D2–D4); saved in `<local>/parked.json`.
    pub parking: parking::Parking,
}

/// What [`Principal::reload_config_if_changed`] compares to notice an
/// edit to `principal.toml`.
type ConfigFileStamp = (std::time::SystemTime, u64);

fn config_file_stamp(path: &std::path::Path) -> Option<ConfigFileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl Principal {
    /// The stamp of `<workspace>/principal.toml` right now, for the
    /// constructor: the config being installed is the one on disk.
    fn current_config_file_stamp(
        workspace_path: &std::path::Path,
    ) -> std::sync::Mutex<Option<ConfigFileStamp>> {
        std::sync::Mutex::new(config_file_stamp(&workspace_path.join("principal.toml")))
    }

    /// Adopt hand edits to `principal.toml` while the daemon runs: when
    /// the file changed since `config` last matched it, read it again.
    /// Turn builders call this first, so an edit applies to the next
    /// turn without a restart.
    ///
    /// The identity fields (`name`, `id`, `did`, `actor_key`, `[owner]`)
    /// keep their loaded values: runtime state is keyed by them. A file
    /// that no longer parses keeps the loaded config and is reported
    /// once per edit.
    pub async fn reload_config_if_changed(&self) {
        let path = self.workspace_path.join("principal.toml");
        let Some(stamp) = config_file_stamp(&path) else {
            return;
        };
        {
            let mut seen = self
                .config_file_stamp
                .lock()
                .expect("config_file_stamp lock poisoned");
            if *seen == Some(stamp) {
                return;
            }
            *seen = Some(stamp);
        }
        let parsed = match tokio::fs::read_to_string(&path).await {
            Ok(text) => toml::from_str::<PrincipalConfig>(&text).map_err(anyhow::Error::from),
            Err(e) => Err(e.into()),
        };
        let mut edited = match parsed {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(
                    principal = %self.id,
                    path = %path.display(),
                    "principal.toml changed but cannot be read; keeping the loaded config: {e:#}"
                );
                return;
            }
        };
        let model = {
            let mut config = self.config.write().await;
            edited.name = config.name.clone();
            edited.id = config.id.clone();
            edited.did = config.did.clone();
            edited.actor_key = config.actor_key.clone();
            edited.owner = config.owner.clone();
            *config = edited;
            config.preferred_model_id.clone()
        };
        self.mcp.set_model(model);
    }

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
    /// The summary with its Hub ID from the actor registry (ADR-081 §3).
    pub async fn summary_in(&self, manager: &manager::PrincipalManager) -> PrincipalSummary {
        let mut summary = self.summary().await;
        summary.hub_id = manager.hub_id_of(self).await;
        summary
    }

    pub async fn summary(&self) -> PrincipalSummary {
        let config = self.config.read().await;
        PrincipalSummary {
            local_id: self.id.0.clone(),
            // Filled from the actor registry by callers that have it.
            hub_id: None,
            name: config.name.clone(),
            did: self.did().await,
            owner: config.owner.clone(),
            description: config.identity.description.clone(),
            avatar: config.identity.avatar.clone(),
            exposure: config.exposure.clone(),
            status: config.status.clone(),
            preferred_model_id: config.preferred_model_id.clone(),
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
    pub local_id: String,
    pub hub_id: Option<String>,
    pub name: String,
    pub did: PrincipalDID,
    pub owner: peko_auth::ActorRef,
    pub description: Option<String>,
    /// The peko's avatar: an emoji, or a short monogram
    /// (`identity.avatar`, see [`config::validate_avatar`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub exposure: peko_auth::Exposure,
    pub status: Option<crate::principal::config::Status>,
    pub preferred_model_id: Option<String>,
    pub agent_prompt_count: usize,
    pub workspace_path: String,
}

fn synthetic_local_did(name: &str, id: &PrincipalId) -> PrincipalDID {
    PrincipalDID(format!("did:peko:local:{name}:{}", id.0))
}

// Same-runtime `ChannelSend` principal-branch tests over real DM channels.
#[cfg(test)]
mod tests;
