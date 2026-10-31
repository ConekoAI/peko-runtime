use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{
    factory::{PrincipalMemoryFactory, PrincipalRouterFactory},
    router::{ChannelContext, ChannelKind, RouteDecision, RouterContext, RouterError},
    Principal, PrincipalId,
};
use crate::common::paths::PathResolver;
use crate::extensions::role::RoleAdapter;
use crate::principal::agent_prompt::load_agent_prompt;
use crate::principal::child_turns::PeerChildIngress;
use crate::principal::peer_dm::{post_peer_dm_inbound_attached, post_peer_dm_reply};
use crate::principal::AgentPrompt;
use crate::principal::PrincipalConfig;
use peko_auth::ownership::{check_permission, Permission, Resource};
use peko_auth::ActorRef;
use peko_observability::Observability;
use peko_providers::LlmResolver;
use peko_session::InboxRegistry;
use peko_session::RunPermitGuard;
use peko_session::SteeringMessage;
use peko_subject::PrincipalDID;

/// Error type for PrincipalManager operations.
#[derive(Debug, thiserror::Error)]
pub enum PrincipalManagerError {
    #[error("principal not found: {0}")]
    NotFound(String),
    #[error("principal already exists: {0}")]
    AlreadyExists(String),
    #[error("agent prompt not found: {0}")]
    AgentPromptNotFound(String),
    #[error("invalid route decision: {0}")]
    InvalidDecision(String),
    #[error("router error: {0}")]
    RouterError(#[from] RouterError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config error: {0}")]
    Config(String),
    #[error("identity error: {0}")]
    Identity(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

/// B8c.4: outcome of [`PrincipalManager::drive_principal_ingress`].
///
/// The two principal ingress sites (`receive_streaming`,
/// `handle_principal_send`) each format the outcome
/// differently — `PrincipalResponse::text(...)` vs `PrincipalSent` vs
/// `(success: bool, error: Option<String>)` — but the upstream
/// sequence (resolve child + DM channel, post inbound, acquire or
/// queue) is identical. The helper hands back this tagged enum; the
/// caller maps `Ready` / `Queued` / `Failed` into its own wire format.
pub enum IngressOutcome {
    /// Run permit acquired; caller drives the turn. `permit` is held
    /// for the caller scope; dropping it before the turn ends lets a
    /// second ingress acquire its own run slot.
    Ready {
        child_id: String,
        dm_channel: Option<peko_channel::ChannelId>,
        permit: RunPermitGuard,
    },
    /// No run permit (a run is already active for this child
    /// session). The message was pushed to the child's inbox as a
    /// `SteeringMessage`; caller emits a "Queued..." notice.
    Queued { child_id: String },
}

/// B8c.4: dispatch mode for [`PrincipalManager::drive_principal_ingress`].
pub enum IngressMode {
    /// Acquire a run permit; queue on conflict. Used by
    /// `receive_streaming` and the principal IPC ingress.
    Interactive,
}

/// B8c.4: failure modes from [`PrincipalManager::drive_principal_ingress`].
/// Callers add their own wire-format prefix on `Post` (e.g. "Failed
/// to persist chat input:" vs "Failed to persist steered chat input:").
#[derive(Debug)]
pub enum IngressError {
    /// `peer_child_turns_for` or `ensure_child_ingress` failed.
    Resolve(PrincipalManagerError),
    /// `post_dm_inbound` failed — the peer's DM channel could not
    /// be written. The ingress is rejected so the consumer cannot
    /// believe they sent something that never landed.
    Post(anyhow::Error),
}

impl std::fmt::Display for IngressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{e}"),
            Self::Post(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for IngressError {}

/// Owns all Principals in a runtime.
pub struct PrincipalManager {
    /// The manager's own `Arc`, once [`Self::install_policy_gate`] binds
    /// it: owner approval commands replay calls through it.
    self_ref: std::sync::OnceLock<std::sync::Weak<PrincipalManager>>,
    /// This runtime's identity DID (ADR-081 D2): the runtime every local
    /// route of this manager's principals and contacts belongs to.
    runtime_did: std::sync::OnceLock<String>,
    tooling: Arc<crate::tools::runtime::ToolingRuntime>,
    principals: RwLock<HashMap<PrincipalId, Arc<Principal>>>,
    principals_by_name: RwLock<HashMap<String, PrincipalId>>,
    path_resolver: PathResolver,
    memory_factory: Arc<dyn PrincipalMemoryFactory>,
    router_factory: Arc<dyn PrincipalRouterFactory>,
    resolver: Option<Arc<LlmResolver>>,
    /// Shared inbox registry used by root agents and the Principal
    /// boundary to queue steering messages for sessions that already have
    /// a run in flight.
    ///
    /// This is a required constructor input (no private default): with
    /// two registries, the per-session run permit lives in two
    /// independent permit spaces, so a cron/tunnel turn via `receive`
    /// runs fully concurrent with an in-flight CLI turn on the same
    /// session JSONL — interleaved appends then break
    /// tool_call/tool_result adjacency and brick the session for
    /// Anthropic-style providers (2026-08-07 field test, finding N1).
    /// The daemon MUST wire its `AppState` registry in; tests and
    /// offline CLI contexts build a standalone one.
    inbox_registry: Arc<InboxRegistry>,
    /// Per-principal lock guarding first-time session creation/open so
    /// concurrent peers do not race on shared metadata/index writes.
    session_creation_locks: tokio::sync::RwLock<HashMap<PrincipalId, Arc<tokio::sync::Mutex<()>>>>,
    /// Optional observability hub. Threaded into `RouterContext` so the root
    /// agent and subagent spawns can emit audit events.
    observability: Option<Arc<Observability>>,
    /// F20: optional per-peer quota registry. When `Some`, callers can
    /// resolve a peer's quota meter via
    /// [`PrincipalManager::get_or_create_peer`] and stack it alongside
    /// the principal's meter. `None` for tests / contexts that don't
    /// have a daemon-managed `PeerRegistry`.
    peer_registry: Option<Arc<crate::principal::peer::PeerRegistry>>,
    /// Sprint 2 Phase 7: per-principal peer-child turn bundles
    /// (`PeerChildTurns`), built lazily on first peer ingress and
    /// cached for the principal's lifetime. The bundle owns the
    /// persona-carrying `SubagentExecutor` + the session manager the
    /// principal's peer children live in. The persona and model are
    /// re-read at every turn (`PeerChildTurns::turn_executor`), so
    /// caching the bundle never pins them. Build failures (no
    /// resolvable model) surface per call; nothing is cached on
    /// error so a config fix takes effect on the next message.
    peer_child_turns: tokio::sync::RwLock<
        HashMap<PrincipalId, Arc<crate::principal::child_turns::PeerChildTurns>>,
    >,
    /// Sprint 3 Phase 10: the daemon-global channel port threaded into
    /// `PeerChildTurns` so peer ingress auto-provisions the peer's DM
    /// channel. `None` (tests / offline CLI) disables provisioning —
    /// session behavior unchanged.
    channel_port: Option<Arc<dyn peko_channel::ChannelPort>>,
    /// Activity record for `Idle` schedules (`peko_cron::idle`): people
    /// talking to a principal mark it active. `None` outside the daemon.
    idle_detector: Option<Arc<peko_cron::IdleDetector>>,
    /// Phase 10: kickoff hook fired when a peer's DM channel is
    /// freshly created (the daemon installs a closure forwarding to
    /// `ChannelBindingSupervisor::ensure_subscriber` AFTER the
    /// supervisor is built — the supervisor needs the `Arc` of this
    /// manager first, so this is a post-construction install behind
    /// interior mutability, not a builder arg).
    dm_subscriber_hook: std::sync::RwLock<Option<crate::principal::peer_dm::PeerDmSubscriberHook>>,
    /// Optional shared vault. Without one, identity creation opens the configured runtime vault.
    identity_vault: Option<Arc<crate::common::vault::Vault>>,
}

impl PrincipalManager {
    pub fn new(
        memory_factory: Arc<dyn PrincipalMemoryFactory>,
        router_factory: Arc<dyn PrincipalRouterFactory>,
        inbox_registry: Arc<InboxRegistry>,
    ) -> Self {
        Self::with_path_resolver(
            PathResolver::new(),
            memory_factory,
            router_factory,
            inbox_registry,
        )
    }

    pub fn with_path_resolver(
        path_resolver: PathResolver,
        memory_factory: Arc<dyn PrincipalMemoryFactory>,
        router_factory: Arc<dyn PrincipalRouterFactory>,
        inbox_registry: Arc<InboxRegistry>,
    ) -> Self {
        let channel_port: Arc<dyn peko_channel::ChannelPort> = Arc::new(
            peko_channel::ChannelStore::new(peko_channel::ChannelConfig {
                runtime_dir: path_resolver.runtime_dir(),
                shared_dir: None,
            }),
        );
        Self {
            self_ref: std::sync::OnceLock::new(),
            runtime_did: std::sync::OnceLock::new(),
            tooling: crate::tools::runtime::ToolingRuntime::standalone(),
            principals: RwLock::new(HashMap::new()),
            principals_by_name: RwLock::new(HashMap::new()),
            path_resolver,
            memory_factory,
            router_factory,
            resolver: None,
            inbox_registry,
            session_creation_locks: tokio::sync::RwLock::new(HashMap::new()),
            observability: None,
            peer_registry: None,
            peer_child_turns: tokio::sync::RwLock::new(HashMap::new()),
            channel_port: Some(channel_port),
            idle_detector: None,
            dm_subscriber_hook: std::sync::RwLock::new(None),
            identity_vault: None,
        }
    }

    pub fn tooling(&self) -> Arc<crate::tools::runtime::ToolingRuntime> {
        Arc::clone(&self.tooling)
    }

    /// Bind the ADR-078 D7 policy gate onto this manager's tooling: the
    /// reviewer for non-owner runs and the owner's approval desk.
    pub fn install_policy_gate(self: &Arc<Self>) {
        let _ = self.self_ref.set(Arc::downgrade(self));
        self.tooling.dispatcher().set_policy_gate(Arc::new(
            crate::principal::policy_gate::PrincipalPolicyGate::new(Arc::downgrade(self)),
        ));
    }

    pub fn with_tooling(mut self, tooling: Arc<crate::tools::runtime::ToolingRuntime>) -> Self {
        self.tooling = tooling;
        self
    }

    pub fn with_resolver(mut self, resolver: Arc<LlmResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// The shared LLM resolver, when bound (daemon path). Used by
    /// out-of-band turn builders (cron cold-start Agent registration)
    /// that need model resolution outside a live turn.
    /// The observability hub, when one is attached.
    pub(crate) fn observability(&self) -> Option<Arc<Observability>> {
        self.observability.clone()
    }

    pub fn llm_resolver(&self) -> Option<Arc<LlmResolver>> {
        self.resolver.clone()
    }

    /// Attach an observability hub. When present, it is threaded into every
    /// `RouterContext` so the root agent and subagent spawns can emit audit
    /// events.
    #[must_use]
    pub fn with_observability(mut self, observability: Arc<Observability>) -> Self {
        self.observability = Some(observability);
        self
    }

    /// F20: attach a per-peer quota registry. When attached,
    /// [`PrincipalManager::get_or_create_peer`] resolves a peer's
    /// `Arc<QuotaMeter>` from the registry, materializing the peer
    /// directory + state file on first contact.
    #[must_use]
    pub fn with_peer_registry(
        mut self,
        peer_registry: Arc<crate::principal::peer::PeerRegistry>,
    ) -> Self {
        self.peer_registry = Some(peer_registry);
        self
    }

    /// Attach the runtime vault for principal signing-key custody.
    #[must_use]
    pub fn with_identity_vault(mut self, vault: Arc<crate::common::vault::Vault>) -> Self {
        self.identity_vault = Some(vault);
        self
    }

    /// F20: resolve a peer's quota meter. Returns `Some(meter)` when a
    /// peer registry is attached and the peer exists (or is freshly
    /// materialized); `None` when no registry is attached (tests /
    /// non-daemon contexts). `peer_id` must be a validated peer
    /// identifier — see [`crate::principal::peer::validate_peer_id`]
    /// for the rules.
    pub async fn get_or_create_peer(
        &self,
        peer_id: &str,
    ) -> Option<Arc<crate::principal::peer::Peer>> {
        let registry = self.peer_registry.as_ref()?;
        match registry.get_or_create(peer_id, chrono::Utc::now()).await {
            Ok(peer) => Some(peer),
            Err(e) => {
                tracing::warn!(
                    peer_id = %peer_id,
                    error = %e,
                    "get_or_create_peer failed; falling back to no peer meter"
                );
                None
            }
        }
    }

    /// F20: optional reference to the attached peer registry. `None`
    /// when no registry was attached at construction time. Used by the
    /// daemon's `PeerHost` impl to expose the registry to IPC handlers.
    #[must_use]
    pub fn peer_registry(&self) -> Option<&Arc<crate::principal::peer::PeerRegistry>> {
        self.peer_registry.as_ref()
    }

    /// Sprint 3 Phase 10: attach the daemon-global channel port. When
    /// attached, peer ingress (`PeerChildTurns::ensure_child`)
    /// auto-provisions the peer's DM channel alongside its standing
    /// child session. `None` (the default) disables provisioning —
    /// the right choice for tests / non-daemon contexts.
    #[must_use]
    /// The principal's Hub ID, from the actor registry — the only source
    /// (ADR-081 §3). `None` while local-only or without a registry.
    pub async fn hub_id_of(&self, principal: &Principal) -> Option<String> {
        let port = self.channel_port()?;
        port.actor_info(&ActorRef::from(&principal.id)).await.hub_id
    }

    /// Bind this runtime's identity DID (ADR-081 D2).
    pub fn with_runtime_did(self, runtime_did: impl Into<String>) -> Self {
        let _ = self.runtime_did.set(runtime_did.into());
        self
    }

    /// This runtime's identity DID; `"local"` for managers outside a daemon
    /// (tests and offline CLI paths), whose routes never leave the process.
    #[must_use]
    pub fn runtime_did(&self) -> &str {
        self.runtime_did.get().map_or("local", String::as_str)
    }

    pub fn with_channel_port(mut self, channel_port: Arc<dyn peko_channel::ChannelPort>) -> Self {
        self.channel_port = Some(channel_port);
        self
    }

    /// Record people talking to principals as activity for `Idle`
    /// schedules (the daemon's persisted detector).
    pub fn with_idle_detector(mut self, detector: Arc<peko_cron::IdleDetector>) -> Self {
        self.idle_detector = Some(detector);
        self
    }

    /// Phase 10: install the DM-channel subscriber kickoff hook.
    /// Called by the daemon AFTER the `ChannelBindingSupervisor` is
    /// built (the supervisor needs this manager's `Arc` first).
    pub(crate) fn set_dm_subscriber_hook(
        &self,
        hook: crate::principal::peer_dm::PeerDmSubscriberHook,
    ) {
        *self
            .dm_subscriber_hook
            .write()
            .expect("dm_subscriber_hook lock poisoned") = Some(hook);
    }

    /// Start the live subscriber for a channel `principal` created or
    /// joined after boot (the hook the daemon installs; a no-op without
    /// one).
    pub(crate) fn ensure_channel_subscriber(
        &self,
        principal: &PrincipalId,
        channel: &peko_channel::ChannelId,
    ) {
        let hook = self
            .dm_subscriber_hook
            .read()
            .expect("dm_subscriber_hook lock poisoned")
            .clone();
        if let Some(hook) = hook {
            hook(principal.clone(), channel.clone());
        }
    }

    /// ADR-082 D2: `principal`'s group session for `channel`, created in
    /// the channel's compartment, with its binding recorded. Uses the
    /// session manager the group wake uses whenever turns can be built.
    pub(crate) async fn ensure_group_binding(
        &self,
        principal: &Arc<Principal>,
        channel: &peko_channel::ChannelId,
    ) -> anyhow::Result<String> {
        let port = self
            .channel_port()
            .ok_or_else(|| anyhow::anyhow!("no channel port is attached"))?;
        let (agent_name, sessions) = match self.peer_child_turns_for(principal).await {
            Ok(turns) => (
                turns.agent_name().to_string(),
                Arc::clone(turns.session_manager()),
            ),
            Err(_) => {
                let config = principal.config.read().await;
                let agent_name = crate::principal::child_turns::peer_child_session_agent_name(
                    &config,
                    &principal.workspace_path,
                );
                let sessions = crate::principal::child_turns::peer_child_session_manager(
                    principal,
                    &agent_name,
                    &config.owner,
                );
                (agent_name, sessions)
            }
        };
        crate::principal::peer_children::ensure_group_binding(
            &port,
            &principal.id,
            &agent_name,
            channel,
            &sessions,
        )
        .await
    }

    /// Create a new Principal from config, generate a real identity, and load
    /// its agent prompts.
    ///
    /// **Phase A.** The principal's `workspace_path` now points at the Shared
    /// tier root (`{config_dir}/principals/{name}/`), not the Local data
    /// dir. Local state (sessions, cron, quota meter state, caches) lives
    /// under `…/{name}/local/` and is reachable through `self.resolver`.
    /// Both tiers are created up front via `ensure_principal_dirs` so the
    /// runtime never has to mkdir mid-write.
    pub async fn create(
        &self,
        config: PrincipalConfig,
    ) -> Result<Arc<Principal>, PrincipalManagerError> {
        let name = config.name.clone();
        config.warn_deprecated_children(&name);
        {
            let by_name = self.principals_by_name.read().await;
            if by_name.contains_key(&name) {
                return Err(PrincipalManagerError::AlreadyExists(name));
            }
        }

        let id = PrincipalId::generate();
        let layout = self.path_resolver.principal_layout(&name);
        self.path_resolver.ensure_principal_dirs(&name)?;

        // ADR-064 follow-up: the root role prompt has no render-time
        // fallback — stamp the compiled-in default into
        // `roles/root.md` here (create-if-missing) so EVERY create
        // path (CLI provision, package import, tests) satisfies the
        // workspace-file contract before the router factory resolves
        // the root prompt and fails loudly on absence.
        crate::principal::genesis::seed_default_roles(&layout.shared.root)
            .map_err(|e| PrincipalManagerError::Config(format!("role seed failed: {e:#}")))?;

        // Generate and persist a real DID identity for this Principal.
        let mut config = config;
        let did = self.generate_identity(&name).await?;
        config.did = Some(PrincipalDID(did));
        config.actor_key = Some(uuid::Uuid::new_v4().to_string());
        // Persist the generated id so daemon restarts load the same
        // principal identity (peer DM channels are keyed by it).
        config.id = Some(id.clone());

        // ADR-054: stamp the genesis boot state. A config that arrives
        // with definition content (e.g. a `.peko` package import)
        // enters at `defined`; a bare create (the CLI writes a default
        // agent prompt but no identity/intent) enters at `provisioned`.
        // The daemon's genesis boot pass (`principal::genesis`) picks
        // the state up and advances it.
        config.set_boot_state(if config.has_definition() {
            crate::principal::config::BootState::Defined
        } else {
            crate::principal::config::BootState::Provisioned
        });

        // Persist principal.toml under the Shared root.
        self.persist_config(&layout.shared.root, &config).await?;

        let memory = self.memory_factory.create(&id, &layout.local.root).await;

        // Default session nodes (`/tmp`, `/trash`): seeded ONCE here at
        // principal creation as ordinary standing sessions parented at
        // the (still-dangling) trunk. Deliberately NOT ensured at boot
        // or root-run setup — a principal that removes them keeps them
        // removed (create-once semantics; see
        // `principal::default_nodes` module docs). Best-effort: failures
        // warn and continue, never blocking principal creation (the
        // `seen_models.json` tolerated-corruption precedent).
        {
            let seed_manager = peko_session::manager::SessionManager::new()
                .with_sessions_dir_internal(memory.sessions_dir());
            let seed_manager = Arc::new(tokio::sync::RwLock::new(seed_manager));
            match crate::principal::default_nodes::seed_default_nodes("root", &seed_manager).await {
                Ok(n) if n > 0 => {
                    tracing::info!("principal '{name}': seeded {n} default session node(s)")
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("principal '{name}': failed to seed default session nodes: {e}")
                }
            }
        }

        // F18: build the quota meter first so the router can capture
        // the same Arc. The meter is built before the router because
        // `RouterFactory::create` takes `Arc<QuotaMeter>` — both the
        // root router and the principal carry the same handle so an
        // `update_config` call that mutates `config.quota` only
        // needs to swap the meter on the principal.
        //
        // Phase A: quota state lives in the Local tier under `local/cron/`
        // alongside the cron schedule — quota is per-principal runtime
        // state, not part of the portable bundle.
        let quota_config = config.quota.clone().unwrap_or_default();
        let quota_state_path = layout.local.cron_dir.join("quota_state.json");
        let quota_meter = peko_quota::QuotaMeter::load_or_init(
            quota_config,
            Some(quota_state_path),
            chrono::Utc::now(),
        )
        .await
        .map_err(|e| PrincipalManagerError::Config(format!("quota meter init: {e}")))?;
        let quota_meter = Arc::new(quota_meter);

        // Phase 12+ PR #1: per-principal plan DAG storage. The
        // concrete `PlanStorage` is wrapped in `Arc<dyn PlanPort>` —
        // the principal harness reaches plans through the dyn-trait
        // boundary so future impls (in-memory, network-backed) slot
        // in without rewriting this construction site.
        let plan_port: Arc<dyn peko_plan::PlanPort> =
            Arc::new(peko_plan::PlanStorage::new(layout.local.plans_dir.clone()));

        let router = self
            .router_factory
            .create(
                &config,
                memory.clone(),
                &layout.shared.root,
                self.resolver.clone(),
                plan_port.clone(),
            )
            .await;

        let agent_prompts = discover_role_prompts(&layout.shared.roles_dir).await?;

        let mcp = crate::extensions::mcp::service::PrincipalMcpRuntime::new(
            id.clone(),
            layout.shared.root.clone(),
            layout.local.root.join("mcp_cache"),
            self.signing_vault().ok(),
            self.resolver.clone(),
            quota_meter.clone(),
            config.preferred_model_id.clone(),
        );
        self.tooling.bind_mcp(mcp.clone()).await;

        let principal = Arc::new(Principal {
            mcp,
            id: id.clone(),
            config: RwLock::new(config),
            config_file_stamp: Principal::current_config_file_stamp(&layout.shared.root),
            run_failures: Default::default(),
            workspace_path: layout.shared.root.clone(),
            memory,
            router,
            agent_prompts,
            quota_meter,
            plan_port,
        });

        self.principals
            .write()
            .await
            .insert(id.clone(), principal.clone());
        if let Some(port) = self.channel_port() {
            self.register_actor_identity(&principal, &port)
                .await
                .map_err(|e| PrincipalManagerError::Config(format!("actor registration: {e}")))?;
        }
        self.principals_by_name.write().await.insert(name, id);

        Ok(principal)
    }

    /// Load an existing Principal from a `principal.toml` on disk.
    ///
    /// The parent directory of `config_path` becomes the Principal's
    /// workspace. The caller is responsible for ensuring the directory
    /// and agent prompt files exist.
    pub async fn load(&self, config_path: &Path) -> Result<Arc<Principal>, PrincipalManagerError> {
        let config_str = tokio::fs::read_to_string(config_path)
            .await
            .map_err(PrincipalManagerError::Io)?;

        let mut config: PrincipalConfig = toml::from_str(&config_str)
            .map_err(|e| PrincipalManagerError::Config(e.to_string()))?;

        let name = config.name.clone();
        config.warn_deprecated_children(&name);
        {
            let by_name = self.principals_by_name.read().await;
            if let Some(id) = by_name.get(&name) {
                return self
                    .get(id.clone())
                    .await
                    .ok_or(PrincipalManagerError::NotFound(name));
            }
        }

        let workspace_path = config_path
            .parent()
            .ok_or_else(|| PrincipalManagerError::Config("invalid config path".to_string()))?
            .to_path_buf();

        // Phase A: derive the typed layout from the principal name
        // (already known via `config.name`). `workspace_path` is the
        // Shared tier root (parent of `principal.toml`); quota state
        // lives in the Local tier under
        // `<data_dir>/principals/{name}/local/cron/`.
        let layout = self.path_resolver.principal_layout(&name);

        // ADR-064 follow-up: principals loaded from disk (daemon boot,
        // e2e workspaces assembled by hand) get the same default-start
        // stamp as created ones — the root role prompt has no
        // render-time fallback, so `roles/root.md` must exist before
        // the router factory resolves the root prompt.
        crate::principal::genesis::seed_default_roles(&workspace_path)
            .map_err(|e| PrincipalManagerError::Config(format!("role seed failed: {e:#}")))?;

        // Stable principal id: use the persisted one when present. For
        // pre-existing `principal.toml` files without an `id` field,
        // generate once and re-persist so the id (and everything keyed
        // by it — peer DM channels, cron ownership, quota state)
        // stabilizes across daemon restarts. A persist failure is
        // non-fatal: the in-memory principal still works with the
        // generated id.
        let id = match config.id.clone() {
            Some(id) => id,
            None => {
                let id = PrincipalId::generate();
                config.id = Some(id.clone());
                if let Err(e) = self.persist_config(&workspace_path, &config).await {
                    tracing::warn!(
                        principal = %name,
                        error = %e,
                        "failed to persist generated principal id; id will churn on next restart"
                    );
                }
                id
            }
        };
        if config.actor_key.is_none() {
            config.actor_key = Some(uuid::Uuid::new_v4().to_string());
            self.persist_config(&workspace_path, &config).await?;
        }
        // Canonical actor addressing requires a persisted identity, even for
        // pre-identity local definitions. Never replace an existing DID.
        if config.did.as_ref().is_none_or(|did| did.0.is_empty()) {
            config.did = Some(PrincipalDID(self.generate_identity(&name).await?));
            self.persist_config(&workspace_path, &config).await?;
        }
        // Phase A: memory factory takes the Local tier root, not
        // the Shared root.
        let memory = self.memory_factory.create(&id, &layout.local.root).await;

        // F19: build / restore the quota meter from disk so a daemon
        // restart preserves the principal's accumulated usage. The
        // meter is owned by `Principal` directly — the engine loop
        // fetches it via `Principal.quota_meter` and opens
        // `QuotaScope::with` at run entrypoint.
        let quota_config = config.quota.clone().unwrap_or_default();
        let quota_state_path = layout.local.cron_dir.join("quota_state.json");
        let quota_meter = peko_quota::QuotaMeter::load_or_init(
            quota_config,
            Some(quota_state_path),
            chrono::Utc::now(),
        )
        .await
        .map_err(|e| PrincipalManagerError::Config(format!("quota meter init: {e}")))?;
        let quota_meter = Arc::new(quota_meter);

        // Phase 12+ PR #1: per-principal plan DAG storage. Same shape
        // as the `create` site above.
        let plan_port: Arc<dyn peko_plan::PlanPort> =
            Arc::new(peko_plan::PlanStorage::new(layout.local.plans_dir.clone()));

        let router = self
            .router_factory
            .create(
                &config,
                memory.clone(),
                &workspace_path,
                self.resolver.clone(),
                plan_port.clone(),
            )
            .await;
        let agent_prompts = discover_role_prompts(&layout.shared.roles_dir).await?;

        let mcp = crate::extensions::mcp::service::PrincipalMcpRuntime::new(
            id.clone(),
            workspace_path.clone(),
            layout.local.root.join("mcp_cache"),
            self.signing_vault().ok(),
            self.resolver.clone(),
            quota_meter.clone(),
            config.preferred_model_id.clone(),
        );
        self.tooling.bind_mcp(mcp.clone()).await;

        let principal = Arc::new(Principal {
            mcp,
            id: id.clone(),
            config: RwLock::new(config),
            config_file_stamp: Principal::current_config_file_stamp(&workspace_path),
            run_failures: Default::default(),
            workspace_path,
            memory,
            router,
            agent_prompts,
            quota_meter,
            plan_port,
        });

        self.principals
            .write()
            .await
            .insert(id.clone(), principal.clone());
        if let Some(port) = self.channel_port() {
            self.register_actor_identity(&principal, &port)
                .await
                .map_err(|e| PrincipalManagerError::Config(format!("actor registration: {e}")))?;
        }
        self.principals_by_name.write().await.insert(name, id);

        Ok(principal)
    }

    pub async fn get(&self, id: PrincipalId) -> Option<Arc<Principal>> {
        self.principals.read().await.get(&id).cloned()
    }

    pub async fn get_by_name(&self, name: &str) -> Option<Arc<Principal>> {
        if let Some(id) = name.strip_prefix("principal:local:") {
            return self.get(PrincipalId(id.into())).await;
        }
        if let Some(hub) = name.strip_prefix("principal:hub:") {
            for principal in self.list_all().await {
                if self.hub_id_of(&principal).await.as_deref() == Some(hub) {
                    return Some(principal);
                }
            }
            return None;
        }
        let id = self.principals_by_name.read().await.get(name).cloned()?;
        self.get(id).await
    }

    pub async fn prepare_hub_registration(
        &self,
        name: &str,
        exposure: peko_auth::Exposure,
    ) -> Result<(), PrincipalManagerError> {
        let principal = self
            .get_by_name(name)
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound(name.into()))?;
        let did = if principal.did().await.0.starts_with("did:key:") {
            principal.did().await.0
        } else {
            self.generate_identity(name).await?
        };
        self.update_config(name, |config| {
            config.did = Some(did.into());
            config.exposure = exposure;
        })
        .await?;
        if let Some(port) = self.channel_port() {
            self.register_actor_identity(&principal, &port)
                .await
                .map_err(|e| PrincipalManagerError::Config(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn register_actor_identity(
        &self,
        principal: &Arc<Principal>,
        port: &Arc<dyn peko_channel::ChannelPort>,
    ) -> peko_channel::Result<peko_channel::ActorInfo> {
        let config = principal.config.read().await;
        // Only authenticated Hub receipts and verified directory results establish aliases.
        // principal.toml is editable workspace content, not a Hub identity attestation.
        let existing = port
            .resolve_actor(&format!("principal:local:{}", principal.id.0))
            .await
            .ok();
        let existing = if existing.is_some() {
            existing
        } else {
            // A snapshot can install a previously remote contact locally.
            // Only this runtime's verified registry can supply its Hub link.
            port.known_actors().await?.into_iter().find(|actor| {
                actor.kind == "principal"
                    && config.actor_key.as_deref() == Some(actor.identity_key.as_str())
            })
        };
        let did = config.did.as_ref().map(|d| d.0.clone());
        if existing
            .as_ref()
            .is_some_and(|actor| actor.local_id.is_none())
        {
            let vault = self
                .signing_vault()
                .map_err(|e| peko_channel::ChannelError::Adapter(e.to_string()))?;
            if !did.as_ref().is_some_and(|did| {
                vault
                    .get_principal_identity_private_key(&format!("{did}#keys-1"))
                    .is_some()
            }) {
                return Err(peko_channel::ChannelError::Adapter(
                    "Promoting a remote contact requires its vault-backed signing credential"
                        .into(),
                ));
            }
        }
        let hub_id = existing.and_then(|actor| actor.hub_id);
        let key = config.actor_key.clone();
        let name = config.name.clone();
        let registration = if hub_id.is_none() {
            peko_channel::Registration::LocalOnly
        } else if config.exposure == peko_auth::Exposure::Public {
            peko_channel::Registration::Public
        } else {
            peko_channel::Registration::Private
        };
        drop(config);
        port.register_actor(
            &ActorRef::from(&principal.id),
            hub_id.as_deref(),
            did.as_deref(),
            key.as_deref(),
            Some(&name),
            registration,
        )
        .await
    }

    /// Find a loaded Principal by its stable DID.
    pub async fn find_by_did(&self, did: &str) -> Option<Arc<Principal>> {
        let principals = self.principals.read().await;
        for principal in principals.values() {
            // Avoid awaiting while holding the read lock.
            if let Some(config_did) = principal.config.try_read().ok().and_then(|c| c.did.clone()) {
                if config_did.0 == did {
                    return Some(principal.clone());
                }
            }
        }
        None
    }

    /// List all loaded Principals.
    pub async fn list_all(&self) -> Vec<Arc<Principal>> {
        self.principals.read().await.values().cloned().collect()
    }

    /// Remove a Principal by name, deleting its workspace, data, and identity.
    ///
    /// **Phase A.** The removal walks the typed layout: Shared tier root first
    /// (`{config_dir}/principals/{name}/`) and then Local tier root
    /// (`{data_dir}/principals/{name}/local/`) — both via `PathResolver`. The
    /// previous hand-rolled `data_dir.join("principals").join(name)` join at
    /// line 466 missed the keychain identity cleanup and any cron file; the
    /// new layout makes ownership explicit at every path.
    ///
    /// Note: this method does NOT remove the principal's cron file directly.
    /// The cron handler owns its per-principal `local/cron/schedule.toml` and
    /// `local/cron/history.log` — those are deleted by `delete_jobs_for_principal`
    /// when the caller invokes the cron IPC verb. Future Phase A.5 will wire
    /// that into `PrincipalManager::remove` so the cron tier is cleaned up in
    /// the same call.
    pub async fn remove(&self, name: &str) -> Result<(), PrincipalManagerError> {
        let id = {
            let by_name = self.principals_by_name.read().await;
            by_name
                .get(name)
                .cloned()
                .ok_or_else(|| PrincipalManagerError::NotFound(name.to_string()))?
        };

        // Existence check — the in-memory entry must be present.
        self.get(id.clone())
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound(name.to_string()))?;

        let layout = self.path_resolver.principal_layout(name);
        if let Some(principal) = self.get(id.clone()).await {
            principal.mcp.shutdown().await;
        }
        self.tooling.unbind_mcp(&id).await;

        // Remove from in-memory indexes first.
        {
            self.principals.write().await.remove(&id);
            self.principals_by_name.write().await.remove(name);
            self.session_creation_locks.write().await.remove(&id);
            // Phase 7: drop the cached peer-child turn bundle too.
            self.peer_child_turns.write().await.remove(&id);
        }

        // Phase A: remove both tier roots via the typed layout.
        // Shared first (config), then Local (data). Errors here are
        // surfaced rather than swallowed — silent retention would leak
        // per-principal state the caller was promised to lose.
        if layout.shared.root.exists() {
            tokio::fs::remove_dir_all(&layout.shared.root)
                .await
                .map_err(|e| {
                    tracing::warn!(
                        "failed to remove principal shared root {:?}: {e}",
                        layout.shared.root
                    );
                    PrincipalManagerError::Io(e)
                })?;
        }
        if layout.local.root.exists() {
            tokio::fs::remove_dir_all(&layout.local.root)
                .await
                .map_err(|e| {
                    tracing::warn!(
                        "failed to remove principal local root {:?}: {e}",
                        layout.local.root
                    );
                    PrincipalManagerError::Io(e)
                })?;
        }

        if let Some(port) = self.channel_port() {
            port.unhost_principal(&id)
                .await
                .map_err(|e| PrincipalManagerError::Config(e.to_string()))?;
        }

        Ok(())
    }

    /// Update a Principal's config in place, persisting it to disk.
    pub async fn update_config<F>(
        &self,
        name: &str,
        update: F,
    ) -> Result<Arc<Principal>, PrincipalManagerError>
    where
        F: FnOnce(&mut PrincipalConfig),
    {
        let principal = self
            .get_by_name(name)
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound(name.to_string()))?;

        // Pick up edits made to principal.toml since it was loaded (by
        // hand, or by the principal's own file tools): the write below
        // persists the whole config, and would otherwise put back the
        // old values (a `quota set` reverted a changed model).
        principal.reload_config_if_changed().await;

        // Apply the update under the write lock, then snapshot and release
        // the lock before touching disk so config readers are not blocked
        // for the duration of the (async) file write.
        let snapshot = {
            let mut config = principal.config.write().await;
            update(&mut config);
            config.clone()
        };
        principal.mcp.set_model(snapshot.preferred_model_id.clone());
        self.persist_config(&principal.workspace_path, &snapshot)
            .await?;
        if let Some(port) = self.channel_port() {
            self.register_actor_identity(&principal, &port)
                .await
                .map_err(|e| PrincipalManagerError::Config(e.to_string()))?;
        }
        Ok(principal)
    }

    /// Stamp a runtime boot transition without overwriting definition
    /// edits authored by the genesis turn or creator since this manager
    /// loaded its config. Publish in-memory state only after persistence.
    pub(crate) async fn persist_boot_state(
        &self,
        name: &str,
        state: super::config::BootState,
    ) -> Result<(), PrincipalManagerError> {
        let principal = self
            .get_by_name(name)
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound(name.to_string()))?;
        let path = principal.workspace_path.join("principal.toml");
        let _file_lock = peko_fs_persistence::WorkspaceFileLock::acquire_in(
            self.path_resolver.runtime_locks_dir(),
            &path,
            peko_fs_persistence::DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS,
        )
        .await
        .map_err(|error| PrincipalManagerError::Config(error.to_string()))?;
        let mut config = principal.config.write().await;
        let mut current: PrincipalConfig = toml::from_str(&tokio::fs::read_to_string(&path).await?)
            .map_err(|error| PrincipalManagerError::Config(error.to_string()))?;
        // A stale CLI seeding pass must not rewind a daemon's completed
        // genesis transition. Organized is the terminal runtime state.
        if current.boot_state() != super::config::BootState::Organized {
            current.set_boot_state(state);
        }
        let content = toml::to_string(&current)
            .map_err(|error| PrincipalManagerError::Config(error.to_string()))?;
        let temp_path = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        tokio::fs::write(&temp_path, content).await?;
        if let Err(error) = tokio::fs::rename(&temp_path, &path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error.into());
        }
        *config = current;
        Ok(())
    }

    async fn persist_config(
        &self,
        workspace_path: &Path,
        config: &PrincipalConfig,
    ) -> Result<(), PrincipalManagerError> {
        let toml =
            toml::to_string(config).map_err(|e| PrincipalManagerError::Config(e.to_string()))?;
        // Write-then-rename: turns re-read principal.toml when it
        // changes (`Principal::reload_config_if_changed`) and must
        // never see a half-written file.
        let path = workspace_path.join("principal.toml");
        let temp_path = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        tokio::fs::write(&temp_path, toml).await?;
        if let Err(error) = tokio::fs::rename(&temp_path, &path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error.into());
        }
        Ok(())
    }

    /// Mint the principal's DID identity. Returns the DID string.
    ///
    /// ADR-058 D1: when the daemon's `identity_vault` is attached,
    /// this generates a per-principal ed25519 keypair, derives the
    /// principal DID as `did:key` of that key, and custodies the
    /// private-key seed in the vault under the `principal-identity`
    /// namespace (key id `{did}#keys-1`, base64-STANDARD 32-byte seed,
    /// algorithm `ed25519-raw-base64`). The DID is now a
    /// self-certifying identifier only the key holder can act for; the
    /// cross-runtime channel fan-out loads the key back for author
    /// signatures (`VaultPrincipalSigningKeys` in `daemon::state`).
    ///
    /// Standalone callers load the same runtime vault as the daemon.
    pub(crate) fn signing_vault(&self) -> anyhow::Result<Arc<crate::common::vault::Vault>> {
        match &self.identity_vault {
            Some(vault) => Ok(vault.clone()),
            None => Ok(Arc::new(crate::common::vault::Vault::load(
                self.path_resolver.vault(),
            )?)),
        }
    }

    async fn generate_identity(&self, _name: &str) -> Result<String, PrincipalManagerError> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        use rand::RngCore as _;
        let vault = self
            .signing_vault()
            .map_err(|e| PrincipalManagerError::Identity(e.to_string()))?;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let did =
            peko_identity::runtime::public_key_to_did_key(&signing_key.verifying_key().to_bytes());
        vault
            .set_principal_identity_private_key(
                &format!("{did}#keys-1"),
                "ed25519-raw-base64",
                &BASE64.encode(seed),
            )
            .map_err(|e| PrincipalManagerError::Identity(e.to_string()))?;
        Ok(did)
    }

    /// Get (or create) the session-creation mutex for a given Principal.
    async fn session_creation_lock(
        &self,
        principal_id: PrincipalId,
    ) -> Arc<tokio::sync::Mutex<()>> {
        {
            let locks = self.session_creation_locks.read().await;
            if let Some(lock) = locks.get(&principal_id) {
                return lock.clone();
            }
        }
        let mut locks = self.session_creation_locks.write().await;
        locks
            .entry(principal_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// The shared inbox registry (daemon-global when wired via
    /// `AppState`). Exposed for Phase 7 peer-child turn construction
    /// (`PeerChildTurns::build`) so child runs drain the same registry
    /// the ingress serial-queue fallback queues steering into.
    pub fn shared_inbox_registry(&self) -> Arc<peko_session::InboxRegistry> {
        Arc::clone(&self.inbox_registry)
    }

    /// Sprint 2 Phase 7: get-or-build the principal's peer-child turn
    /// bundle (the shared persona-carrying executor + session manager
    /// + trunk ownership anchor). Cached per principal; a build
    /// failure (no resolvable model — the same configuration error
    /// that breaks the trunk path) is NOT cached, so a config fix
    /// takes effect on the next message.
    ///
    /// B8c.4 fix: serialize the build step under the cache's write
    /// lock so concurrent first-callers don't each construct their
    /// own `PeerChildTurns` (each carries a fresh `session_manager`,
    /// so racing creates would route `ensure_child_ingress` /
    /// `drive_turn_streaming` through different managers — the child
    /// session created by one would be invisible to the other's
    /// `resume_preflight`). The fix uses `entry` to coalesce: first
    /// writer builds + inserts; racing writers re-read the inserted
    /// value and return it.
    pub(crate) async fn peer_child_turns_for(
        &self,
        principal: &Arc<Principal>,
    ) -> Result<Arc<crate::principal::child_turns::PeerChildTurns>, PrincipalManagerError> {
        if let Some(turns) = self.peer_child_turns.read().await.get(&principal.id) {
            return Ok(Arc::clone(turns));
        }
        let mut cache = self.peer_child_turns.write().await;
        // Re-check under the write lock — a racing writer may have
        // already populated the cache between our read lock release
        // and our write lock acquire.
        if let Some(turns) = cache.get(&principal.id) {
            return Ok(Arc::clone(turns));
        }
        let resolver = self.resolver.clone().ok_or_else(|| {
            PrincipalManagerError::Config(
                "no LLM resolver configured; cannot drive peer-child turns".to_string(),
            )
        })?;
        let observability = self
            .observability
            .clone()
            .unwrap_or_else(|| Arc::new(Observability::new("principal")));
        let turns = crate::principal::child_turns::PeerChildTurns::build(
            principal,
            &resolver,
            observability,
            Some(self.shared_inbox_registry()),
            Arc::clone(&self.tooling),
        )
        .await
        .map_err(|e| {
            PrincipalManagerError::RouterError(RouterError::AgentFailed(format!("{e:?}")))
        })?;
        // Phase 10: thread the channel port + DM provisioning wiring
        // through so ingress auto-provisions the peer's DM channel and
        // a freshly created one gets its subscriber without a restart.
        let dm_hook = self
            .dm_subscriber_hook
            .read()
            .expect("dm_subscriber_hook lock poisoned")
            .clone();
        let turns = turns
            .with_channel_port(self.channel_port.clone())
            .with_dm_subscriber_hook(dm_hook)
            .with_dm_lock(Some(self.session_creation_lock(principal.id.clone()).await))
            .with_idle_detector(self.idle_detector.clone());
        let turns = Arc::new(turns);
        cache.insert(principal.id.clone(), Arc::clone(&turns));
        Ok(turns)
    }

    /// Phase 11: find-or-create the peer's standing child of the trunk
    /// and return it together with the peer's DM channel (when a
    /// channel port is attached; `None` in port-less contexts).
    /// Idempotent per peer. The IPC ingress handlers post the inbound
    /// message + reply to the returned channel
    /// (`peer_dm::post_peer_dm_inbound` / `post_peer_dm_reply`); the
    /// IPC steering path keys the CHILD session inbox via
    /// `ingress.child_id`.
    pub(crate) async fn ensure_peer_child_ingress(
        &self,
        principal: &Arc<Principal>,
        peer: &ActorRef,
    ) -> Result<PeerChildIngress, PrincipalManagerError> {
        let turns = self.peer_child_turns_for(principal).await?;
        turns.ensure_child_ingress(peer).await.map_err(|e| {
            PrincipalManagerError::RouterError(RouterError::AgentFailed(format!("{e:?}")))
        })
    }

    /// B8c.4: shared body for [`Self::receive_streaming`] and
    /// `handle_principal_send`. Two call sites previously
    /// inlined the same five-step sequence (resolve peer's standing
    /// child + DM channel → post inbound message attributed to the
    /// peer → acquire a run permit OR queue a steering message →
    /// surface the outcome). The bodies had drifted subtly in
    /// error-mapping and queued-notice formatting — this helper is
    /// the single source of truth.
    ///
    /// `mode = Interactive` acquires a run permit and queues on
    /// conflict (used by `receive_streaming` and the principal IPC
    /// ingress).
    ///
    /// `attachments` (ADR-088, already copied into the workspace) ride
    /// the DM post; a queued message carries their notes only (D5).
    pub(crate) async fn drive_principal_ingress(
        &self,
        principal: &Arc<Principal>,
        peer: &ActorRef,
        message: &str,
        attachments: &[peko_protocol::channel::Attachment],
        mode: IngressMode,
    ) -> Result<IngressOutcome, IngressError> {
        let turns = self
            .peer_child_turns_for(principal)
            .await
            .map_err(IngressError::Resolve)?;
        let ingress = turns.ensure_child_ingress(peer).await.map_err(|e| {
            IngressError::Resolve(PrincipalManagerError::RouterError(
                RouterError::AgentFailed(format!("{e:?}")),
            ))
        })?;
        let child_id = ingress.child_id.clone();

        // Phase 11: post the inbound message to the peer's DM channel
        // (peer-attributed) BEFORE acquiring the run permit / queueing.
        // A post failure rejects the ingress so the consumer cannot
        // believe they sent something that never entered the
        // conversation's durable home.
        if let (Some(port), Some(dm)) = (&self.channel_port, &ingress.dm_channel) {
            post_peer_dm_inbound_attached(
                port,
                &principal.id,
                dm,
                &peer.to_string(),
                message,
                attachments.to_vec(),
            )
            .await
            .map_err(|e| IngressError::Post(anyhow::anyhow!("{e:#}")))?;
        }

        match mode {
            IngressMode::Interactive => {
                match self.inbox_registry.try_acquire_run(&child_id).await {
                    Some(permit) => Ok(IngressOutcome::Ready {
                        child_id,
                        dm_channel: ingress.dm_channel,
                        permit,
                    }),
                    None => {
                        // A run is already active for this child session —
                        // queue the message as steering. Caller emits the
                        // "Queued..." notice.
                        let inbox = self.inbox_registry.get_or_create(&child_id).await;
                        let text = crate::principal::attachments::turn_text(
                            message,
                            &principal.workspace_path,
                            attachments,
                        );
                        inbox.push(SteeringMessage::new(text).into()).await;
                        // A queued message is someone talking to the
                        // principal, though no new turn starts yet.
                        if let Some(detector) = &self.idle_detector {
                            detector.record_activity(&principal.id.0).await;
                        }
                        Ok(IngressOutcome::Queued { child_id })
                    }
                }
            }
        }
    }

    /// Phase 11: the daemon-global channel port (when attached) — the
    /// IPC ingress handlers + `peko log` read the peer DM channels
    /// through it.
    pub(crate) fn channel_port(&self) -> Option<Arc<dyn peko_channel::ChannelPort>> {
        self.channel_port.clone()
    }

    /// Borrow the shared `inbox_registry` and per-principal
    /// `session_creation_lock` for callers (e.g. the streaming IPC
    /// handler) that need to invoke the root router without
    /// going through `receive()`. Identical semantics to the values
    /// `receive()` uses internally, so the streaming and
    /// non-streaming paths share back-pressure.
    pub async fn streaming_primitives(
        &self,
        principal_id: &PrincipalId,
    ) -> (
        Arc<peko_session::InboxRegistry>,
        Arc<tokio::sync::Mutex<()>>,
    ) {
        let lock = self.session_creation_lock(principal_id.clone()).await;
        (Arc::clone(&self.inbox_registry), lock)
    }

    /// Build the `RouterContext` for a message arriving at a Principal
    /// boundary. This is the single point of truth for permission checks,
    /// session recall, and the principal's per-message view of its
    /// configuration — both the one-shot `receive` path and the streaming
    /// `PrincipalSendStream` path funnel through here so the two can never
    /// drift (audit H1).
    ///
    /// Returns the assembled `RouterContext` ready to hand to a
    /// `PrincipalRouter::route` / `route_streaming` call.
    pub async fn build_router_context(
        &self,
        principal: &Arc<Principal>,
        peer: ActorRef,
        message: String,
        channel: ChannelContext,
        // Per-message configured model override (`peko send --model`).
        // `None` preserves the principal's pinned model.
        override_model: Option<String>,
    ) -> Result<RouterContext, PrincipalManagerError> {
        self.build_router_context_as(
            principal,
            peer.clone(),
            peer,
            message,
            channel,
            override_model,
        )
        .await
    }

    /// ADR-057: authority-split variant of
    /// [`PrincipalManager::build_router_context`]. `peer` is the
    /// attribution identity (conversation key: session child, DM
    /// channel, author rows); `authority` is the grant-matching
    /// subject the `Permission::Chat` check runs against. They differ
    /// only for local callers on pekohub-logged-in runtimes (local
    /// trust keeps owner authority while attribution follows the hub
    /// user); all other surfaces pass the same subject for both.
    pub async fn build_router_context_as(
        &self,
        principal: &Arc<Principal>,
        peer: ActorRef,
        authority: ActorRef,
        message: String,
        channel: ChannelContext,
        override_model: Option<String>,
    ) -> Result<RouterContext, PrincipalManagerError> {
        // Hand edits to principal.toml apply to this message.
        principal.reload_config_if_changed().await;
        // Enforce Principal-level permissions before any routing or session work.
        let resource = {
            let config = principal.config.read().await;
            Resource::Principal {
                name: config.name.clone(),
                owner: config.owner.clone(),
                permissions: config.permissions.clone(),
                exposure: config.exposure.clone(),
            }
        };
        let port = self.channel_port();
        let keys = crate::security::actor_keys(port.as_deref());
        if let Err(denied) = check_permission(&resource, Permission::Chat, &authority, &*keys) {
            return Err(PrincipalManagerError::PermissionDenied(denied.to_string()));
        }

        // Recall any existing session for this peer to inform routing.
        let latest_session = principal
            .memory
            .find_latest_session_for_peer(&peer)
            .await
            .map_err(|e| {
                PrincipalManagerError::RouterError(RouterError::AgentFailed(e.to_string()))
            })?;

        let recalled_context: Vec<_> = latest_session
            .into_iter()
            .map(|artifact| super::router::ContextInjection {
                kind: super::router::ContextInjectionKind::Session,
                id: artifact.session_id,
                content: artifact.summary.unwrap_or_default(),
            })
            .collect();

        let (available_agents, routing, intent, governance, preferred_model_id, principal_name) = {
            let config = principal.config.read().await;
            // ADR-066 P2: presence = spawnability — every role file in
            // the workspace is enabled.
            let available_agents: Vec<_> = principal
                .agent_prompts
                .iter()
                .map(|(id, p)| super::router::AgentPromptSummary {
                    id: id.clone(),
                    name: p.name.clone(),
                    description: p.frontmatter.description.clone(),
                    enabled: true,
                })
                .collect();

            (
                available_agents,
                config.routing.clone(),
                config.intent.clone(),
                config.governance.clone(),
                config.preferred_model_id.clone(),
                config.name.clone(),
            )
        };

        Ok(RouterContext {
            tooling: Arc::clone(&self.tooling),
            principal_id: principal.id.clone(),
            principal_name,
            peer,
            message,
            channel,
            routing,
            recalled_context,
            available_agents,
            intent,
            governance,
            preferred_model_id,
            inbox_registry: Arc::clone(&self.inbox_registry),
            session_creation_lock: self.session_creation_lock(principal.id.clone()).await,
            observability: self.observability.clone(),
            override_model,
            // Bug A (2026-08-01 v2): populate the principal's
            // `Arc<QuotaMeter>` here so `route_streaming` →
            // `run_root_agent_prompt_streaming` → the engine loop
            // charges the per-cycle counter on every LLM call.
            // `QuotaMeter::unlimited()` returns an unlimited meter
            // when the principal has no quota configured, so passing
            // it through unconditionally is safe.
            quota_meter: Some(Arc::clone(&principal.quota_meter)),
            stamps: Some(crate::principal::trust::stamps_for(
                principal,
                self.channel_port.clone(),
            )),
            acting_for: None,
        })
    }

    /// The main entry point: a message arrives at a Principal boundary.
    ///
    /// Sprint 2 Phase 7 routing: peer channels (Cli/Http/Hub/A2a/P2p/
    /// Webhook/FileWatch) drive the turn in the peer's STANDING CHILD
    /// of the trunk (provisioned on first contact via
    /// [`crate::principal::peer_children::ensure_peer_child`], driven
    /// via [`crate::principal::child_turns::PeerChildTurns`]) — the
    /// per-peer `root:{peer}` root sessions are retired. Automation
    /// channels (`Cron`) and explicit trunk turns delegate to
    /// [`Self::receive_trunk`]: the trunk (`root:self`) is cron-only.
    ///
    /// The turn is driven via
    /// [`crate::principal::child_turns::PeerChildTurns::drive_turn_streaming`]
    /// so token deltas are delivered to `on_event` as they are produced.
    ///
    /// The returned [`PrincipalResponse`] carries the authoritative final
    /// answer. Callers that forwarded the streamed deltas can ignore the
    /// body; callers that need a single final string — or that hit the
    /// queued path, which emits no events — use it.
    ///
    /// Sprint 3 Phase 12b: the blocking one-shot sibling (`receive`)
    /// was retired with the A2A RPC stack — its only production callers
    /// were the principal-to-principal dispatcher arm and the
    /// `ChannelSend` tool's local (principal) branch, both now
    /// channel-based. Sprint 4: `send_peer` is retired entirely; the
    /// local branch of `ChannelSend` covers the same workload via the
    /// `principal:<did>` channel-id form.
    pub async fn receive_streaming(
        &self,
        principal_id: PrincipalId,
        peer: ActorRef,
        message: String,
        channel: ChannelContext,
        on_event: Box<dyn Fn(peko_engine::AgenticEvent) + Send + Sync>,
        // Per-message configured model override (RP8 wires CLI flags).
        override_model: Option<String>,
    ) -> Result<PrincipalResponse, PrincipalManagerError> {
        // Automation + trunk channels have no streaming trunk entry
        // point; route them through the one-shot trunk path.
        if matches!(channel.kind, ChannelKind::Trunk | ChannelKind::Cron) {
            // ADR-078 D2: the peer must be the owner to reach the trunk.
            let principal = self
                .get(principal_id.clone())
                .await
                .ok_or_else(|| PrincipalManagerError::NotFound("unknown".to_string()))?;
            let stamps = crate::principal::trust::stamps_for(&principal, self.channel_port.clone());
            let driver = stamps.driver(&peer, &peer).await;
            let acting_for = stamps
                .enter(
                    &driver,
                    peko_tools_core::Compartment::Internal,
                    peko_tools_core::Origin::Ingress { channel: None },
                )
                .await;
            return self
                .receive_trunk(principal_id, message, override_model, acting_for)
                .await;
        }

        let principal = self
            .get(principal_id)
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound("unknown".to_string()))?;

        let ctx = self
            .build_router_context(
                &principal,
                peer.clone(),
                message,
                channel,
                override_model.clone(),
            )
            .await?;

        // ADR-078 D7: the owner's approval commands are answered by the
        // runtime before any agent turn.
        if let Some(manager) = self.self_ref.get().and_then(std::sync::Weak::upgrade) {
            let stamps = crate::principal::trust::stamps_for(&principal, self.channel_port.clone());
            if stamps.driver(&peer, &peer).await.trust == peko_tools_core::Trust::Owner {
                if let Some(reply) = crate::principal::policy_gate::handle_owner_command(
                    &manager,
                    &principal,
                    &ctx.message,
                )
                .await
                {
                    return Ok(PrincipalResponse::text(reply));
                }
            }
        }

        // Phase 11: same DM-channel discipline as `receive` — the
        // inbound message is posted (peer-attributed) before dispatch;
        // a post failure rejects dispatch. The full ingress sequence
        // (resolve child + DM channel, post inbound, acquire or queue)
        // is unified in `drive_principal_ingress` (B8c.4).
        match self
            .drive_principal_ingress(
                &principal,
                &peer,
                &ctx.message,
                &[],
                IngressMode::Interactive,
            )
            .await
        {
            Ok(IngressOutcome::Ready {
                child_id,
                dm_channel,
                permit: _permit,
            }) => {
                // Same serial-queue discipline as `receive`: only one
                // run may be active per peer child (the helper
                // already acquired the permit). Drive the streaming
                // turn. The helper also re-built `turns` internally
                // (cached), but we only need `child_id` here.
                let turns = self.peer_child_turns_for(&principal).await?;
                // ADR-078: this path checks permissions against the
                // peer itself, so it is also the authority.
                let driver = turns.driver(&peer, &peer).await;
                let acting_for = turns
                    .root_stamp(
                        Some(&driver),
                        &child_id,
                        peko_tools_core::Origin::Ingress { channel: None },
                    )
                    .await;
                let outcome = turns
                    .drive_turn_streaming(
                        &child_id,
                        &ctx.message,
                        Arc::from(on_event),
                        None,
                        override_model,
                        acting_for,
                    )
                    .await
                    .map_err(|e| {
                        PrincipalManagerError::RouterError(RouterError::AgentFailed(format!(
                            "{e:?}"
                        )))
                    })?;
                let response = outcome.final_text;
                if let (Some(port), Some(dm)) = (&self.channel_port, &dm_channel) {
                    post_peer_dm_reply(port, &principal.id, dm, &response).await;
                }
                self.record_peer_recall(&principal, &peer, &child_id, &response)
                    .await;
                Ok(PrincipalResponse::text(response))
            }
            Ok(IngressOutcome::Queued { child_id }) => {
                // Phase 11: the "Queued…" notice is transport UX, not
                // conversation — no DM channel row (see `receive`).
                let queued = format!("Queued for root agent session {child_id}.");
                Ok(PrincipalResponse::queued(queued))
            }
            Err(IngressError::Resolve(e)) => Err(e),
            Err(IngressError::Post(e)) => Err(PrincipalManagerError::Config(format!(
                "peer DM inbound post: {e:#}"
            ))),
        }
    }

    /// Principal-self entry point (Phase 3, 2026-08-15): a turn fired
    /// into the principal's forever-continuous trunk session
    /// `root:self` (see `routers::root::trunk_session_id`). This is the
    /// receive path for cron `Send` jobs with `target = "trunk"`.
    ///
    /// Design:
    /// - **Proxy subject.** No external peer exists for a trunk turn.
    ///   The principal's owner is used as the proxy subject so the
    ///   existing gates work unchanged: `build_router_context` runs its
    ///   `check_permission(Chat, &peer)` and peer-keyed memory recall
    ///   (`find_latest_session_for_peer`) against the owner. The
    ///   SESSION id, however, is always `root:self` — never
    ///   `root:{owner}` — via `ChannelKind::Trunk`.
    /// - **No conversation projection.** The trunk has no peer thread
    ///   to project into, so the trunk path never touched the peer
    ///   conversation record (pre-Phase-11: chat log; post-Phase-11:
    ///   the per-peer DM channels). The trunk session JSONL remains
    ///   the durable record; the cron `Send` outcome surfaces via the
    ///   messenger's `deliver_note` on the owner's DM channel.
    /// - **Same run discipline as `receive`.** The per-session run
    ///   permit is acquired on `root:self`; a second trunk turn
    ///   arriving while a run is active is queued as a steering
    ///   message into the trunk inbox (a rapid cron tick steers the
    ///   live trunk run instead of crashing).
    pub async fn receive_trunk(
        &self,
        principal_id: PrincipalId,
        message: String,
        // Per-message configured model override; same semantics as
        // `receive` (`None` uses the principal's pinned model).
        override_model: Option<String>,
        // ADR-078 D2: the trunk acts only for the owner. Anything else
        // is refused before a turn starts.
        acting_for: peko_tools_core::ActingFor,
    ) -> Result<PrincipalResponse, PrincipalManagerError> {
        if !acting_for.is_owner()
            || acting_for.compartment != peko_tools_core::Compartment::Internal
        {
            return Err(PrincipalManagerError::PermissionDenied(format!(
                "the trunk acts only for the owner; refused a run acting for {} ({})",
                acting_for.actor,
                acting_for.trust.as_str()
            )));
        }
        let principal = self
            .get(principal_id)
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound("unknown".to_string()))?;

        let peer = {
            let config = principal.config.read().await;
            config.owner.clone()
        };
        let channel = ChannelContext {
            kind: ChannelKind::Trunk,
            streaming: false,
        };

        let mut ctx = self
            .build_router_context(&principal, peer, message, channel, override_model)
            .await?;
        ctx.acting_for = Some(acting_for);

        // The owner talking to the trunk directly is activity for `Idle`
        // schedules; a cron turn is not.
        let idle_detector = self.idle_detector.as_ref().filter(|_| {
            matches!(
                ctx.acting_for.as_ref().map(|a| &a.origin),
                Some(peko_tools_core::Origin::Ingress { .. })
            )
        });
        if let Some(detector) = idle_detector {
            detector.record_activity(&principal.id.0).await;
        }
        let session_id = super::routers::root::trunk_session_id();
        // The trunk id is the same constant for every principal; the
        // registry key is principal-scoped so another principal's busy
        // trunk neither refuses nor receives this turn.
        let inbox_key = peko_session::inbox_key(&principal.id.0, &session_id);
        match self.inbox_registry.try_acquire_run(&inbox_key).await {
            Some(_permit) => {
                let decision = principal.router.route(ctx).await;
                if let Some(detector) = idle_detector {
                    detector.record_activity(&principal.id.0).await;
                }
                let decision = decision?;
                match decision {
                    RouteDecision::Respond { response } => Ok(PrincipalResponse::text(response)),
                }
            }
            None => {
                let inbox = self.inbox_registry.get_or_create(&inbox_key).await;
                inbox
                    .push(SteeringMessage::new(ctx.message.clone()).into())
                    .await;
                let queued = format!("Queued for root agent session {session_id}.");
                Ok(PrincipalResponse::queued(queued))
            }
        }
    }
}

impl PrincipalManager {
    /// Sprint 2 Phase 7: record the peer-recall artifact for a peer
    /// turn that ran in the peer's STANDING CHILD session (the
    /// per-peer child of the trunk provisioned by
    /// [`crate::principal::peer_children::ensure_peer_child`]).
    ///
    /// This is the write side of the session-recall loop after the
    /// per-peer root sessions were retired: the peer's latest session
    /// IS the child, so the artifact's `session_id` is the child
    /// session id. The read side (`build_router_context` →
    /// `PrincipalMemory::find_latest_session_for_peer`) is peer-keyed
    /// and needs no change — it picks this artifact up as-is.
    ///
    /// Best-effort like the retired router's write: a failure logs a
    /// warning and does not fail the turn. The Phase 7 ingress
    /// wrappers (`receive` / `receive_streaming` / the IPC
    /// `principal_send` handler) call this around the child turn
    /// driver.
    pub(crate) async fn record_peer_recall(
        &self,
        principal: &Arc<Principal>,
        peer: &ActorRef,
        child_session_id: &str,
        summary: &str,
    ) {
        let artifact = super::memory::SessionArtifact {
            session_id: child_session_id.to_string(),
            peer: peer.clone(),
            title: Some("peer-child".to_string()),
            updated_at: chrono::Utc::now(),
            summary: Some(summary.to_string()),
        };
        if let Err(e) = principal.memory.record_session(artifact).await {
            tracing::warn!("failed to record peer-child session artifact: {e}");
        }
    }
}

/// Response from a Principal.receive call.
#[derive(Debug, Clone)]
pub struct PrincipalResponse {
    pub content: String,
}

impl PrincipalResponse {
    pub fn text(content: String) -> Self {
        Self { content }
    }

    pub fn queued(content: String) -> Self {
        Self { content }
    }
}

/// Discover agent prompts for a principal.
///
/// **Phase A.** The caller passes the typed `roles_dir` directly
/// (i.e. `PathResolver::principal_layout(name).shared.roles_dir`)
/// rather than the Shared tier root + a hand-rolled `"agents"`
/// suffix. The legacy `workspace_path.join("agents")` join is gone
/// from this function.
async fn discover_role_prompts(
    roles_dir: &Path,
) -> Result<HashMap<String, AgentPrompt>, PrincipalManagerError> {
    let mut prompts = HashMap::new();

    if roles_dir.exists() {
        let adapter = RoleAdapter::new();
        let discovered = adapter.discover_roles(roles_dir);
        for d in discovered {
            let canonical_id = d.manifest.id.clone();
            let prompt = load_agent_prompt(&d.file_path)
                .map_err(|e| PrincipalManagerError::Config(format!("{}: {e}", canonical_id)))?;
            prompts.insert(canonical_id, prompt);
        }
    }

    Ok(prompts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_auth::{ActorRef, Permission, PermissionGrant};

    use crate::engine::tool_runtime::ToolRuntime;

    use crate::principal::{
        router::{ChannelContext, ChannelKind},
        DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
    };
    use crate::principal::{
        PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalRoutingConfig,
    };

    use peko_providers::{LlmResolver, MockAdapter};
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, Arc<PrincipalManager>, MockAdapter, PrincipalId) {
        let temp = TempDir::new().expect("temp dir");
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let tool_runtime = ToolRuntime::with_workspace(path_resolver.clone(), temp.path())
            .await
            .expect("tool runtime should initialize");

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("models.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::async_exec::executor::standalone_inbox_registry(),
            )
            .with_tooling(tool_runtime.tooling().clone())
            .with_resolver(resolver),
        );

        let principal = create_test_principal(&manager, "stressy").await;
        (temp, manager, adapter, principal.id.clone())
    }

    /// Phase 11: `setup` with a real `ChannelStore` attached via
    /// `with_channel_port`, so the re-routed ingress paths post the
    /// peer conversation to the peer's DM channel.
    async fn setup_with_channels() -> (
        TempDir,
        Arc<PrincipalManager>,
        MockAdapter,
        PrincipalId,
        Arc<peko_channel::ChannelStore>,
    ) {
        let temp = TempDir::new().expect("temp dir");
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let tool_runtime = ToolRuntime::with_workspace(path_resolver.clone(), temp.path())
            .await
            .expect("tool runtime should initialize");

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("models.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        let store = Arc::new(peko_channel::ChannelStore::new(
            peko_channel::ChannelConfig {
                runtime_dir: temp.path().join("runtime"),
                shared_dir: None,
            },
        ));
        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::async_exec::executor::standalone_inbox_registry(),
            )
            .with_tooling(tool_runtime.tooling().clone())
            .with_resolver(resolver)
            .with_channel_port(store.clone()),
        );

        let principal = create_test_principal(&manager, "stressy").await;
        (temp, manager, adapter, principal.id.clone(), store)
    }

    /// Phase 11 test helper: the `(author, text)` rows of every
    /// `Posted` event in the peer's DM channel, oldest first. Finds
    /// the channel by the peer child's binding path.
    async fn dm_posted_rows(
        manager: &PrincipalManager,
        store: &peko_channel::ChannelStore,
        principal: &Arc<Principal>,
        peer: &ActorRef,
    ) -> Vec<(String, String)> {
        use peko_channel::ChannelPort as _;
        let mut session_manager = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(principal.memory.sessions_dir());
        let metas = session_manager
            .list_all_sessions(false)
            .await
            .expect("list sessions");
        let child_id = crate::principal::peer_children::find_dm_child(
            &metas,
            &principal.id,
            peer,
            &manager.channel_port().expect("conversation channel store"),
        )
        .await
        .expect("peer child exists after ingress");
        let slug = metas
            .iter()
            .find(|m| m.session_id.to_string() == child_id)
            .and_then(|m| m.slug.clone())
            .expect("peer child has a slug");
        let port = manager.channel_port().expect("channel port attached");
        let channel = crate::principal::peer_dm::find_peer_dm_channel(
            &port,
            &principal.id,
            &format!("/{slug}"),
        )
        .await
        .expect("dm lookup")
        .expect("DM channel exists after ingress");
        store
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .expect("peek")
            .iter()
            .filter_map(|ev| match ev {
                peko_channel::ChannelEvent::Posted { author, text, .. } => {
                    Some((author.clone(), text.clone()))
                }
                _ => None,
            })
            .collect()
    }
    async fn create_test_principal(manager: &PrincipalManager, name: &str) -> Arc<Principal> {
        create_test_principal_with_agents(manager, name, &[]).await
    }

    async fn create_test_principal_with_agents(
        manager: &PrincipalManager,
        name: &str,
        extra_agents: &[&str],
    ) -> Arc<Principal> {
        let roles_dir = manager
            .path_resolver
            .principal_layout(name)
            .shared
            .roles_dir;
        tokio::fs::create_dir_all(&roles_dir).await.unwrap();

        let primary_body = format!(
            "---\ndescription: \"Test assistant for {name}\"\n---\n\n\
             You are {name}, a test assistant. Reply concisely.\n"
        );
        tokio::fs::write(roles_dir.join("primary.md"), primary_body)
            .await
            .unwrap();

        for agent in extra_agents {
            let body = format!(
                "---\nname: {agent}\ndescription: \"Agent {agent}\"\n---\n\n\
                 You are {agent}.\n"
            );
            tokio::fs::write(roles_dir.join(format!("{agent}.md")), body)
                .await
                .unwrap();
        }

        let config = test_config(name);
        manager.create(config).await.unwrap()
    }

    fn test_config(name: &str) -> PrincipalConfig {
        PrincipalConfig {
            actor_key: None,
            name: name.to_string(),
            id: None,
            did: None,
            owner: ActorRef::User("test-owner".into()),
            identity: PrincipalIdentityConfig {
                display_name: Some(name.to_string()),
                description: Some(format!("The {name} Principal")),
                avatar: None,
            },
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            exposure: peko_auth::Exposure::Private,
            status: None,
            boot_state: None,
            permissions: vec![PermissionGrant {
                subject: ActorRef::Public,
                permission: Permission::Chat,
                granted_at: chrono::Utc::now().to_rfc3339(),
                granted_by: ActorRef::User("test-owner".into()),
            }],
            preferred_model_id: Some("mock".to_string()),
            quota: None,
            children: Default::default(),
        }
    }

    fn cli_channel() -> ChannelContext {
        ChannelContext {
            kind: ChannelKind::Cli,
            streaming: false,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn same_peer_continues_session() {
        let (_temp, manager, adapter, id) = setup().await;
        let turns = 5;
        for i in 0..turns {
            adapter.queue_text(format!("reply {i}"));
        }

        let peer = ActorRef::User("alice".into());
        for i in 0..turns {
            let response = manager
                .receive_streaming(
                    id.clone(),
                    peer.clone(),
                    format!("message {i}"),
                    cli_channel(),
                    Box::new(|_| {}),
                    None,
                )
                .await
                .expect("receive should succeed");
            assert!(
                response.content.contains(&format!("reply {i}")),
                "response should contain mock reply {i}: {}",
                response.content
            );
        }

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions = principal
            .memory
            .list_sessions()
            .await
            .expect("list sessions");
        assert_eq!(
            sessions.len(),
            1,
            "repeated messages from one peer must reuse one session"
        );
        assert_eq!(sessions[0].peer, peer);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn distinct_peers_spawn_isolated_sessions() {
        let (_temp, manager, adapter, id) = setup().await;
        let peers = 5;
        for i in 0..peers {
            adapter.queue_text(format!("peer reply {i}"));
        }

        for i in 0..peers {
            let peer = ActorRef::User(format!("peer-{i}").into());
            let response = manager
                .receive_streaming(
                    id.clone(),
                    peer,
                    format!("hello {i}"),
                    cli_channel(),
                    Box::new(|_| {}),
                    None,
                )
                .await
                .expect("receive should succeed");
            assert!(response.content.contains(&format!("peer reply {i}")));
        }

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions = principal
            .memory
            .list_sessions()
            .await
            .expect("list sessions");
        assert_eq!(
            sessions.len(),
            peers as usize,
            "each new peer should get its own session"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn concurrent_receives_are_isolated() {
        let (_temp, manager, adapter, id) = setup().await;
        let peers = 10;
        for i in 0..peers {
            adapter.queue_text(format!("concurrent {i}"));
        }

        let mut handles = Vec::with_capacity(peers as usize);
        for i in 0..peers {
            let manager = Arc::clone(&manager);
            let id = id.clone();
            let handle = tokio::spawn(async move {
                let peer = ActorRef::User(format!("concurrent-{i}").into());
                // M7 fix: `SessionManager::create_session` now holds the
                // `metadata_controller` write lock for the full
                // create-metadata + create-for-peer + save-index sequence,
                // so two peers can no longer interleave their index
                // updates. The previous test had a retry loop on the
                // transient `AgentFailed("failed to create root agent
                // session")` race; with the lock held end-to-end the
                // race can't happen, and this receive is a one-shot.
                manager
                    .receive_streaming(
                        id.clone(),
                        peer.clone(),
                        format!("hello {i}"),
                        cli_channel(),
                        Box::new(|_| {}),
                        None,
                    )
                    .await
                    .map_err(|e| -> PrincipalManagerError { e })
            });
            handles.push(handle);
        }

        let results = futures::future::join_all(handles).await;
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(
            ok_count, peers as usize,
            "all concurrent receives should complete"
        );

        let mut actual_texts: Vec<String> = Vec::with_capacity(peers as usize);
        for result in results {
            let response = result
                .expect("task should not panic")
                .expect("receive should succeed");
            actual_texts.push(response.content);
        }

        let expected_texts: Vec<String> = (0..peers).map(|i| format!("concurrent {i}")).collect();
        for expected in &expected_texts {
            assert!(
                actual_texts.iter().any(|t| t.contains(expected)),
                "expected one response to contain '{expected}'"
            );
        }

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions = principal
            .memory
            .list_sessions()
            .await
            .expect("list sessions");
        assert_eq!(
            sessions.len(),
            peers as usize,
            "concurrent peers should each get a distinct session"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn concurrent_same_peer_messages_are_queued() {
        let (_temp, manager, adapter, id) = setup().await;
        let messages = 5;
        adapter.queue_text("same-peer final".to_string());

        let peer = ActorRef::User("same-peer".into());
        let principal = manager
            .get(id.clone())
            .await
            .expect("principal should exist");

        // Phase 7: the serial queue keys on the peer's standing CHILD
        // session. Provision it up front and hold its run permit so
        // every concurrent receive deterministically takes the
        // steering-queue branch (the mock LLM answers instantly, so a
        // real first run would complete before the others arrive —
        // the permit hold makes the interleaving deterministic).
        let child_id = manager
            .ensure_peer_child_ingress(&principal, &peer)
            .await
            .expect("peer child provisioning")
            .child_id;
        let permit_hold = manager
            .inbox_registry
            .try_acquire_run(&child_id)
            .await
            .expect("permit should be free");

        let mut handles = Vec::with_capacity(messages);
        for i in 0..messages {
            let manager = Arc::clone(&manager);
            let id = id.clone();
            let peer = peer.clone();
            let handle = tokio::spawn(async move {
                manager
                    .receive_streaming(
                        id,
                        peer,
                        format!("hello {i}"),
                        cli_channel(),
                        Box::new(|_| {}),
                        None,
                    )
                    .await
            });
            handles.push(handle);
        }

        let results = futures::future::join_all(handles).await;
        let responses: Vec<PrincipalResponse> = results
            .into_iter()
            .map(|r| {
                r.expect("task should not panic")
                    .expect("receive should succeed")
            })
            .collect();

        // Every concurrent arrival queued as a steering message into
        // the CHILD session inbox.
        let expected_queued = format!("Queued for root agent session {child_id}.");
        for r in &responses {
            assert_eq!(
                r.content, expected_queued,
                "a message arriving while a run holds the child permit must queue as steering"
            );
        }
        // The inbox holds exactly the queued steering messages.
        let inbox = manager.inbox_registry.get_or_create(&child_id).await;
        assert_eq!(
            inbox.len().await,
            messages as usize,
            "all concurrent messages queued into the child inbox"
        );

        // Release the permit; the next receive runs ONE turn whose
        // loop drains the queued steering at its first iteration.
        drop(permit_hold);
        let response = manager
            .receive_streaming(
                id.clone(),
                peer.clone(),
                "after".to_string(),
                cli_channel(),
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("receive should succeed");
        assert!(
            response.content.contains("same-peer final"),
            "response should carry the mock reply: {}",
            response.content
        );
        assert_eq!(
            inbox.len().await,
            0,
            "the child run drained the queued steering messages"
        );
        assert_eq!(
            adapter.recorded_requests().len(),
            1,
            "exactly one LLM call ran for the whole batch"
        );

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions = principal
            .memory
            .list_sessions()
            .await
            .expect("list sessions");
        assert_eq!(
            sessions.len(),
            1,
            "all messages for the same peer share one peer-child session"
        );
    }

    /// Build a fresh `PrincipalManager` over the same temp dirs —
    /// the "daemon restart" half of the id-stability tests.
    async fn build_manager(temp: &TempDir) -> Arc<PrincipalManager> {
        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let catalog_path = temp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::async_exec::executor::standalone_inbox_registry(),
            )
            .with_resolver(resolver),
        )
    }

    /// The principal id must survive a daemon restart: `create`
    /// persists it into `principal.toml` and a fresh manager's `load`
    /// recovers the SAME id instead of minting a new one (peer DM
    /// channels are keyed by it — re-minting fragmented `peko log`
    /// history across restarts).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn principal_id_is_stable_across_restarts() {
        let (temp, manager, _adapter, id) = setup().await;

        let config_path = manager
            .path_resolver
            .principal_layout("stressy")
            .shared
            .config_file;
        let toml = tokio::fs::read_to_string(&config_path).await.unwrap();
        assert!(
            toml.contains(&format!("id = \"{id}\"")),
            "generated id should be persisted, got: {toml}"
        );

        // Simulate a daemon restart: a fresh manager loading the same
        // principal.toml must recover the persisted id.
        let manager2 = build_manager(&temp).await;
        let loaded = manager2.load(&config_path).await.expect("load");
        assert_eq!(loaded.id, id);
    }

    /// Pre-existing `principal.toml` files without an `id` field: the
    /// load path generates one, re-persists the config, and the next
    /// restart recovers the same id.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn load_assigns_and_persists_id_for_legacy_config() {
        let (temp, manager, _adapter, original_id) = setup().await;

        let config_path = manager
            .path_resolver
            .principal_layout("stressy")
            .shared
            .config_file;

        // Strip the top-level `id` line to simulate a pre-existing
        // config. Anchored to the `prin_` prefix so the `[owner]`
        // table's `id = "user:..."` line survives.
        let toml = tokio::fs::read_to_string(&config_path).await.unwrap();
        let legacy = toml
            .lines()
            .filter(|line| !line.starts_with("id = \"prin_"))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(&config_path, legacy).await.unwrap();

        let manager2 = build_manager(&temp).await;
        assert!(
            manager2.load(&config_path).await.is_err(),
            "An immutable actor cannot be duplicated under a fresh local ID"
        );
        assert!(manager.get(original_id).await.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn create_generates_real_identity() {
        let (_temp, manager, _adapter, id) = setup().await;
        let principal = manager.get(id).await.expect("principal should exist");
        let did = principal.did().await;
        assert!(
            did.0.starts_with("did:key:"),
            "principal should have a real DID, got {}",
            did.0
        );

        let vault = crate::common::vault::Vault::load(manager.path_resolver.vault()).unwrap();
        assert!(vault
            .get_principal_identity_private_key(&format!("{}#keys-1", did.0))
            .is_some());
        assert!(manager.hub_id_of(&principal).await.is_none());
    }

    /// ADR-058 D1: a manager wired with `with_identity_vault` (the
    /// daemon path) mints the principal DID as `did:key` of a fresh
    /// per-principal ed25519 key and custodies the seed in the vault
    /// under the `principal-identity` namespace — and the seed's
    /// public half IS the DID's key.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn create_with_identity_vault_mints_did_key_and_vaults_seed() {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        use secrecy::ExposeSecret as _;

        let temp = TempDir::new().expect("temp dir");
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let tool_runtime = ToolRuntime::with_workspace(path_resolver.clone(), temp.path())
            .await
            .expect("tool runtime should initialize");

        let catalog_path = temp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        let vault = Arc::new(crate::common::vault::Vault::for_test(
            temp.path(),
            "test-passphrase",
        ));
        let manager = PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::async_exec::executor::standalone_inbox_registry(),
        )
        .with_tooling(tool_runtime.tooling().clone())
        .with_resolver(resolver)
        .with_identity_vault(Arc::clone(&vault));

        let principal = create_test_principal(&manager, "stressy").await;
        let did = principal.did().await.0;
        assert!(
            did.starts_with("did:key:z6Mk"),
            "D1: the principal DID must be did:key of the principal key, got {did}"
        );

        // The seed is custodied in the vault under the dedicated
        // namespace…
        let secret = vault
            .get_principal_identity_private_key(&format!("{did}#keys-1"))
            .expect("the seed must be in the vault");
        let seed_bytes = BASE64
            .decode(secret.expose_secret())
            .expect("seed is base64-STANDARD");
        let seed: [u8; 32] = seed_bytes.try_into().expect("seed is 32 bytes (ed25519)");
        // …its public half is exactly the DID's key…
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(
            peko_identity::runtime::public_key_to_did_key(&signing.verifying_key().to_bytes()),
            did,
            "the vault seed must correspond to the principal DID"
        );
        // …and it is NOT visible in the runtime key's `identity`
        // namespace (the first-match reconstruction scan must never
        // see principal keys).
        assert!(vault
            .get_identity_private_key(&format!("{did}#keys-1"))
            .is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn list_all_returns_all_principals() {
        let (_temp, manager, _adapter, _id) = setup().await;
        create_test_principal(&manager, "beta").await;

        let all = manager.list_all().await;
        let names: Vec<String> = futures::future::join_all(all.iter().map(|p| p.name())).await;
        assert!(names.contains(&"stressy".to_string()));
        assert!(names.contains(&"beta".to_string()));
    }

    /// Regression: every principal's sessions must live under its own
    /// Local tier root (`{data}/principals/{name}/local/sessions/`). The
    /// memory factory receives the Local tier root whose `file_name()` is
    /// always `"local"` — factories that re-derived the principal name
    /// from it once collapsed all sessions into a single shared
    /// `principals/local/local/sessions/` store, leaking one principal's
    /// history into another's context.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn principals_have_isolated_session_stores() {
        let (_temp, manager, _adapter, _id) = setup().await;
        create_test_principal(&manager, "beta").await;

        let all = manager.list_all().await;
        assert_eq!(all.len(), 2);
        let mut sessions_dirs: Vec<std::path::PathBuf> =
            all.iter().map(|p| p.memory.sessions_dir()).collect();
        sessions_dirs.sort();
        assert_ne!(
            sessions_dirs[0], sessions_dirs[1],
            "principals must not share a session store"
        );
        let mut expected_dirs: Vec<_> = ["stressy", "beta"]
            .iter()
            .map(|name| {
                manager
                    .path_resolver
                    .principal_layout(name)
                    .local
                    .sessions_dir
            })
            .collect();
        expected_dirs.sort();
        assert_eq!(
            sessions_dirs, expected_dirs,
            "sessions must use their principal's Local tier"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn promoting_a_contact_requires_a_vault_credential_and_preserves_verified_hub_link() {
        let (_temp, manager, _adapter, id) = setup().await;
        let principal = manager.get(id.clone()).await.unwrap();
        let port = manager.channel_port().unwrap();
        let config = principal.config.read().await;
        let did = config.did.clone().unwrap();
        port.register_actor(
            &ActorRef::from(&id),
            Some("promoted-hub"),
            Some(&did.0),
            config.actor_key.as_deref(),
            Some("stressy"),
            peko_channel::Registration::Private,
        )
        .await
        .unwrap();
        drop(config);
        port.unhost_principal(&id).await.unwrap();
        assert!(port
            .resolve_actor("principal:hub:promoted-hub")
            .await
            .unwrap()
            .local_id
            .is_none());
        principal.config.write().await.did = Some("did:key:missing-credential".into());
        let error = manager
            .register_actor_identity(&principal, &port)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("vault-backed signing credential"));
        assert!(port
            .resolve_actor("principal:hub:promoted-hub")
            .await
            .unwrap()
            .local_id
            .is_none());
        principal.config.write().await.did = Some(did);
        let actor = manager
            .register_actor_identity(&principal, &port)
            .await
            .unwrap();
        assert_eq!(actor.local_id.as_deref(), Some(id.0.as_str()));
        assert_eq!(actor.hub_id.as_deref(), Some("promoted-hub"));
        assert_eq!(manager.hub_id_of(&principal).await, actor.hub_id);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn remove_principal_deletes_workspace() {
        let (_temp, manager, _adapter, id) = setup().await;
        let principal = manager
            .get(id.clone())
            .await
            .expect("principal should exist");
        let workspace_path = principal.workspace_path.clone();
        let port = manager.channel_port().unwrap();
        let config = principal.config.read().await;
        port.register_actor(
            &ActorRef::from(&id),
            Some("removed-hub"),
            config.did.as_ref().map(|did| did.0.as_str()),
            config.actor_key.as_deref(),
            Some("stressy"),
            peko_channel::Registration::Private,
        )
        .await
        .unwrap();
        drop(config);

        manager
            .remove("stressy")
            .await
            .expect("remove should succeed");
        assert!(manager.get_by_name("stressy").await.is_none());
        assert!(!workspace_path.exists(), "workspace should be deleted");
        let actor = port
            .resolve_actor("principal:hub:removed-hub")
            .await
            .unwrap();
        assert!(actor.local_id.is_none());
        assert!(port
            .resolve_actor(&format!("principal:local:{}", id.0))
            .await
            .is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn update_config_persists_changes() {
        let (_temp, manager, _adapter, id) = setup().await;
        manager
            .update_config("stressy", |config| {
                config.exposure = peko_auth::Exposure::Public;
            })
            .await
            .expect("update_config should succeed");

        let principal = manager.get(id).await.expect("principal should exist");
        assert_eq!(principal.exposure().await, peko_auth::Exposure::Public);

        let toml = tokio::fs::read_to_string(principal.workspace_path.join("principal.toml"))
            .await
            .unwrap();
        assert!(
            toml.contains("public"),
            "updated exposure should be persisted"
        );
    }

    /// An update through the manager (e.g. `quota set`) keeps an edit
    /// made to principal.toml on disk since the config was loaded.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn update_config_keeps_edits_made_on_disk() {
        let (_temp, manager, _adapter, id) = setup().await;
        let principal = manager.get(id).await.expect("principal should exist");
        let path = principal.workspace_path.join("principal.toml");
        let mut on_disk: PrincipalConfig =
            toml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        on_disk.preferred_model_id = Some("hand-edited".to_string());
        tokio::fs::write(&path, toml::to_string(&on_disk).unwrap())
            .await
            .unwrap();

        manager
            .update_config("stressy", |config| {
                config.exposure = peko_auth::Exposure::Public;
            })
            .await
            .expect("update_config should succeed");

        let saved: PrincipalConfig =
            toml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(saved.preferred_model_id.as_deref(), Some("hand-edited"));
        assert_eq!(saved.exposure, peko_auth::Exposure::Public);
        assert_eq!(
            principal.config.read().await.preferred_model_id.as_deref(),
            Some("hand-edited")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn boot_state_stamp_preserves_authored_definition_and_publishes_after_persist() {
        use super::super::config::BootState;
        let (_temp, manager, _adapter, id) = setup().await;
        let principal = manager.get(id).await.unwrap();
        let path = principal.workspace_path.join("principal.toml");
        let original = tokio::fs::read_to_string(&path).await.unwrap();
        let mut authored: PrincipalConfig = toml::from_str(&original).unwrap();
        authored.identity.description = Some("Maintain the release evidence".into());
        authored.intent.goals = vec!["Close verified commitments".into()];
        tokio::fs::write(&path, toml::to_string(&authored).unwrap())
            .await
            .unwrap();
        manager
            .persist_boot_state("stressy", BootState::Organized)
            .await
            .unwrap();
        let persisted: PrincipalConfig =
            toml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(
            persisted.identity.description,
            authored.identity.description
        );
        assert_eq!(persisted.intent.goals, authored.intent.goals);
        assert_eq!(persisted.boot_state(), BootState::Organized);
        assert_eq!(
            principal.config.read().await.boot_state(),
            BootState::Organized
        );
        assert_eq!(
            principal.config.read().await.identity.description,
            authored.identity.description
        );

        // A seeder with stale state cannot rewind successful completion.
        manager
            .persist_boot_state("stressy", BootState::GenesisPending)
            .await
            .unwrap();
        let persisted: PrincipalConfig =
            toml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(persisted.boot_state(), BootState::Organized);

        // Invalid authored config stays intact; no success is published.
        principal
            .config
            .write()
            .await
            .set_boot_state(BootState::GenesisPending);
        tokio::fs::write(&path, "invalid = [").await.unwrap();
        assert!(manager
            .persist_boot_state("stressy", BootState::Organized)
            .await
            .is_err());
        assert_eq!(
            principal.config.read().await.boot_state(),
            BootState::GenesisPending
        );
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "invalid = ["
        );
    }

    /// ADR-078 D7: the reviewer reads the policy model's verdict, and the
    /// owner's commands list, deny, and approve requests — each decided
    /// once, with *always* recorded in POLICY.md.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn the_policy_gate_reviews_and_the_owner_decides() {
        use crate::principal::policy_gate::{handle_owner_command, PrincipalPolicyGate};
        use crate::security::judge::{GatedCall, PolicyGate, Verdict};
        use peko_tools_core::{ActingFor, Compartment, Origin, Trust};
        let (_temp, manager, adapter, id) = setup().await;
        let principal = manager.get(id.clone()).await.unwrap();
        let gate = PrincipalPolicyGate::new(Arc::downgrade(&manager));
        let call = GatedCall {
            call_id: Some("call_gated".into()),
            principal_id: id.0.clone(),
            principal_name: None,
            session_id: Some("guest-session".into()),
            agent_id: None,
            workspace: None,
            stamp: ActingFor {
                actor: "user:hub:x".into(),
                name: None,
                trust: Trust::Untrusted,
                compartment: Compartment::Conversation {
                    channel: "dm_x".into(),
                },
                origin: Origin::Ingress { channel: None },
            },
            tool_name: "Read".into(),
            params: serde_json::json!({"file_path": "finances/q3.md"}),
        };

        adapter.queue_text("{\"decision\": \"deny\", \"reason\": \"finances\"}");
        assert_eq!(gate.review(&call).await, Verdict::Deny("finances".into()));
        adapter.queue_text("I think it's fine");
        assert!(matches!(gate.review(&call).await, Verdict::AskOwner(_)));

        let first = gate.ask_owner(&call, "unclear").await.unwrap();
        let listed = handle_owner_command(&manager, &principal, "approvals")
            .await
            .unwrap();
        assert!(
            listed.contains(&first) && listed.contains("user:hub:x"),
            "{listed}"
        );
        assert!(handle_owner_command(&manager, &principal, "hello there")
            .await
            .is_none());
        let denied = handle_owner_command(&manager, &principal, &format!("deny {first}"))
            .await
            .unwrap();
        assert!(denied.starts_with("Denied"), "{denied}");
        let again = handle_owner_command(&manager, &principal, &format!("approve {first}"))
            .await
            .unwrap();
        assert!(again.contains("already decided"), "{again}");

        let second = gate.ask_owner(&call, "unclear").await.unwrap();
        let mut replay = call.clone();
        replay.stamp.origin = Origin::Approval {
            request_id: second.clone(),
        };
        assert!(!gate.is_approved_replay(&replay).await, "not approved yet");
        let approved =
            handle_owner_command(&manager, &principal, &format!("approve {second} always"))
                .await
                .unwrap();
        assert!(approved.starts_with("Approved"), "{approved}");
        assert!(gate.is_approved_replay(&replay).await);
        // ADR-079 D4: the replay's ledger record names the request and the
        // call the owner approved.
        let ledger_dir =
            crate::security::ledger::dir_for_sessions(&principal.memory.sessions_dir());
        let mut replayed = None;
        for _ in 0..50 {
            replayed = crate::security::ledger::read::load_calls(&ledger_dir, None)
                .unwrap_or_default()
                .into_iter()
                .find(|c| {
                    c.start
                        .as_ref()
                        .is_some_and(|s| s["parent"]["kind"] == "approval")
                });
            if replayed.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let replayed = replayed.expect("the approved replay is in the ledger");
        assert_eq!(
            replayed.start.as_ref().unwrap()["parent"],
            serde_json::json!({"kind": "approval", "id": second, "call_id": "call_gated"})
        );
        let mut altered = replay.clone();
        altered.params = serde_json::json!({"file_path": "finances/q4.md"});
        assert!(
            !gate.is_approved_replay(&altered).await,
            "an approval covers exactly the call the owner saw"
        );
        let policy = std::fs::read_to_string(
            principal
                .workspace_path
                .join(crate::security::policy::POLICY_FILE),
        )
        .unwrap();
        assert!(policy.contains("## user:hub:x"), "{policy}");
        assert!(policy.contains("finances/q3.md"), "{policy}");
    }

    /// ADR-081 D8: a cron job's trust is re-derived when it fires and only
    /// lowered, so revoking a grant reaches jobs that already exist.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn cron_trust_drops_when_its_grant_is_revoked() {
        let (_temp, manager, _adapter, id) = setup().await;
        let principal = manager.get(id.clone()).await.unwrap();
        let alice = ActorRef::User("hub:alice".into());
        manager
            .update_config("stressy", |config| {
                config.permissions.push(PermissionGrant {
                    subject: alice.clone(),
                    permission: Permission::Trusted,
                    granted_at: String::new(),
                    granted_by: ActorRef::User("local".into()),
                });
            })
            .await
            .unwrap();
        let job = peko_cron::CronJob {
            id: "job".into(),
            name: "job".into(),
            principal_id: id.clone(),
            schedule: peko_cron::ScheduleKind::Every {
                every_ms: 3_600_000,
            },
            action: peko_cron::CronJobAction::Send {
                message: "ping".into(),
                target: None,
                model: None,
                when: None,
            },
            delete_after_run: false,
            enabled: true,
            created_at: chrono::Utc::now(),
            next_run: chrono::Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
            origin_session: None,
            created_by: Some(peko_tools_core::ActingFor {
                actor: "user:hub:alice".into(),
                name: None,
                trust: peko_tools_core::Trust::Trusted,
                compartment: peko_tools_core::Compartment::Internal,
                origin: peko_tools_core::Origin::Ingress { channel: None },
            }),
        };
        let session = uuid::Uuid::new_v4().to_string();
        let stamp = crate::principal::trust::cron_job_stamp(&principal, None, &job, &session)
            .await
            .unwrap();
        assert_eq!(stamp.trust, peko_tools_core::Trust::Trusted);

        manager
            .update_config("stressy", |config| config.permissions.clear())
            .await
            .unwrap();
        let stamp = crate::principal::trust::cron_job_stamp(&principal, None, &job, &session)
            .await
            .unwrap();
        assert_eq!(stamp.trust, peko_tools_core::Trust::Untrusted);
    }

    /// ADR-078 D2: the trunk acts only for the owner. A non-owner stamp
    /// is refused before any turn starts, whatever its trust.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn trunk_refuses_runs_acting_for_anyone_but_the_owner() {
        use peko_tools_core::{ActingFor, Compartment, Origin, Trust};
        let (_temp, manager, _adapter, id) = setup().await;
        for (trust, compartment) in [
            (Trust::Trusted, Compartment::Internal),
            (Trust::Untrusted, Compartment::Internal),
            (
                Trust::Owner,
                Compartment::Conversation {
                    channel: "dm_x".into(),
                },
            ),
        ] {
            let stamp = ActingFor {
                actor: "user:hub:x".into(),
                name: None,
                trust,
                compartment,
                origin: Origin::Cron {
                    job_id: "cron_x".into(),
                },
            };
            let result = manager
                .receive_trunk(id.clone(), "do it".into(), None, stamp)
                .await;
            assert!(
                matches!(result, Err(PrincipalManagerError::PermissionDenied(_))),
                "{trust:?}: {result:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn receive_denies_unauthorized_user() {
        let (_temp, manager, _adapter, id) = setup().await;
        manager
            .update_config("stressy", |config| {
                config.permissions.clear();
            })
            .await
            .expect("update_config should succeed");
        let stranger = ActorRef::User("stranger".into());
        let result = manager
            .receive_streaming(
                id,
                stranger,
                "hello".to_string(),
                cli_channel(),
                Box::new(|_| {}),
                None,
            )
            .await;
        assert!(
            matches!(result, Err(PrincipalManagerError::PermissionDenied(_))),
            "stranger should be denied, got {result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn receive_allows_owner_and_grant() {
        let (_temp, manager, adapter, id) = setup().await;
        adapter.queue_text("owner reply".to_string());

        // Owner always passes.
        let owner = ActorRef::User("test-owner".into());
        let response = manager
            .receive_streaming(
                id.clone(),
                owner,
                "hello".to_string(),
                cli_channel(),
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("owner should be allowed");
        assert!(response.content.contains("owner reply"));

        // Grant Chat to a specific user.
        manager
            .update_config("stressy", |config| {
                config.permissions.push(PermissionGrant {
                    subject: ActorRef::User("friend".into()),
                    permission: Permission::Chat,
                    granted_at: chrono::Utc::now().to_rfc3339(),
                    granted_by: config.owner.clone(),
                });
            })
            .await
            .unwrap();

        adapter.queue_text("friend reply".to_string());
        let friend = ActorRef::User("friend".into());
        let response = manager
            .receive_streaming(
                id,
                friend,
                "hi".to_string(),
                cli_channel(),
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("grantee should be allowed");
        assert!(response.content.contains("friend reply"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn build_router_context_marks_all_agents_enabled() {
        // ADR-066 P2: presence = spawnability — every role file in the
        // workspace is enabled; no grant check.
        let (temp, manager, _adapter, _id) = setup().await;

        manager.remove("stressy").await.unwrap();

        let principal = create_test_principal_with_agents(
            &manager,
            "stressy",
            &["first_agent", "second_agent"],
        )
        .await;

        let ctx = manager
            .build_router_context(
                &principal,
                ActorRef::User("test-owner".into()),
                "hello".to_string(),
                cli_channel(),
                None,
            )
            .await
            .expect("build_router_context should succeed");

        for id in ["first_agent", "second_agent"] {
            let entry = ctx
                .available_agents
                .iter()
                .find(|a| a.id == id)
                .unwrap_or_else(|| panic!("{id} should be in catalog"));
            assert!(entry.enabled, "{id} is enabled by presence");
        }

        // Suppress unused warning for temp.
        let _ = temp;
    }

    /// Per-message configured model override survives
    /// `build_router_context` and lands on `RouterContext` so the
    /// root router can mirror it onto `PrincipalContext`.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn build_router_context_with_override_model() {
        let (_temp, manager, _adapter, _id) = setup().await;
        let principal = manager.get_by_name("stressy").await.expect("principal");

        let ctx = manager
            .build_router_context(
                &principal,
                ActorRef::User("test-owner".into()),
                "hello".to_string(),
                cli_channel(),
                Some("openai-gpt-4o".to_string()),
            )
            .await
            .expect("build_router_context should succeed");

        assert_eq!(ctx.override_model.as_deref(), Some("openai-gpt-4o"));
    }

    /// When the caller doesn't supply an override, the field stays
    /// `None` so the resolver falls back to the principal's pinned
    /// model.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn build_router_context_without_override_model() {
        let (_temp, manager, _adapter, _id) = setup().await;
        let principal = manager.get_by_name("stressy").await.expect("principal");

        let ctx = manager
            .build_router_context(
                &principal,
                ActorRef::User("test-owner".into()),
                "hello".to_string(),
                cli_channel(),
                None,
            )
            .await
            .expect("build_router_context should succeed");

        assert!(ctx.override_model.is_none());
    }

    // ===================================================================
    // Gap-1: parallel `receive_streaming` calls for the same peer must
    // serialize — arrivals while the peer child's run permit is held
    // queue as SteeringMessages into the CHILD session inbox (Phase 7
    // re-key). Mirrors `concurrent_same_peer_messages_are_queued` for
    // the streaming variant.
    // ===================================================================
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn concurrent_same_peer_streaming_receives_are_queued() {
        let (_temp, manager, adapter, id) = setup().await;
        let messages = 4;
        adapter.queue_text("streaming final".to_string());

        let peer = ActorRef::User("streaming-peer".into());
        let principal = manager
            .get(id.clone())
            .await
            .expect("principal should exist");

        // Hold the peer child's run permit so every concurrent
        // streaming receive deterministically takes the
        // steering-queue branch (see the non-streaming sibling test).
        let child_id = manager
            .ensure_peer_child_ingress(&principal, &peer)
            .await
            .expect("peer child provisioning")
            .child_id;
        let permit_hold = manager
            .inbox_registry
            .try_acquire_run(&child_id)
            .await
            .expect("permit should be free");

        let mut handles = Vec::with_capacity(messages);
        for i in 0..messages {
            let manager = Arc::clone(&manager);
            let id = id.clone();
            let peer = peer.clone();
            let handle = tokio::spawn(async move {
                manager
                    .receive_streaming(
                        id,
                        peer,
                        format!("hello {i}"),
                        cli_channel(),
                        Box::new(|_event| {}),
                        None,
                    )
                    .await
            });
            handles.push(handle);
        }

        let results = futures::future::join_all(handles).await;
        let responses: Vec<PrincipalResponse> = results
            .into_iter()
            .map(|r| {
                r.expect("task should not panic")
                    .expect("receive_streaming should succeed")
            })
            .collect();

        let expected_queued = format!("Queued for root agent session {child_id}.");
        for r in &responses {
            assert_eq!(
                r.content, expected_queued,
                "all concurrent sends should be queued as steering"
            );
        }

        // Release the permit; one streaming receive runs ONE turn that
        // drains the queued steering at its first iteration.
        drop(permit_hold);
        let response = manager
            .receive_streaming(
                id,
                peer,
                "after".to_string(),
                cli_channel(),
                Box::new(|_event| {}),
                None,
            )
            .await
            .expect("receive_streaming should succeed");
        assert!(
            response.content.contains("streaming final"),
            "response should carry the mock reply: {}",
            response.content
        );

        // Exactly one LLM call for the whole batch — the queued
        // messages rode the single run's steering drain.
        let recorded = adapter.recorded_requests();
        assert_eq!(
            recorded.len(),
            1,
            "the single child run should be the only LLM call recorded"
        );
    }

    // ===================================================================
    // Gap-3 (Phase 11 form): a `SteeringMessage` arriving while a run
    // is in flight (queued by `drive_principal_ingress`) is posted to
    // the peer's DM channel
    // (peer-attributed) so the user sees their own message in
    // `peko log`. The handler resolves the peer child + DM channel via
    // `ensure_peer_child_ingress` and posts through
    // `post_peer_dm_inbound` before pushing the inbox item — this test
    // verifies that path persists correctly.
    // ===================================================================
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn steering_message_persists_to_dm_channel() {
        let (_temp, manager, _adapter, id, store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");

        // Mimic the Phase 11 steer handler: one ingress call resolves
        // the child + DM channel, then the steered text is posted
        // inbound (peer-attributed).
        let peer = ActorRef::User("alice".into());
        let steered_text = "wait, do this instead";
        let ingress = manager
            .ensure_peer_child_ingress(&principal, &peer)
            .await
            .expect("ingress");
        let port = manager.channel_port().expect("port attached");
        let dm = ingress.dm_channel.expect("DM channel provisioned");
        crate::principal::peer_dm::post_peer_dm_inbound(
            &port,
            &principal.id,
            &dm,
            &peer.to_string(),
            steered_text,
        )
        .await
        .expect("inbound post");

        let rows = dm_posted_rows(&manager, &store, &principal, &peer).await;
        assert_eq!(
            rows,
            vec![("user:local:alice".to_string(), steered_text.to_string())],
            "exactly one peer-authored row should land on the DM channel"
        );

        // The principal name lookup the gap-3 handler uses must work.
        let resolved = manager.get_by_name("stressy").await;
        assert!(
            resolved.is_some(),
            "principal should be resolvable by name from the Steer handler"
        );

        // Sanity: the principal id round-trips.
        let _ = id;
    }

    /// ADR-085 D4: with only the owner talking to the principal, the
    /// default `memory_review: guests` skips the reviewer; once someone
    /// else speaks it reviews, and `always` / `off` override.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn memory_review_runs_only_while_others_talk_to_the_principal() {
        use crate::security::judge::PolicyGate as _;
        use crate::security::memory_gate::{Claim, ClaimCheck};
        let (_temp, manager, adapter, id, _store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");
        let owner = principal.config.read().await.owner.clone();
        let port = manager.channel_port().expect("port");
        let mine = manager
            .ensure_peer_child_ingress(&principal, &owner)
            .await
            .expect("owner ingress");
        crate::principal::peer_dm::post_peer_dm_inbound(
            &port,
            &principal.id,
            mine.dm_channel.as_ref().expect("owner DM"),
            &owner.to_string(),
            "I like tea",
        )
        .await
        .expect("post");
        let gate =
            crate::principal::policy_gate::PrincipalPolicyGate::new(Arc::downgrade(&manager));
        let claims = vec![Claim {
            entry: "- owner likes tea (source: /local-user)".into(),
            sources: vec!["/local-user".into()],
        }];
        assert!(
            !crate::principal::policy_gate::others_spoke_recently(&principal, Some(port.clone()))
                .await
        );
        assert!(matches!(
            gate.check_claims(&id.0, &claims).await.as_slice(),
            [ClaimCheck::Skipped(_)]
        ));

        let theirs = manager
            .ensure_peer_child_ingress(&principal, &ActorRef::User("alice".into()))
            .await
            .expect("guest ingress");
        crate::principal::peer_dm::post_peer_dm_inbound(
            &port,
            &principal.id,
            theirs.dm_channel.as_ref().expect("guest DM"),
            "user:local:alice",
            "hello",
        )
        .await
        .expect("post");
        assert!(
            crate::principal::policy_gate::others_spoke_recently(&principal, Some(port.clone()))
                .await
        );
        adapter.queue_text("1: supported");
        assert_eq!(
            gate.check_claims(&id.0, &claims).await,
            vec![ClaimCheck::Supported]
        );

        let policy_path = principal
            .workspace_path
            .join(crate::security::policy::POLICY_FILE);
        std::fs::write(&policy_path, "---\nmemory_review: off\n---\n# Policy\n").unwrap();
        assert!(matches!(
            gate.check_claims(&id.0, &claims).await.as_slice(),
            [ClaimCheck::Skipped(_)]
        ));
    }

    /// ADR-085: the reviewer's evidence for a channel source is the cited
    /// event and its neighbours, or the posts sharing words with the claim.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn memory_review_evidence_quotes_the_cited_posts() {
        let (_temp, manager, _adapter, id, store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");
        let owner = principal.config.read().await.owner.clone();
        let ingress = manager
            .ensure_peer_child_ingress(&principal, &owner)
            .await
            .expect("owner ingress");
        let port = manager.channel_port().expect("port");
        let dm = ingress.dm_channel.expect("owner DM");
        for text in [
            "the weather is fine",
            "I drink green tea every morning",
            "bye",
        ] {
            crate::principal::peer_dm::post_peer_dm_inbound(
                &port,
                &principal.id,
                &dm,
                &owner.to_string(),
                text,
            )
            .await
            .expect("post");
        }
        use peko_channel::ChannelPort as _;
        let events = store
            .peek_with_ids(&dm, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        let tea_id = events
            .iter()
            .find(|(_, e)| matches!(e, peko_protocol::channel::ChannelEvent::Posted { text, .. } if text.contains("tea")))
            .map(|(id, _)| id.clone())
            .unwrap();
        let claim = "- owner drinks green tea in the morning";
        let cited = crate::principal::policy_gate::source_excerpt(
            &principal,
            Some(&port),
            &format!("{}#{tea_id}", dm.0),
            claim,
        )
        .await;
        assert!(
            cited.contains("green tea") && cited.contains("weather"),
            "{cited}"
        );
        let searched =
            crate::principal::policy_gate::source_excerpt(&principal, Some(&port), &dm.0, claim)
                .await;
        assert!(
            searched.contains("green tea") && !searched.contains("weather"),
            "{searched}"
        );
        assert!(crate::principal::policy_gate::source_excerpt(
            &principal,
            Some(&port),
            "/no-such-session",
            claim
        )
        .await
        .is_empty());
    }

    /// ADR-085: a session bound to a conversation is quoted from the
    /// conversation, the owner's posts first and marked; any other
    /// session leaves out what the runtime injected into it, so a line
    /// already in `kb/MEMORY.md` cannot vouch for itself.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn memory_review_evidence_leaves_out_runtime_entries() {
        use peko_session::events::MessageSource;
        let (_temp, manager, _adapter, id, _store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");
        let owner = principal.config.read().await.owner.clone();
        let mine = manager
            .ensure_peer_child_ingress(&principal, &owner)
            .await
            .expect("owner ingress");
        let port = manager.channel_port().expect("port");
        let dm = mine.dm_channel.clone().expect("owner DM");
        let me = format!("principal:local:{}", principal.id.0);
        for (author, text) in [
            (owner.to_string(), "the weather is fine"),
            (owner.to_string(), "I drink green tea every morning"),
            (
                me.clone(),
                "Noted: you drink green tea every morning. Green tea every morning, got it.",
            ),
        ] {
            crate::principal::peer_dm::post_peer_dm_inbound(
                &port,
                &principal.id,
                &dm,
                &author,
                text,
            )
            .await
            .expect("post");
        }
        let storage = peko_session::jsonl::SessionStorage::new(principal.memory.sessions_dir());
        let append = |session: String, text: &str, source: MessageSource| {
            let storage = storage.clone();
            let event = peko_session::SessionEvent::MessageV2(
                peko_session::message::SessionMessage::user(text, source),
            );
            async move {
                storage
                    .append_event(&session, &event)
                    .await
                    .expect("append")
            }
        };
        let injected = "<runtime-context>\n## memory\n- owner drinks green tea every morning \
                        (source: /local-user)\n</runtime-context>";
        append(mine.child_id.clone(), injected, MessageSource::Hook).await;
        let claim = "- owner drinks green tea every morning";

        // Bound to the owner's DM: the DM's posts, the owner's marked.
        let bound = crate::principal::policy_gate::source_excerpt(
            &principal,
            Some(&port),
            &mine.child_id,
            claim,
        )
        .await;
        assert!(
            bound.contains(" (owner): I drink green tea every morning")
                && !bound.contains("weather"),
            "{bound}"
        );
        assert!(bound.contains(&me) && !bound.contains(&format!("{me} (owner)")));
        assert!(!bound.contains("runtime-context") && !bound.contains("## memory"));

        // Not bound to any conversation: its own messages, runtime
        // entries left out.
        let mut sessions = peko_session::SessionManager::new()
            .with_sessions_dir_internal(principal.memory.sessions_dir());
        let metas = sessions.list_all_sessions(false).await.unwrap();
        let trunk = metas
            .iter()
            .find(|m| {
                crate::security::compartment_of(&metas, m.session_id)
                    == peko_tools_core::Compartment::Internal
            })
            .expect("a session bound to no conversation")
            .session_id
            .to_string();
        append(trunk.clone(), injected, MessageSource::Hook).await;
        append(
            trunk.clone(),
            "[memory] green tea pass",
            MessageSource::Cron,
        )
        .await;
        append(
            trunk.clone(),
            "green tea note from a helper",
            MessageSource::Agent,
        )
        .await;
        append(
            trunk.clone(),
            "[runtime] Approval request about green tea",
            MessageSource::User,
        )
        .await;
        append(
            trunk.clone(),
            "I drink green tea before nine",
            MessageSource::User,
        )
        .await;
        let unbound =
            crate::principal::policy_gate::source_excerpt(&principal, Some(&port), &trunk, claim)
                .await;
        assert_eq!(unbound, "user: I drink green tea before nine");
    }

    /// ADR-085: `group:<slug>` sources are channels, quoted like any.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn memory_review_evidence_quotes_group_posts() {
        use peko_channel::ChannelPort as _;
        let (_temp, manager, _adapter, id, store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");
        let owner = principal.config.read().await.owner.clone();
        let port = manager.channel_port().expect("port");
        let group = peko_channel::ChannelId::for_group("vendors");
        store
            .create(
                &principal.id,
                peko_channel::CreateOpts::runtime("vendors").with_id(group.clone()),
            )
            .await
            .expect("group");
        for text in ["Harbor Print quoted EUR 1,200", "unrelated"] {
            crate::principal::peer_dm::post_peer_dm_inbound(
                &port,
                &principal.id,
                &group,
                &owner.to_string(),
                text,
            )
            .await
            .expect("post");
        }
        let events = store
            .peek_with_ids(&group, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        let quote = &events
            .iter()
            .find(|(_, e)| matches!(e, peko_protocol::channel::ChannelEvent::Posted { text, .. } if text.contains("Harbor")))
            .unwrap()
            .0;
        let claim = "- Harbor Print quoted EUR 1,200";
        for source in [
            format!("group:vendors#{quote}"),
            "group:vendors".to_string(),
        ] {
            let excerpt = crate::principal::policy_gate::source_excerpt(
                &principal,
                Some(&port),
                &source,
                claim,
            )
            .await;
            assert!(
                excerpt.contains("(owner): Harbor Print quoted EUR 1,200"),
                "{source}: {excerpt}"
            );
        }
    }

    /// ADR-085 D2: thinking counts against the reviewer's cap, so the
    /// budget leaves room for it, and an answer cut off before it covers
    /// every claim is asked once more with more room.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn memory_review_retries_a_cut_off_answer_once() {
        use crate::security::judge::PolicyGate as _;
        use crate::security::memory_gate::{review_budget, Claim, ClaimCheck, REVIEW_RETRY_FACTOR};
        let (_temp, manager, adapter, id, _store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");
        let port = manager.channel_port().expect("port");
        let theirs = manager
            .ensure_peer_child_ingress(&principal, &ActorRef::User("alice".into()))
            .await
            .expect("guest ingress");
        crate::principal::peer_dm::post_peer_dm_inbound(
            &port,
            &principal.id,
            theirs.dm_channel.as_ref().expect("guest DM"),
            "user:local:alice",
            "hello",
        )
        .await
        .expect("post");
        let gate =
            crate::principal::policy_gate::PrincipalPolicyGate::new(Arc::downgrade(&manager));
        let claims = vec![Claim {
            entry: "- owner likes tea (source: /local-user)".into(),
            sources: vec!["/local-user".into()],
        }];
        let cut_off = || {
            peko_providers::MockResponse::Success(peko_provider_api::ChatResponse {
                content: vec![],
                tool_calls: vec![],
                stop_reason: peko_provider_api::StopReason::Length,
                usage: Default::default(),
                provider: "mock".into(),
                model: String::new(),
            })
        };
        let budgets = |adapter: &MockAdapter, from: usize| -> Vec<Option<u32>> {
            adapter.recorded_requests()[from..]
                .iter()
                .map(|r| r.options.max_tokens)
                .collect()
        };

        let before = adapter.recorded_requests().len();
        adapter.queue_chat_response(cut_off());
        adapter.queue_text("1: supported");
        assert_eq!(
            gate.check_claims(&id.0, &claims).await,
            vec![ClaimCheck::Supported]
        );
        let first = review_budget(1);
        assert_eq!(
            budgets(&adapter, before),
            vec![Some(first), Some(first * REVIEW_RETRY_FACTOR)]
        );

        // Cut off twice: no third call, and no finding.
        let before = adapter.recorded_requests().len();
        adapter.queue_chat_response(cut_off());
        adapter.queue_chat_response(cut_off());
        assert!(matches!(
            gate.check_claims(&id.0, &claims).await.as_slice(),
            [ClaimCheck::Unchecked(_)]
        ));
        assert_eq!(budgets(&adapter, before).len(), 2);
    }

    /// ADR-085 D2: a malformed or degenerate answer (observed on MiMo:
    /// `<tool_call><function思った><r><r>…`) is asked once more with the
    /// format restated, at the same budget; the retry fills only the
    /// claims the first answer left without a finding.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn memory_review_retries_a_malformed_answer_once() {
        use crate::security::judge::PolicyGate as _;
        use crate::security::memory_gate::{review_budget, Claim, ClaimCheck};
        let (_temp, manager, adapter, id, _store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");
        let port = manager.channel_port().expect("port");
        let theirs = manager
            .ensure_peer_child_ingress(&principal, &ActorRef::User("alice".into()))
            .await
            .expect("guest ingress");
        crate::principal::peer_dm::post_peer_dm_inbound(
            &port,
            &principal.id,
            theirs.dm_channel.as_ref().expect("guest DM"),
            "user:local:alice",
            "hello",
        )
        .await
        .expect("post");
        let gate =
            crate::principal::policy_gate::PrincipalPolicyGate::new(Arc::downgrade(&manager));
        let claim = |entry: &str| Claim {
            entry: entry.into(),
            sources: vec!["/local-user".into()],
        };
        let claims = vec![
            claim("- owner likes tea (source: /local-user)"),
            claim("- owner lives in Porto (source: /local-user)"),
        ];

        let before = adapter.recorded_requests().len();
        adapter.queue_text("<tool_call><function><r><r><r></function></tool_call>");
        adapter.queue_text("1: supported\n2: unsupported - not in the excerpt");
        assert_eq!(
            gate.check_claims(&id.0, &claims).await,
            vec![
                ClaimCheck::Supported,
                ClaimCheck::Unsupported("not in the excerpt".into())
            ]
        );
        let requests = &adapter.recorded_requests()[before..];
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].options.max_tokens, Some(review_budget(2)));
        assert!(format!("{:?}", requests[1].messages).contains("could not be read"));

        // The first answer's findings stand; the retry only fills gaps.
        let before = adapter.recorded_requests().len();
        adapter.queue_text("1: unsupported - a guess\nnoise");
        adapter.queue_text("1: supported\n2: supported");
        assert_eq!(
            gate.check_claims(&id.0, &claims).await,
            vec![
                ClaimCheck::Unsupported("a guess".into()),
                ClaimCheck::Supported
            ]
        );
        assert_eq!(adapter.recorded_requests().len() - before, 2);

        // A complete first answer is not asked again.
        let before = adapter.recorded_requests().len();
        adapter.queue_text("<n>: supported\n<n>: supported");
        assert_eq!(
            gate.check_claims(&id.0, &claims).await,
            vec![ClaimCheck::Supported, ClaimCheck::Supported]
        );
        assert_eq!(adapter.recorded_requests().len() - before, 1);
    }

    /// ADR-080 D4: a hot-memory source counts as the owner's only when
    /// nobody else speaks in it.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn memory_sources_are_owner_only_when_only_the_owner_speaks() {
        use crate::security::judge::PolicyGate as _;
        use crate::security::memory_gate::SourceClass;
        let (_temp, manager, _adapter, id, _store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");
        let owner = principal.config.read().await.owner.clone();
        let mine = manager
            .ensure_peer_child_ingress(&principal, &owner)
            .await
            .expect("owner ingress");
        let theirs = manager
            .ensure_peer_child_ingress(&principal, &ActorRef::User("alice".into()))
            .await
            .expect("guest ingress");
        let gate =
            crate::principal::policy_gate::PrincipalPolicyGate::new(Arc::downgrade(&manager));
        let class = |source: String| {
            let gate = &gate;
            let id = id.0.clone();
            async move { gate.source_class(&id, &source).await }
        };

        assert_eq!(class(mine.child_id.clone()).await, SourceClass::Owner);
        let my_dm = mine.dm_channel.expect("owner DM").0;
        assert_eq!(class(format!("{my_dm}#evt_1")).await, SourceClass::Owner);
        let their_dm = theirs.dm_channel.expect("guest DM").0;
        assert!(matches!(
            class(theirs.child_id.clone()).await,
            SourceClass::Other(peko_tools_core::Compartment::Conversation { channel }) if channel == their_dm
        ));
        assert!(matches!(
            class(their_dm.clone()).await,
            SourceClass::Other(_)
        ));
        for unknown in [
            "/no-such-session",
            "chan_nope",
            "00000000-0000-0000-0000-000000000001",
        ] {
            assert_eq!(
                class(unknown.into()).await,
                SourceClass::Unknown,
                "{unknown}"
            );
        }
    }

    // ===================================================================
    // Sprint 3 Phase 11: a peer turn driven through `receive` posts the
    // inbound message (peer-attributed) and the reply
    // (principal-authored) to the peer's DM channel, and the recall
    // artifact points at the child session id.
    // ===================================================================
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn peer_child_turn_posts_dm_channel_and_recall_artifact() {
        let (_temp, manager, adapter, id, store) = setup_with_channels().await;
        adapter.queue_text("hello alice".to_string());
        let principal = manager.get(id.clone()).await.expect("principal");

        let peer = ActorRef::User("alice".into());
        let response = manager
            .receive_streaming(
                id,
                peer.clone(),
                "hello principal".to_string(),
                cli_channel(),
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("receive should succeed");
        assert!(response.content.contains("hello alice"));

        // Both directions land on the DM channel: inbound attributed
        // to the peer (ActorRef wire form), reply authored by the
        // principal's raw id.
        let rows = dm_posted_rows(&manager, &store, &principal, &peer).await;
        assert_eq!(
            rows,
            vec![
                (
                    "user:local:alice".to_string(),
                    "hello principal".to_string()
                ),
                (
                    format!("principal:local:{}", principal.id.0),
                    "hello alice".to_string()
                ),
            ],
            "inbound (peer-attributed) + reply (principal-authored) rows"
        );

        // Recall: the peer's latest-session artifact points at the
        // child session id.
        let child_id = manager
            .ensure_peer_child_ingress(&principal, &peer)
            .await
            .expect("peer child")
            .child_id;
        let artifact = principal
            .memory
            .find_latest_session_for_peer(&peer)
            .await
            .expect("recall read")
            .expect("recall artifact exists");
        assert_eq!(
            artifact.session_id, child_id,
            "recall artifact must point at the peer-child session"
        );
        assert!(artifact
            .summary
            .as_deref()
            .is_some_and(|s| s.contains("hello alice")));
    }

    /// Phase 11 queued path: with a channel port attached, every
    /// queued arrival's inbound message is posted to the DM channel
    /// exactly once, and no "Queued…" notice row lands (transport UX,
    /// not conversation).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn queued_messages_post_inbound_once_and_no_notice_row() {
        let (_temp, manager, _adapter, id, store) = setup_with_channels().await;
        let principal = manager.get(id.clone()).await.expect("principal");
        let peer = ActorRef::User("queued-peer".into());

        // Hold the child run permit so every receive takes the
        // steering-queue branch (mirrors
        // `concurrent_same_peer_messages_are_queued`).
        let child_id = manager
            .ensure_peer_child_ingress(&principal, &peer)
            .await
            .expect("peer child provisioning")
            .child_id;
        let _permit_hold = manager
            .inbox_registry
            .try_acquire_run(&child_id)
            .await
            .expect("permit should be free");

        for i in 0..2 {
            let response = manager
                .receive_streaming(
                    id.clone(),
                    peer.clone(),
                    format!("hello {i}"),
                    cli_channel(),
                    Box::new(|_| {}),
                    None,
                )
                .await
                .expect("receive should succeed");
            assert!(
                response
                    .content
                    .starts_with("Queued for root agent session"),
                "expected the queued branch; got: {}",
                response.content
            );
        }

        let rows = dm_posted_rows(&manager, &store, &principal, &peer).await;
        assert_eq!(
            rows,
            vec![
                ("user:local:queued-peer".to_string(), "hello 0".to_string()),
                ("user:local:queued-peer".to_string(), "hello 1".to_string()),
            ],
            "each inbound posted exactly once; no Queued… notice rows"
        );
    }

    // ===================================================================
    // Sprint 2 Phase 7: all external peer ingress lands in per-peer
    // standing children of the trunk.
    // ===================================================================

    /// Assert that no retired per-peer root JSONL (`root:{peer}` /
    /// `root:cron:*`) exists under the principal's sessions dir.
    fn assert_no_retired_root_jsonl(sessions_dir: &std::path::Path) {
        for entry in std::fs::read_dir(sessions_dir).expect("sessions dir readable") {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with("root:") || name == "root:self.jsonl",
                "retired per-peer root session file must never be created: {name}"
            );
        }
    }

    /// The CLI owner (`user:local`) lands in the privileged
    /// `/local-user` standing child of the trunk: the turn's JSONL
    /// carries the exchange, the child is parented at `root:self`, and
    /// no `root:user:local` session is created.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn receive_lands_in_privileged_local_user_child() {
        let (_temp, manager, adapter, id) = setup().await;
        adapter.queue_text("hi owner".to_string());

        // Production shape: the CLI user IS the principal's owner
        // (`user:local`), so its child is the privileged one.
        manager
            .update_config("stressy", |config| {
                config.owner = ActorRef::User("local".into());
            })
            .await
            .expect("update_config should succeed");

        let peer = ActorRef::User("local".into());
        let response = manager
            .receive_streaming(
                id.clone(),
                peer.clone(),
                "hello".to_string(),
                cli_channel(),
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("receive should succeed");
        assert!(response.content.contains("hi owner"));

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions_dir = principal.memory.sessions_dir().clone();

        // The peer child: standing, privileged (owner), parented at
        // the trunk, stamped with the real peer.
        let mut mgr = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir.clone());
        let metas = mgr.list_all_sessions(false).await.unwrap();
        let child_id = crate::principal::peer_children::find_peer_child(&metas, &peer)
            .expect("owner peer child exists");
        let child = metas
            .iter()
            .find(|m| m.session_id.to_string() == child_id)
            .expect("child metadata");
        assert_eq!(child.slug.as_deref(), Some("local-user"));
        assert_eq!(
            child.parent_session_id.map(|id| id.to_string()).as_deref(),
            Some(
                peko_session::SessionId::from("root:self")
                    .to_string()
                    .as_str()
            )
        );

        // The exchange landed in the child JSONL.
        let jsonl = std::fs::read_to_string(sessions_dir.join(format!("{child_id}.jsonl")))
            .expect("child JSONL exists");
        assert!(jsonl.contains("hello"), "child JSONL carries the input");
        assert!(jsonl.contains("hi owner"), "child JSONL carries the reply");

        assert_no_retired_root_jsonl(&sessions_dir);
    }

    /// An A2A peer (`principal:{did}`) lands in a NON-privileged
    /// `/principal-{fragment}` standing child — the subtree-scoped
    /// stranger shape.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn receive_a2a_peer_creates_principal_child() {
        let (_temp, manager, adapter, id) = setup().await;
        adapter.queue_text("a2a reply".to_string());

        let peer = ActorRef::Principal("hub:a2a-peer".into());
        manager
            .channel_port()
            .unwrap()
            .register_actor(
                &peer,
                Some("a2a-peer"),
                Some("did:key:z6MkA2aPeerExample"),
                Some("a2a-peer-key"),
                None,
                peko_channel::Registration::Public,
            )
            .await
            .unwrap();
        let channel = ChannelContext {
            kind: ChannelKind::Cli,
            streaming: false,
        };
        let response = manager
            .receive_streaming(
                id.clone(),
                peer.clone(),
                "ping".to_string(),
                channel,
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("receive should succeed");
        assert!(response.content.contains("a2a reply"));

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions_dir = principal.memory.sessions_dir().clone();
        let mut mgr = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir.clone());
        let metas = mgr.list_all_sessions(false).await.unwrap();
        let child_id = crate::principal::peer_children::find_peer_child(&metas, &peer)
            .expect("A2A peer child exists");
        let child = metas
            .iter()
            .find(|m| m.session_id.to_string() == child_id)
            .expect("child metadata");
        assert!(
            child.slug.as_deref().unwrap().starts_with("principal-"),
            "A2A child slug must be /principal-{{fragment}}, got {:?}",
            child.slug
        );

        assert!(
            child.slug.is_some(),
            "a stranger's child still gets a provisioned session"
        );
        assert!(child.peer_type.is_none() && child.peer_id.is_none());
        assert!(child.channel_id.is_some());

        let jsonl = std::fs::read_to_string(sessions_dir.join(format!("{child_id}.jsonl")))
            .expect("child JSONL exists");
        assert!(jsonl.contains("ping"), "child JSONL carries the input");

        assert_no_retired_root_jsonl(&sessions_dir);
    }

    /// `receive_streaming` drives the peer-child turn and forwards
    /// `AgenticEvent`s (assistant text/deltas) to the caller's sink.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn receive_streaming_streams_from_peer_child() {
        let (_temp, manager, adapter, id) = setup().await;
        adapter.queue_text("streamed reply".to_string());

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_sink = Arc::clone(&events);
        let peer = ActorRef::User("alice".into());
        let response = manager
            .receive_streaming(
                id.clone(),
                peer.clone(),
                "stream me".to_string(),
                cli_channel(),
                Box::new(move |event| events_sink.lock().unwrap().push(event)),
                None,
            )
            .await
            .expect("receive_streaming should succeed");
        assert!(response.content.contains("streamed reply"));

        assert!(
            events.lock().unwrap().iter().any(|e| matches!(
                e,
                peko_engine::AgenticEvent::AssistantText { .. }
                    | peko_engine::AgenticEvent::AssistantDelta { .. }
            )),
            "the sink must see assistant text events; got {} events",
            events.lock().unwrap().len()
        );

        // The turn landed in alice's standing child.
        let principal = manager.get(id).await.expect("principal should exist");
        let sessions_dir = principal.memory.sessions_dir().clone();
        let mut mgr = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir.clone());
        let metas = mgr.list_all_sessions(false).await.unwrap();
        let child_id = crate::principal::peer_children::find_peer_child(&metas, &peer)
            .expect("alice's peer child exists");
        let jsonl = std::fs::read_to_string(sessions_dir.join(format!("{child_id}.jsonl")))
            .expect("child JSONL exists");
        assert!(jsonl.contains("stream me"));
        assert_no_retired_root_jsonl(&sessions_dir);
    }

    // ===================================================================
    // Live definition: edits to roles/root.md and principal.toml apply
    // to the next turn without a daemon restart.
    // ===================================================================

    /// Whether the last recorded LLM request carried `text` anywhere in
    /// its messages (the system prompt holds the root role).
    fn last_request_contains(adapter: &MockAdapter, text: &str) -> bool {
        let requests = adapter.recorded_requests();
        let last = requests.last().expect("a request was recorded");
        format!("{:?}", last.messages).contains(text)
    }

    /// Append `text` to the principal's root role, as a person editing
    /// the file would.
    async fn append_to_root_role(principal: &Principal, text: &str) {
        append_to_role(principal, "root", text).await;
    }

    /// Append `text` to `roles/<role>.md`.
    async fn append_to_role(principal: &Principal, role: &str, text: &str) {
        let path = principal
            .workspace_path
            .join("roles")
            .join(format!("{role}.md"));
        let mut body = tokio::fs::read_to_string(&path).await.expect("role file");
        body.push_str(&format!("\n{text}\n"));
        tokio::fs::write(&path, body).await.unwrap();
    }

    /// Set `preferred_model_id` in the principal's `principal.toml` on
    /// disk, as a person editing the file would.
    async fn set_model_on_disk(principal: &Principal, model: &str) {
        let path = principal.workspace_path.join("principal.toml");
        let mut doc: toml::Value =
            toml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        doc.as_table_mut()
            .unwrap()
            .insert("preferred_model_id".into(), model.into());
        tokio::fs::write(&path, toml::to_string(&doc).unwrap())
            .await
            .unwrap();
    }

    /// A conversation's turn bundle is cached for the principal's
    /// lifetime. Before, it kept the role and model it was built with,
    /// so an edit to either file showed up only after a daemon restart.
    /// Conversations run the `conversation` role (ADR-087).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn conversation_turns_pick_up_role_and_model_edits() {
        let (_temp, manager, adapter, id) = setup().await;
        let principal = manager.get(id.clone()).await.unwrap();
        let peer = ActorRef::User("alice".into());
        let send = |message: &'static str| {
            let manager = Arc::clone(&manager);
            let id = id.clone();
            let peer = peer.clone();
            async move {
                manager
                    .receive_streaming(
                        id,
                        peer,
                        message.to_string(),
                        cli_channel(),
                        Box::new(|_| {}),
                        None,
                    )
                    .await
            }
        };
        let marker = "Always sign off as the edited conversation role.";

        adapter.queue_text("first");
        send("one").await.expect("first turn");
        assert!(!last_request_contains(&adapter, marker));

        append_to_role(&principal, "conversation", marker).await;
        adapter.queue_text("second");
        send("two").await.expect("second turn");
        assert!(
            last_request_contains(&adapter, marker),
            "the turn after the edit must carry the new conversation role"
        );

        // A model the catalog does not have: the next turn resolves no
        // provider, where the stale cached one would have answered.
        set_model_on_disk(&principal, "not-a-configured-model").await;
        adapter.queue_text("fourth");
        let before = adapter.recorded_requests().len();
        let err = send("three").await.expect_err("unknown model must fail");
        assert!(
            format!("{err:?}").contains("no model configured"),
            "{err:?}"
        );
        assert_eq!(adapter.recorded_requests().len(), before);
        assert_eq!(
            principal.config.read().await.preferred_model_id.as_deref(),
            Some("not-a-configured-model")
        );

        set_model_on_disk(&principal, "mock").await;
        let response = send("four").await.expect("model restored");
        assert!(response.content.contains("fourth"), "{}", response.content);
        assert!(last_request_contains(&adapter, marker));
    }

    /// The trunk (cron and owner turns in `root:self`) reads the root
    /// role and model per turn too.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn trunk_turns_pick_up_root_role_and_model_edits() {
        let (_temp, manager, adapter, id) = setup().await;
        let principal = manager.get(id.clone()).await.unwrap();
        let tick = || {
            manager.receive_trunk(
                id.clone(),
                "tick".to_string(),
                None,
                peko_tools_core::ActingFor::runtime(),
            )
        };
        let marker = "Trunk edit: check the inbox first.";

        adapter.queue_text("first");
        tick().await.expect("first trunk turn");
        assert!(!last_request_contains(&adapter, marker));

        append_to_root_role(&principal, marker).await;
        adapter.queue_text("second");
        tick().await.expect("second trunk turn");
        assert!(
            last_request_contains(&adapter, marker),
            "the trunk turn after the edit must carry the new root role"
        );

        set_model_on_disk(&principal, "not-a-configured-model").await;
        adapter.queue_text("third");
        let before = adapter.recorded_requests().len();
        let err = tick().await.expect_err("unknown model must fail");
        assert!(
            format!("{err:?}").contains("No provider configured"),
            "{err:?}"
        );
        assert_eq!(adapter.recorded_requests().len(), before);
    }

    /// When no role can be read at turn time (mid-save, deleted), a
    /// conversation keeps the last good one instead of failing the turn.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn missing_roles_keep_the_last_good_one() {
        let (_temp, manager, adapter, id) = setup().await;
        let principal = manager.get(id.clone()).await.unwrap();
        let marker = "Last good conversation role.";
        append_to_role(&principal, "conversation", marker).await;
        let peer = ActorRef::User("alice".into());

        adapter.queue_text("first");
        manager
            .receive_streaming(
                id.clone(),
                peer.clone(),
                "one".into(),
                cli_channel(),
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("first turn");
        assert!(last_request_contains(&adapter, marker));

        for role in ["conversation.md", "root.md"] {
            tokio::fs::remove_file(principal.workspace_path.join("roles").join(role))
                .await
                .unwrap();
        }
        adapter.queue_text("second");
        manager
            .receive_streaming(
                id,
                peer,
                "two".into(),
                cli_channel(),
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("missing roles must not fail the turn");
        assert!(last_request_contains(&adapter, marker));
    }

    /// A `principal.toml` that no longer parses keeps the loaded config.
    /// Identity fields keep their loaded values even when the file
    /// changes them.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn config_reload_keeps_identity_and_survives_bad_edits() {
        let (_temp, manager, _adapter, id) = setup().await;
        let principal = manager.get(id.clone()).await.unwrap();
        let path = principal.workspace_path.join("principal.toml");
        let loaded = principal.config.read().await.clone();

        tokio::fs::write(&path, "this is = = not toml")
            .await
            .unwrap();
        principal.reload_config_if_changed().await;
        assert_eq!(
            principal.config.read().await.preferred_model_id,
            loaded.preferred_model_id
        );

        let mut edited = loaded.clone();
        edited.name = "renamed".into();
        edited.id = Some(PrincipalId::generate());
        edited.preferred_model_id = Some("other".into());
        tokio::fs::write(&path, toml::to_string(&edited).unwrap())
            .await
            .unwrap();
        principal.reload_config_if_changed().await;
        let config = principal.config.read().await;
        assert_eq!(config.preferred_model_id.as_deref(), Some("other"));
        assert_eq!(config.name, loaded.name);
        assert_eq!(config.id, loaded.id);
        assert_eq!(config.did, loaded.did);
    }

    // ===================================================================
    // Phase 3 (2026-08-15): principal trunk session `root:self`
    // ===================================================================

    /// A trunk turn runs in `root:self` — never `root:{owner}` or
    /// `root:cron:{owner}` — and the trunk session JSONL is the
    /// durable record (no separate consumer projection exists since
    /// the chat-log store was retired in Phase 13).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn trunk_receive_uses_trunk_session() {
        let temp = TempDir::new().expect("temp dir");
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let tool_runtime = ToolRuntime::with_workspace(path_resolver.clone(), temp.path())
            .await
            .expect("tool runtime should initialize");

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("models.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        adapter.queue_text("trunk reply");

        let manager = PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::async_exec::executor::standalone_inbox_registry(),
        )
        .with_tooling(tool_runtime.tooling().clone())
        .with_resolver(resolver);
        let principal = create_test_principal(&manager, "stressy").await;
        let id = principal.id.clone();

        let response = manager
            .receive_trunk(
                id.clone(),
                "trunk tick".to_string(),
                None,
                peko_tools_core::ActingFor::runtime(),
            )
            .await
            .expect("trunk receive should succeed");
        assert!(
            response.content.contains("trunk reply"),
            "response should carry the mock reply: {}",
            response.content
        );

        // The turn landed in the trunk session, not in any per-peer
        // root session. Sprint 6: the trunk is a UUID; look it up by
        // walking `parent_session_id == None`.
        let sessions_dir = principal.memory.sessions_dir().clone();
        let mut sm = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir.clone());
        let metas = sm.list_all_sessions(false).await.unwrap();
        let trunk_id = metas
            .iter()
            .find(|m| m.parent_session_id.is_none())
            .map(|m| m.session_id.to_string())
            .expect("trunk exists");
        let trunk_jsonl = sessions_dir.join(format!("{trunk_id}.jsonl"));
        assert!(trunk_jsonl.exists(), "trunk session JSONL should exist");
        let content = std::fs::read_to_string(&trunk_jsonl).unwrap();
        assert!(
            content.contains("trunk tick"),
            "trunk turn should be persisted to trunk, got: {content}"
        );
    }

    /// A second trunk turn arriving while a trunk run is active is
    /// queued as a steering message into the trunk inbox — a rapid cron
    /// tick steers the live run instead of erroring (same run
    /// discipline as `receive`).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn trunk_receive_queues_when_run_active() {
        let (_temp, manager, adapter, id) = setup().await;
        adapter.queue_text("unused — the queued path makes no LLM call");

        // Hold the trunk run permit: the next trunk turn must steer.
        // The permit key is the principal-scoped trunk key — it must
        // match what `receive_trunk` looks up.
        let _permit = manager
            .inbox_registry
            .try_acquire_run(&peko_session::inbox_key(
                &id.0,
                &super::super::routers::root::trunk_session_id(),
            ))
            .await
            .expect("permit should be free");

        let response = manager
            .receive_trunk(
                id,
                "second tick".to_string(),
                None,
                peko_tools_core::ActingFor::runtime(),
            )
            .await
            .expect("trunk receive should succeed");
        assert!(
            response.content.contains("Queued for root agent session"),
            "expected the queued-steering response, got: {}",
            response.content
        );
    }

    /// Two principals in one manager share the daemon-wide
    /// `InboxRegistry`, and the trunk id is the same constant for both.
    /// Principal A's busy trunk must neither refuse principal B's trunk
    /// turn (queueing it where A's trunk drains it) nor have its queued
    /// steering drained by B's trunk run.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn trunk_admission_and_inbox_are_per_principal() {
        use super::super::routers::root::trunk_session_id;
        use peko_session::inbox_key;
        let (_temp, manager, adapter, a) = setup().await;
        let b = create_test_principal(&manager, "other").await.id.clone();
        let trunk = trunk_session_id();
        let a_key = inbox_key(&a.0, &trunk);
        let b_key = inbox_key(&b.0, &trunk);

        // A's trunk is busy, with a supervision tick already queued for it.
        let _a_permit = manager
            .inbox_registry
            .try_acquire_run(&a_key)
            .await
            .expect("A's trunk permit should be free");
        let a_queued = manager
            .receive_trunk(
                a.clone(),
                "Supervision tick for A".to_string(),
                None,
                peko_tools_core::ActingFor::runtime(),
            )
            .await
            .expect("A's trunk receive should succeed");
        assert!(
            a_queued.content.contains("Queued for root agent session"),
            "A is busy, so its tick queues: {}",
            a_queued.content
        );

        // B's cron Send runs a real turn instead of queueing behind A.
        adapter.queue_text("b trunk reply");
        let b_response = manager
            .receive_trunk(
                b.clone(),
                "Supervision tick for B".to_string(),
                None,
                peko_tools_core::ActingFor::runtime(),
            )
            .await
            .expect("B's trunk receive should succeed");
        assert!(
            b_response.content.contains("b trunk reply"),
            "B's turn must run, not queue behind A's busy trunk: {}",
            b_response.content
        );

        // A's tick is still waiting for A; B's run did not drain it.
        let a_inbox = manager
            .inbox_registry
            .peek_inbox(&a_key)
            .await
            .expect("A's trunk inbox exists");
        let a_items = a_inbox.drain_all().await;
        assert_eq!(a_items.len(), 1, "exactly A's own tick stays queued for A");
        match &a_items[0] {
            peko_session::AsyncInboxItem::Steering(s) => {
                assert!(s.content.contains("Supervision tick for A"));
            }
            other => panic!("expected A's steering tick, got {other:?}"),
        }

        // B's transcript carries B's tick and none of A's.
        let b_principal = manager.get(b.clone()).await.expect("B exists");
        let b_jsonl = std::fs::read_to_string(
            b_principal
                .memory
                .sessions_dir()
                .join(format!("{trunk}.jsonl")),
        )
        .expect("B's trunk JSONL exists");
        assert!(b_jsonl.contains("Supervision tick for B"));
        assert!(
            !b_jsonl.contains("Supervision tick for A"),
            "A's tick must never reach B's trunk"
        );

        // Both busy: B's next tick queues for B only.
        let _b_permit = manager
            .inbox_registry
            .try_acquire_run(&b_key)
            .await
            .expect("B's trunk permit is free again after its turn");
        manager
            .receive_trunk(
                b.clone(),
                "second tick for B".to_string(),
                None,
                peko_tools_core::ActingFor::runtime(),
            )
            .await
            .expect("B's trunk receive should succeed");
        assert!(
            a_inbox.is_empty().await,
            "B's tick must not land in A's inbox"
        );
        let b_inbox = manager
            .inbox_registry
            .peek_inbox(&b_key)
            .await
            .expect("B's trunk inbox exists");
        assert_eq!(b_inbox.len().await, 1, "B's tick queues for B");
        assert!(
            manager.inbox_registry.peek_inbox(&trunk).await.is_none(),
            "nothing is keyed by the bare, shared trunk id"
        );
    }
}
