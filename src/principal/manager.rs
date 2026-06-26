use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{
    agent_prompt::load_agent_prompt,
    config::PrincipalConfig,
    factory::{PrincipalMemoryFactory, PrincipalRouterFactory},
    router::{ChannelContext, RouteDecision, RouterContext, RouterError},
    AgentPrompt, Principal, PrincipalId,
};
use crate::auth::Subject;

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
        }
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
        let router = self.router_factory.create(&config, memory.clone()).await;

        let agent_prompts = load_agent_prompts(&workspace_path,
            &config.agents,
        )
        .await?;

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
                role: crate::principal::config::AgentRole::Default,
                description: p.frontmatter.description.clone(),
            })
            .collect();

        let ctx = RouterContext {
            principal_id: principal.id.clone(),
            principal_name: principal.config.name.clone(),
            peer,
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

        match decision {
            RouteDecision::Respond { response } => Ok(PrincipalResponse::text(response)),
            RouteDecision::Defer { reason } => Ok(PrincipalResponse::deferred(reason)),
            _ => {
                // Phase 1: execution is not yet implemented.
                Ok(PrincipalResponse::text(
                    "[Phase 1] Routing decision accepted; execution not yet wired.".to_string(),
                ))
            }
        }
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

async fn load_agent_prompts(
    workspace_path: &Path,
    refs: &[super::config::PrincipalAgentRef],
) -> Result<HashMap<String, AgentPrompt>, PrincipalManagerError> {
    let mut prompts = HashMap::new();
    for r in refs {
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
