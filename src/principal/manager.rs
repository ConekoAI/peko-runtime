use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{
    agent_prompt::load_agent_prompt,
    agent_runner::{new_session_id, run_agent_prompt},
    config::PrincipalConfig,
    factory::{PrincipalMemoryFactory, PrincipalRouterFactory},
    memory::SessionArtifact,
    router::{ChannelContext, RouteDecision, RouterContext, RouterError},
    AgentPrompt, Principal, PrincipalId,
};
use crate::auth::Subject;
use crate::extensions::agent::AgentAdapter;
use crate::providers::LlmResolver;

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
}

/// Owns all Principals in a runtime.
pub struct PrincipalManager {
    principals: RwLock<HashMap<PrincipalId, Arc<Principal>>>,
    principals_by_name: RwLock<HashMap<String, PrincipalId>>,
    workspace_root: PathBuf,
    memory_factory: Arc<dyn PrincipalMemoryFactory>,
    router_factory: Arc<dyn PrincipalRouterFactory>,
    resolver: Option<Arc<LlmResolver>>,
    /// Per-principal execution lock so concurrent `receive` calls on the
    /// same Principal do not race on shared session state.
    execution_locks: tokio::sync::RwLock<HashMap<PrincipalId, Arc<tokio::sync::Mutex<()>>>>,
}

impl PrincipalManager {
    pub fn new(
        workspace_root: PathBuf,
        memory_factory: Arc<dyn PrincipalMemoryFactory>,
        router_factory: Arc<dyn PrincipalRouterFactory>,
    ) -> Self {
        Self {
            principals: RwLock::new(HashMap::new()),
            principals_by_name: RwLock::new(HashMap::new()),
            workspace_root,
            memory_factory,
            router_factory,
            resolver: None,
            execution_locks: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    pub fn with_resolver(mut self, resolver: Arc<LlmResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Create a new Principal from config and load its agent prompts.
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

        // Persist principal.toml.
        let toml = toml::to_string(&config)
            .map_err(|e| PrincipalManagerError::Config(e.to_string()))?;
        tokio::fs::write(workspace_path.join("principal.toml"), toml).await?;

        let memory = self.memory_factory.create(&id, &workspace_path).await;
        let router = self
            .router_factory
            .create(&config, memory.clone(), &workspace_path, self.resolver.clone())
            .await;

        let agent_prompts = discover_agent_prompts(&workspace_path, &config.agents).await?;

        let principal = Arc::new(Principal {
            id: id.clone(),
            config,
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
        let agent_prompts = discover_agent_prompts(&workspace_path, &config.agents).await?;

        let principal = Arc::new(Principal {
            id: id.clone(),
            config,
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

    /// Get (or create) the execution mutex for a given Principal.
    async fn execution_lock(&self, principal_id: PrincipalId) -> Arc<tokio::sync::Mutex<()>> {
        {
            let locks = self.execution_locks.read().await;
            if let Some(lock) = locks.get(&principal_id) {
                return lock.clone();
            }
        }
        let mut locks = self.execution_locks.write().await;
        locks
            .entry(principal_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// The main entry point: a message arrives at a Principal boundary.
    ///
    /// Phase 1 scaffolding: validates the decision and returns a placeholder
    /// response. Execution against agent prompts is wired in Phase 3.
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

        let available_agents: Vec<_> = principal
            .agent_prompts
            .values()
            .map(|p| super::router::AgentPromptSummary {
                name: p.name.clone(),
                role: parse_agent_role(p.frontmatter.role.as_deref()),
                description: p.frontmatter.description.clone(),
            })
            .collect();

        let ctx = RouterContext {
            principal_id: principal.id.clone(),
            principal_name: principal.config.name.clone(),
            peer: peer.clone(),
            message,
            channel: _channel,
            routing: principal.config.routing.clone(),
            recalled_context,
            available_agents,
            capabilities: principal.config.capabilities.clone(),
            intent: principal.config.intent.clone(),
            governance: principal.config.governance.clone(),
        };

        let decision = principal.router.route(ctx).await?;

        // Validate the decision against this principal's registered prompts.
        if let Some(target) = decision.target_agent() {
            if principal.agent_prompt(target).is_none() {
                return Err(PrincipalManagerError::InvalidDecision(format!(
                    "unknown agent prompt '{target}'"
                )));
            }
        }

        // Serialize execution for this Principal so concurrent `receive`
        // calls do not race on the shared session index.
        let exec_lock = self.execution_lock(principal.id.clone()).await;
        let _exec_guard = exec_lock.lock().await;

        match decision {
            RouteDecision::Respond { response } => Ok(PrincipalResponse::text(response)),
            RouteDecision::Defer { reason } => Ok(PrincipalResponse::deferred(reason)),
            RouteDecision::Continue {
                target_agent,
                input_message,
                resume_session_id,
                ..
            } => {
                self.execute_agent_decision(
                    principal,
                    peer,
                    target_agent,
                    input_message,
                    resume_session_id,
                )
                .await
            }
            RouteDecision::Spawn {
                target_agent,
                input_message,
                ..
            } => {
                self.execute_agent_decision(principal, peer, target_agent, input_message, None)
                    .await
            }
        }
    }

    async fn execute_agent_decision(
        &self,
        principal: Arc<Principal>,
        peer: Subject,
        target_agent: String,
        input_message: String,
        resume_session_id: Option<String>,
    ) -> Result<PrincipalResponse, PrincipalManagerError> {
        let prompt = principal
            .agent_prompt(&target_agent)
            .ok_or_else(|| {
                PrincipalManagerError::InvalidDecision(format!(
                    "agent prompt '{target_agent}' disappeared after routing"
                ))
            })?;

        let session_id = resume_session_id.unwrap_or_else(new_session_id);
        let sessions_dir = principal.memory.sessions_dir();
        let capabilities = principal.capabilities().clone();

        let response_text = run_agent_prompt(
            prompt,
            &capabilities,
            peer.clone(),
            input_message,
            session_id.clone(),
            sessions_dir,
            self.resolver.clone(),
        )
        .await
        .map_err(|e| {
            PrincipalManagerError::RouterError(RouterError::AgentFailed(e.to_string()))
        })?;

        // Record the session artifact so future routing can resume it.
        let artifact = SessionArtifact {
            session_id,
            peer,
            title: Some(target_agent),
            updated_at: chrono::Utc::now(),
            summary: Some(response_text.clone()),
        };
        if let Err(e) = principal.memory.record_session(artifact).await {
            tracing::warn!("failed to record principal session artifact: {e}");
        }

        Ok(PrincipalResponse::text(response_text))
    }
}

/// Response from a Principal.receive call.
#[derive(Debug, Clone)]
pub struct PrincipalResponse {
    pub content: String,
    pub deferred: bool,
}

impl PrincipalResponse {
    pub fn text(content: String) -> Self {
        Self {
            content,
            deferred: false,
        }
    }

    pub fn deferred(reason: String) -> Self {
        Self {
            content: reason,
            deferred: true,
        }
    }
}

async fn discover_agent_prompts(
    workspace_path: &Path,
    refs: &[super::config::PrincipalAgentRef],
) -> Result<HashMap<String, AgentPrompt>, PrincipalManagerError> {
    let mut prompts = HashMap::new();

    // 1. Discover AGENT.md extensions under <workspace>/agents/
    let agents_dir = workspace_path.join("agents");
    let adapter = AgentAdapter::new();
    let discovered = adapter.discover_agents(&agents_dir);
    for d in discovered {
        let prompt = load_agent_prompt(&d.file_path)
            .map_err(|e| PrincipalManagerError::Config(format!("{}: {e}", d.manifest.name)))?;
        prompts.insert(d.manifest.name.clone(), prompt);
    }

    // 2. Legacy fallback: merge any config.agents entries not yet discovered.
    for r in refs {
        if prompts.contains_key(&r.name) {
            continue;
        }
        let path = if r.prompt.is_absolute() {
            r.prompt.clone()
        } else {
            workspace_path.join(&r.prompt)
        };
        let prompt = load_agent_prompt(&path)
            .map_err(|e| PrincipalManagerError::Config(format!("{}: {e}", r.name)))?;
        prompts.insert(r.name.clone(), prompt);
    }

    Ok(prompts)
}

fn parse_agent_role(role: Option<&str>) -> super::config::AgentRole {
    match role.unwrap_or("default").to_lowercase().as_str() {
        "router" => super::config::AgentRole::Router,
        "specialist" => super::config::AgentRole::Specialist,
        _ => super::config::AgentRole::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Subject;
    use crate::engine::tool_runtime::ToolRuntime;
    use crate::extensions::framework::core::init_global_core;
    use crate::principal::{
        config::{
            AgentRole, PrincipalAgentRef, PrincipalCapabilities, PrincipalConfig,
            PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
            PrincipalMemoryConfig, PrincipalRoutingConfig,
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
        let tool_runtime = ToolRuntime::with_workspace(path_resolver, temp.path())
            .await
            .expect("tool runtime should initialize");
        init_global_core(tool_runtime.extension_core().clone());

        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = temp.path().join("providers.toml");
        let (resolver, adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;
        let manager = Arc::new(
            PrincipalManager::new(
                workspace,
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
            routing: PrincipalRoutingConfig {
                default_agent: "primary".to_string(),
                ..Default::default()
            },
            capabilities: PrincipalCapabilities::default(),
            agents: vec![PrincipalAgentRef {
                name: "primary".to_string(),
                prompt: PathBuf::from("agents/primary.md"),
                role: AgentRole::Default,
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
                manager
                    .receive(id, peer, format!("hello {i}"), cli_channel())
                    .await
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
}
