use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{
    agent_prompt::load_agent_prompt,
    config::PrincipalConfig,
    factory::{PrincipalMemoryFactory, PrincipalRouterFactory},
    router::{ChannelContext, RouteDecision, RouterContext, RouterError},
    slash::{SlashDispatcher, SlashError},
    AgentPrompt, Principal, PrincipalId,
};
use crate::common::paths::PathResolver;
use crate::common::types::OutputFormat;
use crate::extensions::agent::AgentAdapter;
use crate::extensions::framework::store::ExtensionStore;
use crate::observability::Observability;
use crate::providers::LlmResolver;
use crate::session::InboxRegistry;
use crate::subject::PrincipalDID;
use peko_auth::ownership::{check_permission, Permission, Resource};
use peko_auth::Subject;
use peko_identity::did::DIDScope;
use peko_identity::storage::KeyStorage;
use peko_extension_host::SteeringMessage;

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
    workspace_root: PathBuf,
    path_resolver: PathResolver,
    memory_factory: Arc<dyn PrincipalMemoryFactory>,
    router_factory: Arc<dyn PrincipalRouterFactory>,
    resolver: Option<Arc<LlmResolver>>,
    /// Shared inbox registry used by root agents and the Principal
    /// boundary to queue steering messages for sessions that already have
    /// a run in flight.
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
    chat_log_store: Option<Arc<crate::chat_log::ChatLogStore>>,
}

impl PrincipalManager {
    pub fn new(
        workspace_root: PathBuf,
        memory_factory: Arc<dyn PrincipalMemoryFactory>,
        router_factory: Arc<dyn PrincipalRouterFactory>,
    ) -> Self {
        Self::with_path_resolver(
            workspace_root,
            PathResolver::new(),
            memory_factory,
            router_factory,
        )
    }

    pub fn with_path_resolver(
        workspace_root: PathBuf,
        path_resolver: PathResolver,
        memory_factory: Arc<dyn PrincipalMemoryFactory>,
        router_factory: Arc<dyn PrincipalRouterFactory>,
    ) -> Self {
        Self {
            principals: RwLock::new(HashMap::new()),
            principals_by_name: RwLock::new(HashMap::new()),
            workspace_root,
            path_resolver,
            memory_factory,
            router_factory,
            resolver: None,
            inbox_registry: Arc::new(InboxRegistry::new()),
            session_creation_locks: tokio::sync::RwLock::new(HashMap::new()),
            slash_dispatcher: Arc::new(RwLock::new(None)),
            extension_store: None,
            observability: None,
            peer_registry: None,
            chat_log_store: None,
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
    pub fn with_chat_log_store(
        mut self,
        chat_log_store: Arc<crate::chat_log::ChatLogStore>,
    ) -> Self {
        self.chat_log_store = Some(chat_log_store);
        self
    }

    /// Optional reference to the attached chat-log store.
    #[must_use]
    pub fn chat_log_store(&self) -> Option<&Arc<crate::chat_log::ChatLogStore>> {
        self.chat_log_store.as_ref()
    }

    /// Create a new Principal from config, generate a real identity, and load
    /// its agent prompts.
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
        let workspace_path = self.workspace_root.join(&name);
        tokio::fs::create_dir_all(&workspace_path).await?;

        // Generate and persist a real DID identity for this Principal.
        let mut config = config;
        let identity = self.generate_identity(&name).await?;
        config.did = Some(PrincipalDID(identity.did));

        // Persist principal.toml.
        self.persist_config(&workspace_path, &config).await?;

        let memory = self.memory_factory.create(&id, &workspace_path).await;

        // F18: build the quota meter first so the router can capture
        // the same Arc. The meter is built before the router because
        // `RouterFactory::create` takes `Arc<QuotaMeter>` — both the
        // root router and the principal carry the same handle so an
        // `update_config` call that mutates `config.quota` only
        // needs to swap the meter on the principal.
        let quota_config = config.quota.clone().unwrap_or_default();
        let quota_state_path = workspace_path.join("quota_state.json");
        let quota_meter = crate::quota::QuotaMeter::load_or_init(
            quota_config,
            Some(quota_state_path),
            chrono::Utc::now(),
        )
        .await
        .map_err(|e| PrincipalManagerError::Config(format!("quota meter init: {e}")))?;
        let quota_meter = Arc::new(quota_meter);

        let router = self
            .router_factory
            .create(
                &config,
                memory.clone(),
                &workspace_path,
                self.resolver.clone(),
            )
            .await;

        let agent_prompts = discover_agent_prompts(&workspace_path).await?;

        let principal = Arc::new(Principal {
            id: id.clone(),
            config: RwLock::new(config),
            workspace_path,
            memory,
            router,
            agent_prompts,
            quota_meter,
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

        let id = PrincipalId::generate();
        let memory = self.memory_factory.create(&id, &workspace_path).await;

        // F19: build / restore the quota meter from disk so a daemon
        // restart preserves the principal's accumulated usage. The
        // meter is owned by `Principal` directly — the engine loop
        // fetches it via `Principal.quota_meter` and opens
        // `QuotaScope::with` at run entrypoint.
        let quota_config = config.quota.clone().unwrap_or_default();
        let quota_state_path = workspace_path.join("quota_state.json");
        let quota_meter = crate::quota::QuotaMeter::load_or_init(
            quota_config,
            Some(quota_state_path),
            chrono::Utc::now(),
        )
        .await
        .map_err(|e| PrincipalManagerError::Config(format!("quota meter init: {e}")))?;
        let quota_meter = Arc::new(quota_meter);

        let router = self
            .router_factory
            .create(
                &config,
                memory.clone(),
                &workspace_path,
                self.resolver.clone(),
            )
            .await;
        let agent_prompts = discover_agent_prompts(&workspace_path).await?;

        let principal = Arc::new(Principal {
            id: id.clone(),
            config: RwLock::new(config),
            workspace_path,
            memory,
            router,
            agent_prompts,
            quota_meter,
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

        let workspace_path = principal.workspace_path.clone();
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
        }

        // Best-effort cleanup of on-disk directories.
        if workspace_path.exists() {
            tokio::fs::remove_dir_all(&workspace_path)
                .await
                .map_err(|e| {
                    tracing::warn!("failed to remove principal workspace {workspace_path:?}: {e}");
                    PrincipalManagerError::Io(e)
                })?;
        }

        let data_dir = self.path_resolver.data_dir().join("principals").join(name);
        if data_dir.exists() {
            if let Err(e) = tokio::fs::remove_dir_all(&data_dir).await {
                tracing::warn!("failed to remove principal data dir {data_dir:?}: {e}");
            }
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
        let identity_dir = self.path_resolver.principal_identity_dir(name);
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
        Arc<crate::session::InboxRegistry>,
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
                        .is_granted(&crate::principal::Capability::new(format!("agent:{id}")))
                        || allowed.is_granted(&crate::principal::Capability::new(format!(
                            "agent:{}",
                            p.name
                        ))),
                })
                .collect();

            let global_items = match self.extension_store.as_ref() {
                Some(store) => store.global_items().await,
                None => Vec::new(),
            };
            let extension_store =
                super::ExtensionCatalog::build(allowed, &principal.agent_prompts, &global_items);
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
            .build_router_context(&principal, peer.clone(), message, channel, override_model)
            .await?;

        let response_peer = ctx.peer.clone();
        // Serial queue per peer: only one root-agent run may be active for a
        // given peer/session at a time.  If a message arrives while the
        // root agent is already running, queue it as a steering message in the
        // same session inbox; the active run will drain it on its next
        // iteration.
        let session_id = super::routers::root::root_session_id(&response_peer);
        match self.inbox_registry.try_acquire_run(&session_id).await {
            Some(_permit) => {
                let decision = principal.router.route(ctx).await?;
                match decision {
                    RouteDecision::Respond { response } => {
                        self.record_response(&principal, &response_peer, &response)
                            .await;
                        Ok(PrincipalResponse::text(response))
                    }
                }
            }
            None => {
                let inbox = self.inbox_registry.get_or_create(&session_id).await;
                inbox.push(SteeringMessage::new(ctx.message.clone()));
                let queued = format!("Queued for root agent session {session_id}.");
                self.record_response(&principal, &response_peer, &queued)
                    .await;
                Ok(PrincipalResponse::queued(queued))
            }
        }
    }

    /// Streaming entry point: like [`receive`](Self::receive), but drives
    /// the router's `route_streaming` so token deltas are delivered to
    /// `on_event` as they are produced. Permission checks, session recall,
    /// and the per-peer serial queue are identical to `receive` — both
    /// funnel through [`build_router_context`](Self::build_router_context)
    /// and acquire the same `inbox_registry` run permit, so the streaming
    /// and one-shot paths can't drift.
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
        on_event: Box<dyn Fn(crate::engine::AgenticEvent) + Send + Sync>,
        // Per-message configured model override (RP8 wires CLI flags).
        override_model: Option<String>,
    ) -> Result<PrincipalResponse, PrincipalManagerError> {
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
            .build_router_context(&principal, peer.clone(), message, channel, override_model)
            .await?;

        let response_peer = ctx.peer.clone();
        // Same serial-queue discipline as `receive`: only one root-agent
        // run may be active per peer/session. A message arriving while a
        // run is active is queued as a steering message (no streaming
        // events for the queued case).
        let session_id = super::routers::root::root_session_id(&response_peer);
        match self.inbox_registry.try_acquire_run(&session_id).await {
            Some(_permit) => {
                let decision = principal
                    .router
                    .route_streaming(ctx, on_event, None)
                    .await?;
                match decision {
                    RouteDecision::Respond { response } => {
                        self.record_response(&principal, &response_peer, &response)
                            .await;
                        Ok(PrincipalResponse::text(response))
                    }
                }
            }
            None => {
                let inbox = self.inbox_registry.get_or_create(&session_id).await;
                inbox.push(SteeringMessage::new(ctx.message.clone()));
                let queued = format!("Queued for root agent session {session_id}.");
                self.record_response(&principal, &response_peer, &queued)
                    .await;
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
        let key = crate::chat_log::ChatThreadKey::new(principal.did().await, peer.clone());
        let entry = crate::chat_log::ChatLogMessage::new(peer.clone(), message.to_string(), None);
        store
            .append_message(&key, &entry)
            .await
            .map_err(|e| PrincipalManagerError::Config(format!("chat-log append: {e}")))
    }

    /// Persist the principal's authoritative response (or queued
    /// acknowledgement) to the chat-log shard. Best-effort: a failed
    /// write logs a warning but does not reject the response — the
    /// caller has already invested in producing the answer.
    async fn record_response(&self, principal: &Arc<Principal>, peer: &Subject, response: &str) {
        let Some(store) = self.chat_log_store.as_ref() else {
            return;
        };
        let key = crate::chat_log::ChatThreadKey::new(principal.did().await, peer.clone());
        let entry = crate::chat_log::ChatLogMessage::new(
            crate::subject::Subject::Principal(principal.did().await),
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

async fn discover_agent_prompts(
    workspace_path: &Path,
) -> Result<HashMap<String, AgentPrompt>, PrincipalManagerError> {
    let mut prompts = HashMap::new();

    // Discover AGENT.md extensions under <workspace>/agents/
    let agents_dir = workspace_path.join("agents");
    if agents_dir.exists() {
        let adapter = AgentAdapter::new();
        let discovered = adapter.discover_agents(&agents_dir);
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
        config::{
            PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig,
            PrincipalIntentConfig, PrincipalMemoryConfig, PrincipalRoutingConfig,
        },
        router::{ChannelContext, ChannelKind},
        Capabilities, DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
    };
    use crate::providers::{LlmResolver, MockAdapter};
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
                workspace,
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
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
        let agents_dir = manager.workspace_root.join(name).join("agents");
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
        for i in 0..messages {
            adapter.queue_text(format!("same-peer {i}"));
        }

        let peer = Subject::User("same-peer".to_string());
        let mut handles = Vec::with_capacity(messages as usize);
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
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(
            ok_count, messages as usize,
            "all concurrent receives should complete"
        );

        let responses: Vec<PrincipalResponse> = results
            .into_iter()
            .map(|r| {
                r.expect("task should not panic")
                    .expect("receive should succeed")
            })
            .collect();

        let answered = responses
            .iter()
            .filter(|r| !r.content.starts_with("Queued for root agent session"))
            .count();
        assert_eq!(
            answered, 1,
            "only one root-agent run should be active for the same peer at a time"
        );

        let queued = responses
            .iter()
            .filter(|r| r.content.starts_with("Queued for root agent session"))
            .count();
        assert_eq!(
            queued,
            messages as usize - 1,
            "the rest should be queued as steering messages"
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
            "all messages for the same peer share one root-agent session"
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

        let identity_dir = manager.path_resolver.principal_identity_dir("stressy");
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
}
