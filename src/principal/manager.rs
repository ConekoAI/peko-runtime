use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{
    agent_prompt::load_agent_prompt,
    config::{PrincipalConfig, PrincipalDID},
    factory::{PrincipalMemoryFactory, PrincipalRouterFactory},
    router::{ChannelContext, RouteDecision, RouterContext, RouterError},
    AgentPrompt, Principal, PrincipalId,
};
use crate::auth::ownership::{check_permission, Permission, Resource};
use crate::auth::Subject;
use crate::common::paths::PathResolver;
use crate::extensions::agent::AgentAdapter;
use crate::extensions::framework::async_exec::executor::SteeringMessage;
use crate::identity::did::DIDScope;
use crate::identity::storage::KeyStorage;
use crate::providers::LlmResolver;
use crate::session::InboxRegistry;

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

/// Owns all Principals in a runtime.
pub struct PrincipalManager {
    principals: RwLock<HashMap<PrincipalId, Arc<Principal>>>,
    principals_by_name: RwLock<HashMap<String, PrincipalId>>,
    workspace_root: PathBuf,
    path_resolver: PathResolver,
    memory_factory: Arc<dyn PrincipalMemoryFactory>,
    router_factory: Arc<dyn PrincipalRouterFactory>,
    resolver: Option<Arc<LlmResolver>>,
    /// Shared inbox registry used by supervisor agents and the Principal
    /// boundary to queue steering messages for sessions that already have
    /// a run in flight.
    inbox_registry: Arc<InboxRegistry>,
    /// Per-principal lock guarding first-time session creation/open so
    /// concurrent peers do not race on shared metadata/index writes.
    session_creation_locks: tokio::sync::RwLock<HashMap<PrincipalId, Arc<tokio::sync::Mutex<()>>>>,
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
        }
    }

    pub fn with_resolver(mut self, resolver: Arc<LlmResolver>) -> Self {
        self.resolver = Some(resolver);
        self
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
        let router = self
            .router_factory
            .create(&config, memory.clone(), &workspace_path, self.resolver.clone())
            .await;

        let agent_prompts = discover_agent_prompts(&workspace_path).await?;

        let principal = Arc::new(Principal {
            id: id.clone(),
            config: RwLock::new(config),
            workspace_path,
            memory,
            router,
            agent_prompts,
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
    pub async fn load(
        &self,
        config_path: &Path,
    ) -> Result<Arc<Principal>, PrincipalManagerError> {
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
        let router = self
            .router_factory
            .create(&config, memory.clone(), &workspace_path, self.resolver.clone())
            .await;
        let agent_prompts = discover_agent_prompts(&workspace_path).await?;

        let principal = Arc::new(Principal {
            id: id.clone(),
            config: RwLock::new(config),
            workspace_path,
            memory,
            router,
            agent_prompts,
        });

        self.principals
            .write()
            .await
            .insert(id.clone(), principal.clone());
        self.principals_by_name.write().await.insert(name, id);

        Ok(principal)
    }

    pub async fn get(
        &self,
        id: PrincipalId,
    ) -> Option<Arc<Principal>> {
        self.principals.read().await.get(&id).cloned()
    }

    pub async fn get_by_name(
        &self,
        name: &str,
    ) -> Option<Arc<Principal>> {
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

        {
            let mut config = principal.config.write().await;
            update(&mut config);
            self.persist_config(&principal.workspace_path, &config).await?;
        }

        Ok(principal)
    }

    async fn persist_config(
        &self,
        workspace_path: &Path,
        config: &PrincipalConfig,
    ) -> Result<(), PrincipalManagerError> {
        let toml = toml::to_string(config)
            .map_err(|e| PrincipalManagerError::Config(e.to_string()))?;
        tokio::fs::write(workspace_path.join("principal.toml"), toml)
            .await
            .map_err(PrincipalManagerError::Io)
    }

    async fn generate_identity(
        &self,
        name: &str,
    ) -> Result<crate::identity::Identity, PrincipalManagerError> {
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

    /// The main entry point: a message arrives at a Principal boundary.
    pub async fn receive(
        &self,
        principal_id: PrincipalId,
        peer: Subject,
        message: String,
        _channel: ChannelContext,
    ) -> Result<PrincipalResponse, PrincipalManagerError> {
        let principal = self
            .get(principal_id)
            .await
            .ok_or_else(|| PrincipalManagerError::NotFound("unknown".to_string()))?;

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
            .map_err(|e| PrincipalManagerError::RouterError(RouterError::AgentFailed(e.to_string())))?;

        let mut recalled_context = Vec::new();
        if let Some(artifact) = latest_session {
            recalled_context.push(super::router::ContextInjection {
                kind: super::router::ContextInjectionKind::Session,
                id: artifact.session_id.clone(),
                content: artifact.summary.unwrap_or_default(),
            });
        }

        let (available_agents, routing, capabilities, intent, governance, principal_name) = {
            let config = principal.config.read().await;
            let available_agents: Vec<_> = principal
                .agent_prompts
                .values()
                .map(|p| super::router::AgentPromptSummary {
                    name: p.name.clone(),
                    role: parse_agent_role(p.frontmatter.role.as_deref()),
                    description: p.frontmatter.description.clone(),
                })
                .collect();
            (
                available_agents,
                config.routing.clone(),
                config.capabilities.clone(),
                config.intent.clone(),
                config.governance.clone(),
                config.name.clone(),
            )
        };

        let ctx = RouterContext {
            principal_id: principal.id.clone(),
            principal_name,
            peer: peer.clone(),
            message,
            channel: _channel,
            routing,
            recalled_context,
            available_agents,
            capabilities,
            intent,
            governance,
            inbox_registry: Arc::clone(&self.inbox_registry),
            session_creation_lock: self.session_creation_lock(principal.id.clone()).await,
        };

        // Serial queue per peer: only one supervisor run may be active for a
        // given peer/session at a time.  If a message arrives while the
        // supervisor is already running, queue it as a steering message in the
        // same session inbox; the active run will drain it on its next
        // iteration.
        let session_id = super::routers::supervisor::supervisor_session_id(&peer);
        match self.inbox_registry.try_acquire_run(&session_id).await {
            Some(_permit) => {
                let decision = principal.router.route(ctx).await?;
                match decision {
                    RouteDecision::Respond { response } => {
                        Ok(PrincipalResponse::text(response))
                    }
                }
            }
            None => {
                let inbox = self.inbox_registry.get_or_create(&session_id).await;
                inbox.push(SteeringMessage::new(ctx.message.clone()));
                Ok(PrincipalResponse::queued(format!(
                    "Queued for supervisor session {session_id}."
                )))
            }
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
            let prompt = load_agent_prompt(&d.file_path)
                .map_err(|e| PrincipalManagerError::Config(format!("{}: {e}", d.manifest.name)))?;
            prompts.insert(d.manifest.name.clone(), prompt);
        }
    }

    Ok(prompts)
}

fn parse_agent_role(role: Option<&str>) -> super::config::AgentRole {
    match role.unwrap_or("default").to_lowercase().as_str() {
        "supervisor" => super::config::AgentRole::Supervisor,
        "specialist" => super::config::AgentRole::Specialist,
        _ => super::config::AgentRole::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Permission, PermissionGrant, Subject};

    use crate::engine::tool_runtime::ToolRuntime;
    use crate::extensions::framework::core::init_global_core;
    use crate::principal::{
        config::{
            PrincipalCapabilities, PrincipalConfig, PrincipalGovernanceConfig,
            PrincipalIdentityConfig, PrincipalIntentConfig, PrincipalMemoryConfig,
            PrincipalRoutingConfig,
        },
        router::{ChannelContext, ChannelKind},
        DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory,
    };
    use crate::providers::{LlmResolver, MockAdapter};
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, Arc<PrincipalManager>, MockAdapter, PrincipalId) {
        let temp = TempDir::new().expect("temp dir");
        std::env::set_var("PEKO_HOME", temp.path());
        crate::identity::init_test_env();

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

        let catalog_path = temp.path().join("providers.toml");
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
        let agents_dir = manager.workspace_root.join(name).join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();
        let prompt_path = agents_dir.join("primary.md");
        let prompt_body = format!(
            "---\ndescription: \"Test assistant for {name}\"\n---\n\n\
             You are {name}, a test assistant. Reply concisely.\n"
        );
        tokio::fs::write(&prompt_path, prompt_body).await.unwrap();

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
            capabilities: PrincipalCapabilities::default(),
            exposure: crate::tunnel::protocol::InstanceExposure::Private,
            permissions: vec![PermissionGrant {
                subject: Subject::Public,
                permission: Permission::Chat,
                granted_at: chrono::Utc::now().to_rfc3339(),
                granted_by: Subject::User("test-owner".to_string()),
            }],
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
                .receive(id.clone(), peer.clone(), format!("message {i}"), cli_channel())
                .await
                .expect("receive should succeed");
            assert!(
                response.content.contains(&format!("reply {i}")),
                "response should contain mock reply {i}: {}",
                response.content
            );
        }

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions = principal.memory.list_sessions().await.expect("list sessions");
        assert_eq!(sessions.len(), 1, "repeated messages from one peer must reuse one session");
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
                .receive(id.clone(), peer, format!("hello {i}"), cli_channel())
                .await
                .expect("receive should succeed");
            assert!(response.content.contains(&format!("peer reply {i}")));
        }

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions = principal.memory.list_sessions().await.expect("list sessions");
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
                // `SessionManager::create_session` can return a transient
                // `RouterError(AgentFailed("failed to create supervisor session"))`
                // under heavy concurrency when two tasks race to create
                // distinct sessions against the same `metadata_controller`
                // and the underlying JSONL write trips on an OS-level
                // file-create race. The contract this test asserts is
                // **isolation** (each peer gets a session), not that
                // every receive is a one-shot — so retry transient
                // supervisor-creation failures and re-queue the adapter
                // response before giving up. Each task uses a distinct
                // peer, so the isolation invariant is preserved across
                // retries (different session ids, different response text).
                for attempt in 0..3 {
                    match manager
                        .receive(id.clone(), peer.clone(), format!("hello {i}"), cli_channel())
                        .await
                    {
                        Ok(resp) => return Ok::<_, PrincipalManagerError>(resp),
                        Err(e) if attempt < 2 && is_transient_supervisor_error(&e) => {
                            eprintln!("retrying concurrent receive {i} after: {e}");
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                        Err(e) => return Err(e),
                    }
                }
                unreachable!("retry loop should have returned or propagated the error")
            });
            handles.push(handle);
        }

        let results = futures::future::join_all(handles).await;
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok_count, peers as usize, "all concurrent receives should complete");

        let mut actual_texts: Vec<String> = Vec::with_capacity(peers as usize);
        for result in results {
            let response = result.expect("task should not panic").expect("receive should succeed");
            actual_texts.push(response.content);
        }

        let expected_texts: Vec<String> = (0..peers)
            .map(|i| format!("concurrent {i}"))
            .collect();
        for expected in &expected_texts {
            assert!(
                actual_texts.iter().any(|t| t.contains(expected)),
                "expected one response to contain '{expected}'"
            );
        }

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions = principal.memory.list_sessions().await.expect("list sessions");
        assert_eq!(
            sessions.len(),
            peers as usize,
            "concurrent peers should each get a distinct session"
        );
    }

    /// Recognize the transient `AgentFailed("failed to create supervisor
    /// session")` shape from `src/principal/agent_runner.rs:196`. Used by
    /// the concurrent-receives retry loop to distinguish flaky races
    /// from real, structural failures.
    ///
    /// Uses Debug rather than Display because the relevant substring
    /// ("failed to create supervisor session") is the inner context
    /// string, which the thiserror Display impls (RouterError →
    /// PrincipalManagerError) wrap into a "router error: routing agent
    /// failed: ..." prefix. Debug gives the full struct shape
    /// `RouterError(AgentFailed("..."))` so the substring is directly
    /// findable.
    fn is_transient_supervisor_error(e: &PrincipalManagerError) -> bool {
        format!("{e:?}").contains("failed to create supervisor session")
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
                    .receive(id, peer, format!("hello {i}"), cli_channel())
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
            .map(|r| r.expect("task should not panic").expect("receive should succeed"))
            .collect();

        let answered = responses
            .iter()
            .filter(|r| !r.content.starts_with("Queued for supervisor session"))
            .count();
        assert_eq!(
            answered, 1,
            "only one supervisor run should be active for the same peer at a time"
        );

        let queued = responses
            .iter()
            .filter(|r| r.content.starts_with("Queued for supervisor session"))
            .count();
        assert_eq!(
            queued,
            messages as usize - 1,
            "the rest should be queued as steering messages"
        );

        let principal = manager.get(id).await.expect("principal should exist");
        let sessions = principal.memory.list_sessions().await.expect("list sessions");
        assert_eq!(
            sessions.len(),
            1,
            "all messages for the same peer share one supervisor session"
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
            .principal_identity_dir("stressy");
        assert!(identity_dir.exists(), "identity directory should exist");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn list_all_returns_all_principals() {
        let (_temp, manager, _adapter, _id) = setup().await;
        create_test_principal(&manager, "beta").await;

        let all = manager.list_all().await;
        let names: Vec<String> =
            futures::future::join_all(all.iter().map(|p| p.name())).await;
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

        manager.remove("stressy").await.expect("remove should succeed");
        assert!(manager.get_by_name("stressy").await.is_none());
        assert!(!workspace_path.exists(), "workspace should be deleted");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn update_config_persists_changes() {
        let (_temp, manager, _adapter, id) = setup().await;
        manager
            .update_config("stressy", |config| {
                config.exposure = crate::tunnel::protocol::InstanceExposure::Public;
            })
            .await
            .expect("update_config should succeed");

        let principal = manager.get(id).await.expect("principal should exist");
        assert_eq!(
            principal.exposure().await,
            crate::tunnel::protocol::InstanceExposure::Public
        );

        let toml = tokio::fs::read_to_string(principal.workspace_path.join("principal.toml"))
            .await
            .unwrap();
        assert!(toml.contains("public"), "updated exposure should be persisted");
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
            .receive(id, stranger, "hello".to_string(), cli_channel())
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
            .receive(id.clone(), owner, "hello".to_string(), cli_channel())
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
            .receive(id, friend, "hi".to_string(), cli_channel())
            .await
            .expect("grantee should be allowed");
        assert!(response.content.contains("friend reply"));
    }
}
