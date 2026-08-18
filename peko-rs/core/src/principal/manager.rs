use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{
    factory::{PrincipalMemoryFactory, PrincipalRouterFactory},
    router::{ChannelContext, ChannelKind, RouteDecision, RouterContext, RouterError},
    slash::{SlashDispatcher, SlashError},
    Principal, PrincipalId,
};
use crate::common::paths::PathResolver;
use crate::extensions::agent::AgentAdapter;
use crate::extensions::framework::store::ExtensionStore;
use crate::principal::agent_prompt::load_agent_prompt;
use crate::principal::runtime::OutputFormat;
use crate::principal::AgentPrompt;
use crate::principal::PrincipalConfig;
use peko_auth::ownership::{check_permission, Permission, Resource};
use peko_auth::Subject;
use peko_extension_api::SteeringMessage;
use peko_identity::did::DIDScope;
use peko_identity::storage::KeyStorage;
use peko_observability::Observability;
use peko_plan::{PlanNodeStatus, PlanRecord};
use peko_providers::LlmResolver;
use peko_session::InboxRegistry;
use peko_subject::PrincipalDID;

/// Maximum number of resumed plans injected into the router context at
/// session start. Bounds token burn per turn when a principal has many
/// stale open plans; `load_resumable` returns them ordered by
/// `updated_at DESC`, then we cap here.
const PLAN_INJECTION_CAP: usize = 5;

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
    #[error("slash command error: {0}")]
    Slash(#[from] SlashError),
}

/// Owns all Principals in a runtime.
pub struct PrincipalManager {
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
    /// Optional slash-command dispatcher. When set, incoming messages are
    /// inspected for `/`-prefixed slash commands before reaching the root
    /// agent. This is optional so tests and non-daemon contexts can build a
    /// PrincipalManager without extension state.
    slash_dispatcher: Arc<RwLock<Option<Arc<SlashDispatcher>>>>,
    /// Optional daemon extension store. When present, the per-message
    /// `ExtensionCatalog` includes installed extensions as well as built-ins
    /// and principal-scoped agents.
    extension_store: Option<Arc<ExtensionStore>>,
    /// Optional observability hub. Threaded into `RouterContext` so the root
    /// agent and subagent spawns can emit audit events.
    observability: Option<Arc<Observability>>,
    /// F20: optional per-peer quota registry. When `Some`, callers can
    /// resolve a peer's quota meter via
    /// [`PrincipalManager::get_or_create_peer`] and stack it alongside
    /// the principal's meter. `None` for tests / contexts that don't
    /// have a daemon-managed `PeerRegistry`.
    peer_registry: Option<Arc<crate::principal::peer::PeerRegistry>>,
    /// Runtime-owned, append-only chat-log store. When present, every
    /// accepted peer chat-channel message is recorded alongside its
    /// authoritative response. Pure peer-chat channels
    /// (Cli/Http/Hub/A2a/P2p/Webhook) are persisted; automation
    /// channels (Cron/FileWatch) are excluded. `None` for tests /
    /// non-daemon contexts that don't need consumer-visible history.
    chat_log_store: Option<Arc<peko_chat_log::ChatLogStore>>,
    /// Sprint 2 Phase 7: per-principal peer-child turn bundles
    /// (`PeerChildTurns`), built lazily on first peer ingress and
    /// cached for the principal's lifetime. The bundle owns the
    /// persona-carrying `SubagentExecutor` + the session manager the
    /// principal's peer children live in. Build failures (no
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
    /// Phase 10: kickoff hook fired when a peer's DM channel is
    /// freshly created (the daemon installs a closure forwarding to
    /// `ChannelBindingSupervisor::ensure_subscriber` AFTER the
    /// supervisor is built — the supervisor needs the `Arc` of this
    /// manager first, so this is a post-construction install behind
    /// interior mutability, not a builder arg).
    dm_subscriber_hook: std::sync::RwLock<Option<crate::principal::peer_dm::PeerDmSubscriberHook>>,
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
        Self {
            principals: RwLock::new(HashMap::new()),
            principals_by_name: RwLock::new(HashMap::new()),
            path_resolver,
            memory_factory,
            router_factory,
            resolver: None,
            inbox_registry,
            session_creation_locks: tokio::sync::RwLock::new(HashMap::new()),
            slash_dispatcher: Arc::new(RwLock::new(None)),
            extension_store: None,
            observability: None,
            peer_registry: None,
            chat_log_store: None,
            peer_child_turns: tokio::sync::RwLock::new(HashMap::new()),
            channel_port: None,
            dm_subscriber_hook: std::sync::RwLock::new(None),
        }
    }

    pub fn with_resolver(mut self, resolver: Arc<LlmResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Attach a slash-command dispatcher. The dispatcher is shared by all
    /// principals managed by this instance.
    #[must_use]
    pub fn with_slash_dispatcher(mut self, dispatcher: Arc<SlashDispatcher>) -> Self {
        self.slash_dispatcher = Arc::new(RwLock::new(Some(dispatcher)));
        self
    }

    /// Attach a daemon extension store. When present, the per-message
    /// `ExtensionCatalog` includes installed extensions alongside built-ins
    /// and principal-scoped agents.
    #[must_use]
    pub fn with_extension_store(mut self, extension_store: Arc<ExtensionStore>) -> Self {
        self.extension_store = Some(extension_store);
        self
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

    /// Attach the runtime-owned chat-log store. When attached, peer
    /// chat-channel messages accepted by `receive`/`receive_streaming`
    /// are persisted to the chat log alongside their authoritative
    /// response. `None` (the default) disables recording and is the
    /// right choice for tests / non-daemon contexts.
    #[must_use]
    pub fn with_chat_log_store(mut self, chat_log_store: Arc<peko_chat_log::ChatLogStore>) -> Self {
        self.chat_log_store = Some(chat_log_store);
        self
    }

    /// Optional reference to the attached chat-log store.
    #[must_use]
    pub fn chat_log_store(&self) -> Option<&Arc<peko_chat_log::ChatLogStore>> {
        self.chat_log_store.as_ref()
    }

    /// Sprint 3 Phase 10: attach the daemon-global channel port. When
    /// attached, peer ingress (`PeerChildTurns::ensure_child`)
    /// auto-provisions the peer's DM channel alongside its standing
    /// child session. `None` (the default) disables provisioning —
    /// the right choice for tests / non-daemon contexts.
    #[must_use]
    pub fn with_channel_port(mut self, channel_port: Arc<dyn peko_channel::ChannelPort>) -> Self {
        self.channel_port = Some(channel_port);
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
        {
            let by_name = self.principals_by_name.read().await;
            if by_name.contains_key(&name) {
                return Err(PrincipalManagerError::AlreadyExists(name));
            }
        }

        let id = PrincipalId::generate();
        let layout = self.path_resolver.principal_layout(&name);
        self.path_resolver.ensure_principal_dirs(&name)?;

        // Generate and persist a real DID identity for this Principal.
        let mut config = config;
        let identity = self.generate_identity(&name).await?;
        config.did = Some(PrincipalDID(identity.did));

        // Persist principal.toml under the Shared root.
        self.persist_config(&layout.shared.root, &config).await?;

        let memory = self.memory_factory.create(&id, &layout.local.root).await;

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
        let plan_port: Arc<dyn peko_plan::PlanPort> = Arc::new(peko_plan::PlanStorage::new(
            layout.local.plans_dir.clone(),
        ));

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

        let agent_prompts = discover_agent_prompts(&layout.shared.agents_dir).await?;

        let principal = Arc::new(Principal {
            id: id.clone(),
            config: RwLock::new(config),
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

        let config: PrincipalConfig = toml::from_str(&config_str)
            .map_err(|e| PrincipalManagerError::Config(e.to_string()))?;

        let name = config.name.clone();
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

        let id = PrincipalId::generate();
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
        let plan_port: Arc<dyn peko_plan::PlanPort> = Arc::new(peko_plan::PlanStorage::new(
            layout.local.plans_dir.clone(),
        ));

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
        let agent_prompts = discover_agent_prompts(&layout.shared.agents_dir).await?;

        let principal = Arc::new(Principal {
            id: id.clone(),
            config: RwLock::new(config),
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
        self.principals_by_name.write().await.insert(name, id);

        Ok(principal)
    }

    pub async fn get(&self, id: PrincipalId) -> Option<Arc<Principal>> {
        self.principals.read().await.get(&id).cloned()
    }

    pub async fn get_by_name(&self, name: &str) -> Option<Arc<Principal>> {
        let id = self.principals_by_name.read().await.get(name).cloned()?;
        self.get(id).await
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

        let principal = self
            .get(id.clone())
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound(name.to_string()))?;

        let layout = self.path_resolver.principal_layout(name);
        // Snapshot the principal's stable DID before we drop the
        // in-memory entries — `chat_log_store.remove_principal`
        // deletes the principal's shard directory keyed by that
        // DID. The caller-view shards (e.g. principal-A's view of
        // a conversation with principal-B) are NOT touched here:
        // they live under the caller's DID, not this principal's.
        let principal_did = principal.did().await;

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

        // Delete this principal's chat-log shards. Only the
        // principal's own views go; counterpart views held on
        // other principals' shards remain because they are owned
        // by the other principal. Cleanup failures are surfaced —
        // silent retention of a removed principal's logs would
        // leak history the principal was promised to lose.
        if let Some(store) = self.chat_log_store.as_ref() {
            if let Err(error) = store.remove_principal(&principal_did).await {
                tracing::warn!(
                    principal_did = %principal_did.0,
                    %error,
                    "failed to remove chat-log shards for principal"
                );
                return Err(PrincipalManagerError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("chat-log cleanup failed: {error}"),
                )));
            }
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

        // Apply the update under the write lock, then snapshot and release
        // the lock before touching disk so config readers are not blocked
        // for the duration of the (async) file write.
        let snapshot = {
            let mut config = principal.config.write().await;
            update(&mut config);
            config.clone()
        };
        self.persist_config(&principal.workspace_path, &snapshot)
            .await?;

        Ok(principal)
    }

    async fn persist_config(
        &self,
        workspace_path: &Path,
        config: &PrincipalConfig,
    ) -> Result<(), PrincipalManagerError> {
        let toml =
            toml::to_string(config).map_err(|e| PrincipalManagerError::Config(e.to_string()))?;
        tokio::fs::write(workspace_path.join("principal.toml"), toml)
            .await
            .map_err(PrincipalManagerError::Io)
    }

    async fn generate_identity(
        &self,
        name: &str,
    ) -> Result<peko_identity::Identity, PrincipalManagerError> {
        let identity_dir = self.path_resolver.principal_layout(name).shared.root.join("identity");
        tokio::fs::create_dir_all(&identity_dir).await?;
        let identity_dir = identity_dir.clone();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || {
            let storage = KeyStorage::with_path(identity_dir)
                .map_err(|e| PrincipalManagerError::Identity(e.to_string()))?;
            storage
                .generate_identity(DIDScope::Public, Some(&name))
                .map_err(|e| PrincipalManagerError::Identity(e.to_string()))
        })
        .await
        .map_err(|e| PrincipalManagerError::Identity(e.to_string()))?
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
    pub(crate) async fn peer_child_turns_for(
        &self,
        principal: &Arc<Principal>,
    ) -> Result<Arc<crate::principal::child_turns::PeerChildTurns>, PrincipalManagerError> {
        if let Some(turns) = self.peer_child_turns.read().await.get(&principal.id) {
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
            .with_dm_lock(Some(self.session_creation_lock(principal.id.clone()).await));
        let turns = Arc::new(turns);
        self.peer_child_turns
            .write()
            .await
            .insert(principal.id.clone(), Arc::clone(&turns));
        Ok(turns)
    }

    /// Sprint 2 Phase 7: find-or-create the peer's standing child of
    /// the trunk; returns the child session id. Idempotent per peer.
    /// Used by the IPC steering path, which must key the child inbox
    /// without driving a turn.
    pub(crate) async fn ensure_peer_child_session(
        &self,
        principal: &Arc<Principal>,
        peer: &Subject,
    ) -> Result<String, PrincipalManagerError> {
        let turns = self.peer_child_turns_for(principal).await?;
        turns.ensure_child(peer).await.map_err(|e| {
            PrincipalManagerError::RouterError(RouterError::AgentFailed(format!("{e:?}")))
        })
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

    /// Recompute the principal's active extension set for a capability
    /// snapshot against the current global extension inventory.
    pub(crate) async fn active_extensions_for(
        &self,
        principal: &Principal,
        capabilities: &peko_extension_api::Capabilities,
    ) -> peko_extension_api::ActiveExtensionSet {
        let global_items = match self.extension_store.as_ref() {
            Some(store) => store.global_items().await,
            None => Vec::new(),
        };
        crate::principal::extension_store::ExtensionCatalog::build(
            capabilities,
            &principal.agent_prompts,
            &global_items,
        )
        .active_extensions()
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
        peer: Subject,
        message: String,
        channel: ChannelContext,
        // Per-message configured model override (`peko send --model`).
        // `None` preserves the principal's pinned model.
        override_model: Option<String>,
    ) -> Result<RouterContext, PrincipalManagerError> {
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
        if let Err(denied) = check_permission(&resource, Permission::Chat, &peer) {
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

        let mut recalled_context = Vec::new();
        if let Some(artifact) = latest_session {
            recalled_context.push(super::router::ContextInjection {
                kind: super::router::ContextInjectionKind::Session,
                id: artifact.session_id.clone(),
                content: artifact.summary.unwrap_or_default(),
            });
        }

        // Phase 12+ PR #4: surface every open plan with unresolved
        // nodes into the router context, ordered by `updated_at DESC`
        // (most recently active first) and capped at
        // `PLAN_INJECTION_CAP` so a principal with dozens of stale
        // plans doesn't blow the system prompt. `load_resumable`
        // supersedes the single-`current_focus` call from PR #3: any
        // plan `current_focus` would have surfaced is included here,
        // plus everything else still resumable. Port errors and empty
        // results both skip the push so a transient storage failure
        // cannot block session start.
        match principal.plan_port.load_resumable(&principal.id).await {
            Ok(mut plans) => {
                plans.sort_by(|a, b| {
                    b.updated_at
                        .cmp(&a.updated_at)
                        .then_with(|| a.plan_id.cmp(&b.plan_id))
                });
                for record in plans.into_iter().take(PLAN_INJECTION_CAP) {
                    recalled_context.push(super::router::ContextInjection {
                        kind: super::router::ContextInjectionKind::Plan,
                        id: record.plan_id.clone(),
                        content: render_plan_focus_block(&record),
                    });
                }
            }
            Err(e) => {
                tracing::warn!(
                    principal_id = %principal.id,
                    error = %e,
                    "plan_port.load_resumable failed during build_router_context; skipping plan injection"
                );
            }
        }

        let (
            available_agents,
            extension_store,
            active_extensions,
            routing,
            capabilities,
            intent,
            governance,
            principal_name,
        ) = {
            let config = principal.config.read().await;
            let allowed = &config.capabilities;
            let available_agents: Vec<_> = principal
                .agent_prompts
                .iter()
                .map(|(id, p)| super::router::AgentPromptSummary {
                    id: id.clone(),
                    name: p.name.clone(),
                    description: p.frontmatter.description.clone(),
                    enabled: allowed
                        .is_granted(&peko_extension_api::Capability::new(format!("agent:{id}")))
                        || allowed.is_granted(&peko_extension_api::Capability::new(format!(
                            "agent:{}",
                            p.name
                        ))),
                })
                .collect();

            let global_items = match self.extension_store.as_ref() {
                Some(store) => store.global_items().await,
                None => Vec::new(),
            };
            let extension_store = crate::principal::extension_store::ExtensionCatalog::build(
                allowed,
                &principal.agent_prompts,
                &global_items,
            );
            let active_extensions = extension_store.active_extensions();

            (
                available_agents,
                extension_store,
                active_extensions,
                config.routing.clone(),
                allowed.clone(),
                config.intent.clone(),
                config.governance.clone(),
                config.name.clone(),
            )
        };

        Ok(RouterContext {
            principal_id: principal.id.clone(),
            principal_name,
            peer,
            message,
            channel,
            routing,
            recalled_context,
            available_agents,
            extension_store,
            active_extensions,
            capabilities,
            intent,
            governance,
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
            // it through unconditionally is safe. Peer metering is
            // deferred to a follow-up (F20 plumbing, requires the
            // peer registry to be reachable from this entrypoint).
            quota_meter: Some(Arc::clone(&principal.quota_meter)),
            peer_meter: None,
        })
    }

    /// Inspect `message` for slash commands. If a slash command is handled,
    /// returns `(Some(rendered_response), _)`. If the message is not a slash
    /// command (or is escaped with `\/`, or `no_slash` is true), returns
    /// `(None, processed_message)` where `processed_message` has the escape
    /// stripped so the literal `/...` text reaches the root agent.
    pub async fn preprocess_slash(
        &self,
        principal: &Arc<Principal>,
        message: String,
        no_slash: bool,
        format: OutputFormat,
    ) -> Result<(Option<String>, String), PrincipalManagerError> {
        let (message, escaped) = if let Some(rest) = message.strip_prefix("\\/") {
            (format!("/{rest}"), true)
        } else {
            (message, false)
        };

        if escaped || no_slash {
            return Ok((None, message));
        }

        let dispatcher = self.slash_dispatcher.read().await;
        if let Some(dispatcher) = dispatcher.as_ref() {
            match dispatcher
                .dispatch(principal, &message, false, format)
                .await
            {
                Ok(Some(response)) => Ok((Some(response.content), message)),
                Ok(None) => Ok((None, message)),
                Err(e) => Err(PrincipalManagerError::Slash(e)),
            }
        } else {
            Ok((None, message))
        }
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
    /// Kept from the pre-Phase-7 flow: slash preprocessing, the
    /// permission check + recall via [`Self::build_router_context`],
    /// the chat-log projection (same `(principal_did, peer)` keys),
    /// and the per-session serial queue — re-keyed to the CHILD
    /// session id, so a message arriving while the peer's child has a
    /// run in flight is queued as a steering message (drained by the
    /// live child run at its next iteration boundary).
    pub async fn receive(
        &self,
        principal_id: PrincipalId,
        peer: Subject,
        message: String,
        channel: ChannelContext,
        // Per-message configured model override. Existing callers (tunnel,
        // cron, principal_send tool, `peko send` non-flag mode) pass
        // `None` and use the principal's pinned model.
        override_model: Option<String>,
    ) -> Result<PrincipalResponse, PrincipalManagerError> {
        // Automation + trunk channels: the trunk path owns its own
        // discipline (no slash preprocessing, no chat-log projection).
        if matches!(channel.kind, ChannelKind::Trunk | ChannelKind::Cron) {
            return self
                .receive_trunk(principal_id, message, override_model)
                .await;
        }

        let principal = self
            .get(principal_id)
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound("unknown".to_string()))?;

        let (slash_response, message) = self
            .preprocess_slash(&principal, message, false, OutputFormat::Human)
            .await?;
        if let Some(content) = slash_response.as_ref() {
            // Slash responses are part of what the consumer sees —
            // record them as the principal's reply to the (unlogged)
            // slash input.
            self.record_response(&principal, &peer, content).await;
            return Ok(PrincipalResponse::text(content.clone()));
        }

        // Record the raw input *before* dispatching. Recording failure
        // rejects dispatch so the consumer cannot believe they sent
        // something that did not enter the principal's chat-log shard.
        self.record_input(&principal, &peer, &message, &channel)
            .await?;

        let ctx = self
            .build_router_context(
                &principal,
                peer.clone(),
                message,
                channel,
                override_model.clone(),
            )
            .await?;

        // Phase 7: the turn runs in the peer's standing child of the
        // trunk. The recalled/plan context assembled on `ctx` is
        // intentionally NOT forwarded — the child session IS the
        // peer's continuous memory (its JSONL history loads on
        // resume); `ctx` is kept for the permission check.
        let turns = self.peer_child_turns_for(&principal).await?;
        let child_id = turns.ensure_child(&peer).await.map_err(|e| {
            PrincipalManagerError::RouterError(RouterError::AgentFailed(format!("{e:?}")))
        })?;

        // Serial queue per peer: only one run may be active for a
        // given peer child at a time. If a message arrives while the
        // child has a run in flight, queue it as a steering message in
        // the child session inbox; the active run drains it on its
        // next iteration.
        match self.inbox_registry.try_acquire_run(&child_id).await {
            Some(_permit) => {
                let outcome = turns
                    .drive_turn(&child_id, &ctx.message, override_model)
                    .await
                    .map_err(|e| {
                        PrincipalManagerError::RouterError(RouterError::AgentFailed(format!(
                            "{e:?}"
                        )))
                    })?;
                let response = outcome.final_text;
                self.record_response(&principal, &peer, &response).await;
                self.record_peer_recall(&principal, &peer, &child_id, &response)
                    .await;
                Ok(PrincipalResponse::text(response))
            }
            None => {
                let inbox = self.inbox_registry.get_or_create(&child_id).await;
                inbox
                    .push(SteeringMessage::new(ctx.message.clone()).into())
                    .await;
                let queued = format!("Queued for root agent session {child_id}.");
                self.record_response(&principal, &peer, &queued).await;
                Ok(PrincipalResponse::queued(queued))
            }
        }
    }

    /// Streaming entry point: like [`receive`](Self::receive), but drives
    /// the peer-child turn via
    /// [`crate::principal::child_turns::PeerChildTurns::drive_turn_streaming`]
    /// so token deltas are delivered to `on_event` as they are produced.
    /// Permission checks, session recall, and the per-peer serial queue
    /// are identical to `receive` — both funnel through
    /// [`build_router_context`](Self::build_router_context) and acquire
    /// the same `inbox_registry` run permit (keyed by the CHILD session
    /// id, Phase 7), so the streaming and one-shot paths can't drift.
    ///
    /// The returned [`PrincipalResponse`] carries the authoritative final
    /// answer (the same value `receive` would return). Callers that
    /// forwarded the streamed deltas can ignore the body; callers that
    /// need a single final string — or that hit the queued path, which
    /// emits no events — use it.
    pub async fn receive_streaming(
        &self,
        principal_id: PrincipalId,
        peer: Subject,
        message: String,
        channel: ChannelContext,
        on_event: Box<dyn Fn(peko_engine::AgenticEvent) + Send + Sync>,
        // Per-message configured model override (RP8 wires CLI flags).
        override_model: Option<String>,
    ) -> Result<PrincipalResponse, PrincipalManagerError> {
        // Automation + trunk channels have no streaming trunk entry
        // point; route them through the one-shot trunk path.
        if matches!(channel.kind, ChannelKind::Trunk | ChannelKind::Cron) {
            return self
                .receive_trunk(principal_id, message, override_model)
                .await;
        }

        let principal = self
            .get(principal_id)
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound("unknown".to_string()))?;

        let (slash_response, message) = self
            .preprocess_slash(&principal, message, false, OutputFormat::Human)
            .await?;
        if let Some(content) = slash_response.as_ref() {
            self.record_response(&principal, &peer, content).await;
            return Ok(PrincipalResponse::text(content.clone()));
        }

        self.record_input(&principal, &peer, &message, &channel)
            .await?;

        let ctx = self
            .build_router_context(
                &principal,
                peer.clone(),
                message,
                channel,
                override_model.clone(),
            )
            .await?;

        let turns = self.peer_child_turns_for(&principal).await?;
        let child_id = turns.ensure_child(&peer).await.map_err(|e| {
            PrincipalManagerError::RouterError(RouterError::AgentFailed(format!("{e:?}")))
        })?;

        // Same serial-queue discipline as `receive`: only one run may
        // be active per peer child. A message arriving while a run is
        // active is queued as a steering message (no streaming events
        // for the queued case).
        match self.inbox_registry.try_acquire_run(&child_id).await {
            Some(_permit) => {
                let outcome = turns
                    .drive_turn_streaming(
                        &child_id,
                        &ctx.message,
                        Arc::from(on_event),
                        None,
                        override_model,
                    )
                    .await
                    .map_err(|e| {
                        PrincipalManagerError::RouterError(RouterError::AgentFailed(format!(
                            "{e:?}"
                        )))
                    })?;
                let response = outcome.final_text;
                self.record_response(&principal, &peer, &response).await;
                self.record_peer_recall(&principal, &peer, &child_id, &response)
                    .await;
                Ok(PrincipalResponse::text(response))
            }
            None => {
                let inbox = self.inbox_registry.get_or_create(&child_id).await;
                inbox
                    .push(SteeringMessage::new(ctx.message.clone()).into())
                    .await;
                let queued = format!("Queued for root agent session {child_id}.");
                self.record_response(&principal, &peer, &queued).await;
                Ok(PrincipalResponse::queued(queued))
            }
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
    /// - **No chat-log projection.** `record_input` / `record_response`
    ///   are skipped entirely: the chat log is a consumer-facing
    ///   per-peer conversation projection keyed by `(principal_did,
    ///   peer)` and the trunk has no peer thread to project into. (A
    ///   self-thread convention is deliberately deferred — see
    ///   DATA_MODEL.md. The trunk session JSONL remains the durable
    ///   record; `ChannelKind::Trunk` is also excluded by
    ///   `is_peer_chat_channel` as defense in depth.)
    /// - **No slash preprocessing.** Trunk messages are agent-bound
    ///   automation prompts (cron payloads), not interactive commands.
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
    ) -> Result<PrincipalResponse, PrincipalManagerError> {
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

        let ctx = self
            .build_router_context(&principal, peer, message, channel, override_model)
            .await?;

        let session_id = super::routers::root::trunk_session_id();
        match self.inbox_registry.try_acquire_run(&session_id).await {
            Some(_permit) => {
                let decision = principal.router.route(ctx).await?;
                match decision {
                    RouteDecision::Respond { response } => Ok(PrincipalResponse::text(response)),
                }
            }
            None => {
                let inbox = self.inbox_registry.get_or_create(&session_id).await;
                inbox
                    .push(SteeringMessage::new(ctx.message.clone()).into())
                    .await;
                let queued = format!("Queued for root agent session {session_id}.");
                Ok(PrincipalResponse::queued(queued))
            }
        }
    }
}

/// Pure peer-chat channels. Messages arriving on these channels are
/// persisted to the runtime-owned chat log as consumer-visible
/// conversation. Automation channels (Cron/FileWatch) are excluded
/// because they represent scheduled triggers and file-system events,
/// not user/principal conversation.
fn is_peer_chat_channel(kind: &crate::principal::router::ChannelKind) -> bool {
    use crate::principal::router::ChannelKind;
    matches!(
        kind,
        ChannelKind::Cli
            | ChannelKind::Http
            | ChannelKind::Hub
            | ChannelKind::A2a
            | ChannelKind::P2p
            | ChannelKind::Webhook
    )
}

impl PrincipalManager {
    /// Persist a peer-chat input for callers that drive the router directly,
    /// such as the IPC streaming handler.
    pub(crate) async fn record_chat_input(
        &self,
        principal: &Arc<Principal>,
        peer: &Subject,
        message: &str,
        channel: &ChannelContext,
    ) -> Result<(), PrincipalManagerError> {
        self.record_input(principal, peer, message, channel).await
    }

    /// Persist a principal response for callers that drive the router
    /// directly, such as the IPC streaming handler.
    pub(crate) async fn record_chat_response(
        &self,
        principal: &Arc<Principal>,
        peer: &Subject,
        response: &str,
    ) {
        self.record_response(principal, peer, response).await
    }

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
        peer: &Subject,
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

    /// Persist a peer chat-channel input to the chat-log shard for
    /// `(principal_did, peer)`. Skipped silently for non-chat channels
    /// and when no chat-log store is attached (tests / non-daemon).
    ///
    /// Persistence failure surfaces to the caller as
    /// [`PrincipalManagerError::Internal`] so dispatch is rejected —
    /// the consumer must not be allowed to believe they sent something
    /// the principal never recorded.
    async fn record_input(
        &self,
        principal: &Arc<Principal>,
        peer: &Subject,
        message: &str,
        channel: &ChannelContext,
    ) -> Result<(), PrincipalManagerError> {
        let Some(store) = self.chat_log_store.as_ref() else {
            return Ok(());
        };
        if !is_peer_chat_channel(&channel.kind) {
            return Ok(());
        }
        let key = peko_chat_log::ChatThreadKey::new(principal.did().await, peer.clone());
        let entry = peko_chat_log::ChatLogMessage::new(peer.clone(), message.to_string(), None);
        store
            .append_message(&key, &entry)
            .await
            .map_err(|e| PrincipalManagerError::Config(format!("chat-log append: {e}")))
    }

    /// Persist a cron-fired prompt to the chat-log shard for
    /// `(principal_did, peer)`. Unlike `record_input`, this bypasses
    /// the `is_peer_chat_channel` gate because cron prompts arrive on
    /// `ChannelKind::Cron`, which is excluded by design — but the
    /// owner still needs to see the cron-fired text in `peko log`
    /// alongside the principal's reply (`record_response` writes that
    /// unconditionally).
    ///
    /// Skipped silently when no chat-log store is attached (tests /
    /// non-daemon contexts). Persistence failure returns `Err` so the
    /// caller can log a warning — the cron run itself is not
    /// rejected; the chat-log is best-effort projection.
    pub(crate) async fn record_cron_input(
        &self,
        principal: &Arc<Principal>,
        peer: &Subject,
        message: &str,
    ) -> Result<(), PrincipalManagerError> {
        let Some(store) = self.chat_log_store.as_ref() else {
            return Ok(());
        };
        let key = peko_chat_log::ChatThreadKey::new(principal.did().await, peer.clone());
        let entry = peko_chat_log::ChatLogMessage::new(peer.clone(), message.to_string(), None);
        store
            .append_message(&key, &entry)
            .await
            .map_err(|e| PrincipalManagerError::Config(format!("chat-log cron-input append: {e}")))
    }

    /// Persist the principal's authoritative response (or queued
    /// acknowledgement) to the chat-log shard. Best-effort: a failed
    /// write logs a warning but does not reject the response — the
    /// caller has already invested in producing the answer.
    async fn record_response(&self, principal: &Arc<Principal>, peer: &Subject, response: &str) {
        let Some(store) = self.chat_log_store.as_ref() else {
            return;
        };
        let key = peko_chat_log::ChatThreadKey::new(principal.did().await, peer.clone());
        let entry = peko_chat_log::ChatLogMessage::new(
            peko_subject::Subject::Principal(principal.did().await),
            response.to_string(),
            None,
        );
        if let Err(e) = store.append_message(&key, &entry).await {
            let did_str = principal.did().await.0;
            tracing::warn!(
                principal_did = %did_str,
                peer = %peer,
                error = %e,
                "chat-log append (response) failed; response was returned to caller but not persisted"
            );
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
/// **Phase A.** The caller passes the typed `agents_dir` directly
/// (i.e. `PathResolver::principal_layout(name).shared.agents_dir`)
/// rather than the Shared tier root + a hand-rolled `"agents"`
/// suffix. The legacy `workspace_path.join("agents")` join is gone
/// from this function.
async fn discover_agent_prompts(
    agents_dir: &Path,
) -> Result<HashMap<String, AgentPrompt>, PrincipalManagerError> {
    let mut prompts = HashMap::new();

    if agents_dir.exists() {
        let adapter = AgentAdapter::new();
        let discovered = adapter.discover_agents(agents_dir);
        for d in discovered {
            let canonical_id = d.manifest.id.0.clone();
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
    use peko_auth::{Permission, PermissionGrant, Subject};

    use crate::engine::tool_runtime::ToolRuntime;
    use crate::extensions::framework::core::init_global_core;
    use crate::principal::{
        router::{ChannelContext, ChannelKind},
        DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
    };
    use crate::principal::{
        PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use peko_extension_api::Capabilities;
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
        init_global_core(tool_runtime.extension_core().clone());

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("models.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )
            .with_resolver(resolver),
        );

        let principal = create_test_principal(&manager, "stressy").await;
        (temp, manager, adapter, principal.id.clone())
    }

    async fn create_test_principal(manager: &PrincipalManager, name: &str) -> Arc<Principal> {
        create_test_principal_with_agents(manager, name, &[]).await
    }

    async fn create_test_principal_with_agents(
        manager: &PrincipalManager,
        name: &str,
        extra_agents: &[&str],
    ) -> Arc<Principal> {
        let agents_dir = manager
            .path_resolver
            .principal_layout(name)
            .shared
            .agents_dir;
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();

        let primary_body = format!(
            "---\ndescription: \"Test assistant for {name}\"\n---\n\n\
             You are {name}, a test assistant. Reply concisely.\n"
        );
        tokio::fs::write(agents_dir.join("primary.md"), primary_body)
            .await
            .unwrap();

        for agent in extra_agents {
            let body = format!(
                "---\nname: {agent}\ndescription: \"Agent {agent}\"\n---\n\n\
                 You are {agent}.\n"
            );
            tokio::fs::write(agents_dir.join(format!("{agent}.md")), body)
                .await
                .unwrap();
        }

        let config = test_config(name);
        manager.create(config).await.unwrap()
    }

    fn test_config(name: &str) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            did: None,
            owner: Subject::User("test-owner".to_string()),
            identity: PrincipalIdentityConfig {
                display_name: Some(name.to_string()),
                description: Some(format!("The {name} Principal")),
                avatar: None,
            },
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            memory: PrincipalMemoryConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            capabilities: Capabilities::starter_bundle(),
            exposure: peko_auth::Exposure::Private,
            status: None,
            permissions: vec![PermissionGrant {
                subject: Subject::Public,
                permission: Permission::Chat,
                granted_at: chrono::Utc::now().to_rfc3339(),
                granted_by: Subject::User("test-owner".to_string()),
            }],
            preferred_model_id: Some("mock".to_string()),
            transport_preference: Default::default(),
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

        let peer = Subject::User("alice".to_string());
        for i in 0..turns {
            let response = manager
                .receive(
                    id.clone(),
                    peer.clone(),
                    format!("message {i}"),
                    cli_channel(),
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
            let peer = Subject::User(format!("peer-{i}"));
            let response = manager
                .receive(id.clone(), peer, format!("hello {i}"), cli_channel(), None)
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
                let peer = Subject::User(format!("concurrent-{i}"));
                // M7 fix: `SessionManager::create_session` now holds the
                // `metadata_controller` write lock for the full
                // create-metadata + create-for-peer + save-index sequence,
                // so two peers can no longer interleave their index
                // updates. The previous test had a retry loop on the
                // transient `AgentFailed("failed to create root agent
                // session")` race; with the lock held end-to-end the
                // race can't happen, and this receive is a one-shot.
                manager
                    .receive(
                        id.clone(),
                        peer.clone(),
                        format!("hello {i}"),
                        cli_channel(),
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

        let peer = Subject::User("same-peer".to_string());
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
            .ensure_peer_child_session(&principal, &peer)
            .await
            .expect("peer child provisioning");
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
                    .receive(id, peer, format!("hello {i}"), cli_channel(), None)
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
            .receive(
                id.clone(),
                peer.clone(),
                "after".to_string(),
                cli_channel(),
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

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn create_generates_real_identity() {
        let (_temp, manager, _adapter, id) = setup().await;
        let principal = manager.get(id).await.expect("principal should exist");
        let did = principal.did().await;
        assert!(
            did.0.starts_with("did:peko:"),
            "principal should have a real DID, got {}",
            did.0
        );

        let identity_dir = manager
            .path_resolver
            .principal_layout("stressy")
            .shared
            .root
            .join("identity");
        assert!(identity_dir.exists(), "identity directory should exist");
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

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn remove_principal_deletes_workspace() {
        let (_temp, manager, _adapter, id) = setup().await;
        let workspace_path = manager
            .get(id)
            .await
            .expect("principal should exist")
            .workspace_path
            .clone();

        manager
            .remove("stressy")
            .await
            .expect("remove should succeed");
        assert!(manager.get_by_name("stressy").await.is_none());
        assert!(!workspace_path.exists(), "workspace should be deleted");
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
        let stranger = Subject::User("stranger".to_string());
        let result = manager
            .receive(id, stranger, "hello".to_string(), cli_channel(), None)
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
        let owner = Subject::User("test-owner".to_string());
        let response = manager
            .receive(id.clone(), owner, "hello".to_string(), cli_channel(), None)
            .await
            .expect("owner should be allowed");
        assert!(response.content.contains("owner reply"));

        // Grant Chat to a specific user.
        manager
            .update_config("stressy", |config| {
                config.permissions.push(PermissionGrant {
                    subject: Subject::User("friend".to_string()),
                    permission: Permission::Chat,
                    granted_at: chrono::Utc::now().to_rfc3339(),
                    granted_by: config.owner.clone(),
                });
            })
            .await
            .unwrap();

        adapter.queue_text("friend reply".to_string());
        let friend = Subject::User("friend".to_string());
        let response = manager
            .receive(id, friend, "hi".to_string(), cli_channel(), None)
            .await
            .expect("grantee should be allowed");
        assert!(response.content.contains("friend reply"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn build_router_context_marks_disabled_agents() {
        let (temp, manager, _adapter, _id) = setup().await;

        // Re-create a principal with two agents, only one of which is allowed.
        manager.remove("stressy").await.unwrap();

        let principal = create_test_principal_with_agents(
            &manager,
            "stressy",
            &["enabled_agent", "disabled_agent"],
        )
        .await;
        manager
            .update_config("stressy", |config| {
                config.capabilities =
                    Capabilities::with_grants(["agent:enabled_agent", "tool:Read"]);
            })
            .await
            .unwrap();

        let ctx = manager
            .build_router_context(
                &principal,
                Subject::User("test-owner".to_string()),
                "hello".to_string(),
                cli_channel(),
                None,
            )
            .await
            .expect("build_router_context should succeed");

        let enabled = ctx
            .available_agents
            .iter()
            .find(|a| a.id == "enabled_agent")
            .expect("enabled_agent should be in catalog");
        assert!(enabled.enabled, "enabled_agent should be enabled");

        let disabled = ctx
            .available_agents
            .iter()
            .find(|a| a.id == "disabled_agent")
            .expect("disabled_agent should be in catalog");
        assert!(!disabled.enabled, "disabled_agent should be disabled");

        // The ExtensionStore also surfaces the disabled agent.
        let store_disabled = ctx
            .extension_store
            .items()
            .iter()
            .find(|i| i.id == "disabled_agent")
            .expect("disabled_agent should be in extension store");
        assert!(!store_disabled.enabled);

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
                Subject::User("test-owner".to_string()),
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
                Subject::User("test-owner".to_string()),
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

        let peer = Subject::User("streaming-peer".to_string());
        let principal = manager
            .get(id.clone())
            .await
            .expect("principal should exist");

        // Hold the peer child's run permit so every concurrent
        // streaming receive deterministically takes the
        // steering-queue branch (see the non-streaming sibling test).
        let child_id = manager
            .ensure_peer_child_session(&principal, &peer)
            .await
            .expect("peer child provisioning");
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
    // Gap-3: a `SteeringMessage` arriving via `PrincipalSendControl::Steer`
    // must be persisted to the runtime-owned chat log so the user sees
    // their own message in `peko log` and the desktop chat history.
    // The handler calls `record_chat_input` (the `pub(crate)` wrapper)
    // before pushing the inbox item — this test verifies the wrapper
    // persists correctly when the steering channel context is used.
    // ===================================================================
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn steering_message_persists_to_chat_log() {
        use peko_chat_log::{ChatLogStore, ChatThreadKey};

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
        init_global_core(tool_runtime.extension_core().clone());

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;

        // Pre-attach the chat log store to the manager so the gap-3
        // fix's `record_chat_input` call has somewhere to write.
        let chat_log_dir = temp.path().join("chat_log");
        let store = Arc::new(ChatLogStore::new(chat_log_dir));
        let manager = PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(resolver)
        .with_chat_log_store(store.clone());
        let principal = create_test_principal(&manager, "stressy").await;
        let id = principal.id.clone();

        // Mimic the gap-3 persistence path: `handle_principal_send_control`
        // looks up the principal by name, then calls `record_chat_input`
        // with a CLI channel and `streaming: true` (the values the
        // handler builds for the Steer branch).
        let peer = Subject::User("alice".to_string());
        let steered_text = "wait, do this instead";
        let channel = ChannelContext {
            kind: ChannelKind::Cli,
            streaming: true,
        };

        manager
            .record_chat_input(&principal, &peer, steered_text, &channel)
            .await
            .expect("record_chat_input should succeed");

        // Read the chat log back and verify the steered turn is there.
        let key = ChatThreadKey::new(principal.did().await, peer.clone());
        let page = store
            .read_page(&key, None, 100, None)
            .await
            .expect("read_page should succeed");
        assert_eq!(
            page.messages.len(),
            1,
            "exactly one message should have been persisted"
        );
        let persisted = &page.messages[0];
        assert_eq!(
            persisted.text, steered_text,
            "persisted message should contain the steered text"
        );
        assert_eq!(
            persisted.sender, peer,
            "persisted message should be from the user peer"
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

    // ===================================================================
    // Sprint 2 Phase 6: a peer turn driven in the peer's standing
    // child keeps the chat-log projection (keyed `(principal_did,
    // peer)` exactly as today) and the recall artifact points at the
    // child session id. This is the wrapper shape Phase 7's ingress
    // re-route uses around the child turn driver.
    // ===================================================================
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn peer_child_turn_projects_chat_log_and_recall_artifact() {
        use peko_chat_log::{ChatLogStore, ChatThreadKey};

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
        init_global_core(tool_runtime.extension_core().clone());

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;

        let store = Arc::new(ChatLogStore::new(temp.path().join("chat_log")));
        let manager = PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(resolver)
        .with_chat_log_store(store.clone());
        let principal = create_test_principal(&manager, "stressy").await;

        let owner = Subject::User("test-owner".to_string());
        let peer = Subject::User("alice".to_string());

        // Provision the peer's standing child (Phase 5) over the SAME
        // sessions dir the principal's memory exposes — the child the
        // Phase 6 streaming driver runs the turn in.
        let session_manager = Arc::new(tokio::sync::RwLock::new(
            peko_session::manager::SessionManager::new()
                .with_sessions_dir_internal(principal.memory.sessions_dir()),
        ));
        let child_id = crate::principal::peer_children::ensure_peer_child(
            "root",
            &owner,
            &peer,
            &session_manager,
        )
        .await
        .expect("peer child provisioning");

        // The Phase 7 wrapper: input row before the child turn,
        // response row after — both keyed (principal_did, peer).
        manager
            .record_chat_input(&principal, &peer, "hello principal", &cli_channel())
            .await
            .expect("record_chat_input should succeed");
        manager
            .record_chat_response(&principal, &peer, "hello alice")
            .await;

        let key = ChatThreadKey::new(principal.did().await, peer.clone());
        let page = store
            .read_page(&key, None, 100, None)
            .await
            .expect("read_page should succeed");
        assert_eq!(
            page.messages.len(),
            2,
            "input + response rows land around the child turn"
        );
        assert_eq!(page.messages[0].sender, peer);
        assert_eq!(page.messages[0].text, "hello principal");
        assert_eq!(
            page.messages[1].sender,
            Subject::Principal(principal.did().await)
        );
        assert_eq!(page.messages[1].text, "hello alice");

        // Recall: the peer's latest-session artifact points at the
        // child session id, not a `root:{peer}` id.
        manager
            .record_peer_recall(&principal, &peer, &child_id, "hello alice")
            .await;
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
        assert_eq!(artifact.summary.as_deref(), Some("hello alice"));
    }

    /// The chat-log projection gate still applies around child turns:
    /// automation channels (Cron) are not peer chat and persist no
    /// input row.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn peer_child_turn_chat_projection_skips_non_chat_channels() {
        use peko_chat_log::{ChatLogStore, ChatThreadKey};

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
        init_global_core(tool_runtime.extension_core().clone());

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;

        let store = Arc::new(ChatLogStore::new(temp.path().join("chat_log")));
        let manager = PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(resolver)
        .with_chat_log_store(store.clone());
        let principal = create_test_principal(&manager, "stressy").await;

        let peer = Subject::User("alice".to_string());
        let cron_channel = ChannelContext {
            kind: ChannelKind::Cron,
            streaming: false,
        };
        manager
            .record_chat_input(&principal, &peer, "cron-fired prompt", &cron_channel)
            .await
            .expect("record_chat_input should succeed (skipped)");

        let key = ChatThreadKey::new(principal.did().await, peer.clone());
        let page = store
            .read_page(&key, None, 100, None)
            .await
            .expect("read_page should succeed");
        assert!(
            page.messages.is_empty(),
            "non-chat channels persist no chat-log rows"
        );
    }

    /// Phase 12+ PR #3: an open plan with an InProgress node surfaces
    /// in `recalled_context` as a `Plan` injection, carrying the
    /// title + actionable node ids so the agent knows it's resuming
    /// work without an explicit `PlanGet` call.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn build_router_context_includes_plan_focus() {
        let (_temp, manager, _adapter, _id) = setup().await;
        let principal = manager.get_by_name("stressy").await.expect("principal");

        // Seed a plan with one Pending and one InProgress node so
        // current_focus returns this plan.
        let now = chrono::Utc::now();
        let plan = principal
            .plan_port
            .create(
                principal.id.clone(),
                "Migrate auth".to_string(),
                vec![
                    peko_plan::PlanNode {
                        node_id: peko_plan::NodeId::generate(),
                        step: "Wire SQLX".to_string(),
                        status: peko_plan::PlanNodeStatus::Pending,
                        depends_on: vec![],
                        evidence: None,
                        blocked_reason: None,
                        created_at: now,
                        updated_at: now,
                    },
                    peko_plan::PlanNode {
                        node_id: peko_plan::NodeId::generate(),
                        step: "Add smoke tests".to_string(),
                        status: peko_plan::PlanNodeStatus::InProgress,
                        depends_on: vec![],
                        evidence: None,
                        blocked_reason: None,
                        created_at: now,
                        updated_at: now,
                    },
                ],
            )
            .await
            .expect("create plan");

        let ctx = manager
            .build_router_context(
                &principal,
                Subject::User("test-owner".to_string()),
                "hello".to_string(),
                cli_channel(),
                None,
            )
            .await
            .expect("build_router_context should succeed");

        let plan_injection = ctx
            .recalled_context
            .iter()
            .find(|i| matches!(i.kind, super::super::router::ContextInjectionKind::Plan))
            .expect("plan injection should be present");

        assert_eq!(plan_injection.id, plan.plan_id);
        assert!(
            plan_injection.content.contains("Migrate auth"),
            "body should include title; got: {}",
            plan_injection.content
        );
        assert!(
            plan_injection.content.contains("Add smoke tests"),
            "body should include InProgress step; got: {}",
            plan_injection.content
        );
        assert!(
            plan_injection.content.contains("In progress"),
            "body should label section; got: {}",
            plan_injection.content
        );
    }

    /// When the principal has no open plan with InProgress nodes,
    /// `current_focus` returns `Ok(None)` and the Plan injection is
    /// omitted — no empty block, no token burn.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn build_router_context_omits_plan_when_none() {
        let (_temp, manager, _adapter, _id) = setup().await;
        let principal = manager.get_by_name("stressy").await.expect("principal");

        // No plan created → current_focus returns Ok(None).
        let ctx = manager
            .build_router_context(
                &principal,
                Subject::User("test-owner".to_string()),
                "hello".to_string(),
                cli_channel(),
                None,
            )
            .await
            .expect("build_router_context should succeed");

        let plan_count = ctx
            .recalled_context
            .iter()
            .filter(|i| matches!(i.kind, super::super::router::ContextInjectionKind::Plan))
            .count();
        assert_eq!(
            plan_count, 0,
            "no Plan injection should be pushed when current_focus returns None"
        );
    }

    /// Phase 12+ PR #4: when the principal has multiple open plans
    /// with unresolved nodes, every resumable plan is injected as its
    /// own `ContextInjectionKind::Plan` block — not just the
    /// most-recently-updated one. Three open plans ⇒ three Plan
    /// injections, in `updated_at DESC` order.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn build_router_context_injects_all_resumable_plans() {
        let (_temp, manager, _adapter, _id) = setup().await;
        let principal = manager.get_by_name("stressy").await.expect("principal");

        let now = chrono::Utc::now();
        let mk = |title: &str, status: peko_plan::PlanNodeStatus| peko_plan::PlanNode {
            node_id: peko_plan::NodeId::generate(),
            step: format!("{title}-step"),
            status,
            depends_on: vec![],
            evidence: None,
            blocked_reason: None,
            created_at: now,
            updated_at: now,
        };
        let a = principal
            .plan_port
            .create(
                principal.id.clone(),
                "alpha".to_string(),
                vec![mk("alpha", peko_plan::PlanNodeStatus::InProgress)],
            )
            .await
            .expect("create a");
        let b = principal
            .plan_port
            .create(
                principal.id.clone(),
                "beta".to_string(),
                vec![mk("beta", peko_plan::PlanNodeStatus::Pending)],
            )
            .await
            .expect("create b");
        let c = principal
            .plan_port
            .create(
                principal.id.clone(),
                "gamma".to_string(),
                vec![mk(
                    "gamma",
                    peko_plan::PlanNodeStatus::Blocked {
                        reason: "needs review".into(),
                        since: now,
                    },
                )],
            )
            .await
            .expect("create c");

        let ctx = manager
            .build_router_context(
                &principal,
                Subject::User("test-owner".to_string()),
                "hello".to_string(),
                cli_channel(),
                None,
            )
            .await
            .expect("build_router_context should succeed");

        let plan_injections: Vec<_> = ctx
            .recalled_context
            .iter()
            .filter(|i| matches!(i.kind, super::super::router::ContextInjectionKind::Plan))
            .collect();
        assert_eq!(
            plan_injections.len(),
            3,
            "all three open plans should surface; got {:?}",
            plan_injections
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>()
        );
        let ids: std::collections::HashSet<&str> = plan_injections
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert!(ids.contains(a.plan_id.as_str()));
        assert!(ids.contains(b.plan_id.as_str()));
        assert!(ids.contains(c.plan_id.as_str()));
    }

    /// Phase 12+ PR #4: even when the principal has more than
    /// `PLAN_INJECTION_CAP` resumable plans, only the cap-many most
    /// recently updated ones are injected. Plans are ordered by
    /// `updated_at DESC`; ties break by `plan_id` ascending
    /// (lexicographic).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn build_router_context_caps_plan_injections_at_five() {
        let (_temp, manager, _adapter, _id) = setup().await;
        let principal = manager.get_by_name("stressy").await.expect("principal");

        let now = chrono::Utc::now();
        let mk = |step: &str| peko_plan::PlanNode {
            node_id: peko_plan::NodeId::generate(),
            step: step.to_string(),
            status: peko_plan::PlanNodeStatus::Pending,
            depends_on: vec![],
            evidence: None,
            blocked_reason: None,
            created_at: now,
            updated_at: now,
        };
        // Seed 8 plans. Each create() stamps `updated_at` to a
        // strictly later instant, so the order of creation is the
        // order of `updated_at DESC`.
        let mut created_ids = Vec::new();
        for i in 0..8 {
            let record = principal
                .plan_port
                .create(
                    principal.id.clone(),
                    format!("plan-{i:02}"),
                    vec![mk(&format!("step-{i:02}"))],
                )
                .await
                .expect("create");
            created_ids.push(record.plan_id.clone());
        }

        let ctx = manager
            .build_router_context(
                &principal,
                Subject::User("test-owner".to_string()),
                "hello".to_string(),
                cli_channel(),
                None,
            )
            .await
            .expect("build_router_context should succeed");

        let plan_injections: Vec<&super::super::router::ContextInjection> = ctx
            .recalled_context
            .iter()
            .filter(|i| matches!(i.kind, super::super::router::ContextInjectionKind::Plan))
            .collect();
        assert_eq!(
            plan_injections.len(),
            5,
            "exactly 5 plans should be injected; got {}",
            plan_injections.len()
        );
        // The 5 most-recently-created plans (indices 7..3 from the
        // seed loop) are the survivors; the 3 oldest (indices 0..3)
        // are dropped by the cap.
        let expected_ids: Vec<&str> = created_ids
            .iter()
            .rev()
            .take(5)
            .map(String::as_str)
            .collect();
        let actual_ids: Vec<&str> = plan_injections.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            actual_ids, expected_ids,
            "plan injections must be ordered by updated_at DESC"
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
                config.owner = Subject::User("local".to_string());
            })
            .await
            .expect("update_config should succeed");

        let peer = Subject::User("local".to_string());
        let response = manager
            .receive(
                id.clone(),
                peer.clone(),
                "hello".to_string(),
                cli_channel(),
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
            .find(|m| m.session_id == child_id)
            .expect("child metadata");
        assert_eq!(child.slug.as_deref(), Some("local-user"));
        assert!(child.standing);
        assert!(child.privileged, "owner's child must be privileged");
        assert_eq!(child.parent_session_id.as_deref(), Some("root:self"));

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

        let peer = Subject::Principal("did:key:z6MkA2aPeerExample".to_string().into());
        let channel = ChannelContext {
            kind: ChannelKind::A2a,
            streaming: false,
        };
        let response = manager
            .receive(id.clone(), peer.clone(), "ping".to_string(), channel, None)
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
            .find(|m| m.session_id == child_id)
            .expect("child metadata");
        assert!(
            child.slug.as_deref().unwrap().starts_with("principal-"),
            "A2A child slug must be /principal-{{fragment}}, got {:?}",
            child.slug
        );
        assert!(child.standing);
        assert!(
            !child.privileged,
            "a stranger's child must stay subtree-scoped"
        );
        assert_eq!(child.peer_type.as_deref(), Some("principal"));
        assert_eq!(child.peer_id.as_deref(), Some("did:key:z6MkA2aPeerExample"));

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
        let peer = Subject::User("alice".to_string());
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

        let collected = events.lock().unwrap();
        assert!(
            collected.iter().any(|e| matches!(
                e,
                peko_engine::AgenticEvent::AssistantText { .. }
                    | peko_engine::AgenticEvent::AssistantDelta { .. }
            )),
            "the sink must see assistant text events; got {} events",
            collected.len()
        );
        drop(collected);

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
    // Phase 3 (2026-08-15): principal trunk session `root:self`
    // ===================================================================

    /// A trunk turn runs in `root:self` — never `root:{owner}` or
    /// `root:cron:{owner}` — and skips chat-log projection entirely:
    /// the chat log is a per-peer consumer projection and the trunk has
    /// no peer thread (a self-thread convention is deferred; the
    /// session JSONL is the durable record).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn trunk_receive_uses_trunk_session_and_skips_chat_log() {
        use peko_chat_log::{ChatLogStore, ChatThreadKey};

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
        init_global_core(tool_runtime.extension_core().clone());

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("models.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        adapter.queue_text("trunk reply");

        // Attach a chat-log store so a projection would be VISIBLE if
        // one happened — the empty page below is the assertion.
        let store = Arc::new(ChatLogStore::new(temp.path().join("chat_log")));
        let manager = PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(resolver)
        .with_chat_log_store(store.clone());
        let principal = create_test_principal(&manager, "stressy").await;
        let id = principal.id.clone();

        let response = manager
            .receive_trunk(id.clone(), "trunk tick".to_string(), None)
            .await
            .expect("trunk receive should succeed");
        assert!(
            response.content.contains("trunk reply"),
            "response should carry the mock reply: {}",
            response.content
        );

        // The turn landed in the trunk session, not in any per-peer
        // root session.
        let sessions_dir = principal.memory.sessions_dir().clone();
        let trunk_jsonl = sessions_dir.join("root:self.jsonl");
        assert!(trunk_jsonl.exists(), "trunk session JSONL should exist");
        let content = std::fs::read_to_string(&trunk_jsonl).unwrap();
        assert!(
            content.contains("trunk tick"),
            "trunk turn should be persisted to root:self, got: {content}"
        );
        assert!(
            !sessions_dir.join("root:user:test-owner.jsonl").exists(),
            "trunk turn must not create the owner's conversational session"
        );
        assert!(
            !sessions_dir
                .join("root:cron:user:test-owner.jsonl")
                .exists(),
            "trunk turn must not create the per-owner cron session"
        );

        // No chat-log projection for the owner thread.
        let owner = Subject::User("test-owner".to_string());
        let key = ChatThreadKey::new(principal.did().await, owner);
        let page = store
            .read_page(&key, None, 100, None)
            .await
            .expect("read_page should succeed");
        assert!(
            page.messages.is_empty(),
            "trunk turns must not project to the chat log, got {} messages",
            page.messages.len()
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
        let _permit = manager
            .inbox_registry
            .try_acquire_run("root:self")
            .await
            .expect("permit should be free");

        let response = manager
            .receive_trunk(id, "second tick".to_string(), None)
            .await
            .expect("trunk receive should succeed");
        assert!(
            response
                .content
                .contains("Queued for root agent session root:self"),
            "expected the queued-steering response, got: {}",
            response.content
        );
    }
}

/// Render the body of a `ContextInjectionKind::Plan` block from a
/// `PlanRecord`. Plain-text shape mirrors the rest of the
/// `ContextInjection.content` surface (memory, session, file, todo
/// are all free-form strings). Sections:
///   - header: plan title + plan_id
///   - "In progress" — `current_focus_nodes` (drive forward)
///   - "Ready next" — `ready_nodes` (deps satisfied, status Pending)
///   - "Needs attention" — blocked/failed nodes (warnings)
fn render_plan_focus_block(record: &PlanRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Plan: {} ({})\n",
        record.title, record.plan_id
    ));

    let in_progress = record.current_focus_nodes();
    if !in_progress.is_empty() {
        out.push_str("\nIn progress:\n");
        for n in &in_progress {
            out.push_str(&format!("  - [{}] {}\n", n.node_id.as_str(), n.step));
        }
    }

    let ready = record.ready_nodes();
    if !ready.is_empty() {
        out.push_str("\nReady next:\n");
        for n in &ready {
            out.push_str(&format!("  - [{}] {}\n", n.node_id.as_str(), n.step));
        }
    }

    // Blocked + Failed nodes get surfaced as warnings regardless of
    // current_focus_nodes() membership.
    let attention: Vec<&peko_plan::PlanNode> = record
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.status,
                PlanNodeStatus::Blocked { .. } | PlanNodeStatus::Failed { .. }
            )
        })
        .collect();
    if !attention.is_empty() {
        out.push_str("\nNeeds attention:\n");
        for n in &attention {
            let reason = match &n.status {
                PlanNodeStatus::Blocked { reason, .. } => reason.clone(),
                PlanNodeStatus::Failed { reason, .. } => reason.clone(),
                _ => String::new(),
            };
            let status_label = match &n.status {
                PlanNodeStatus::Blocked { .. } => "blocked",
                PlanNodeStatus::Failed { .. } => "failed",
                _ => "unknown",
            };
            out.push_str(&format!(
                "  - [{}] ({}): {} ({})\n",
                n.node_id.as_str(),
                status_label,
                n.step,
                reason
            ));
        }
    }

    out
}
