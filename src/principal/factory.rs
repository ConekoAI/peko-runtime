use std::sync::Arc;

use async_trait::async_trait;

use super::{memory::PrincipalMemory, PrincipalConfig, PrincipalId};
use crate::providers::LlmResolver;

/// Factory for building a Principal's memory store.
#[async_trait]
pub trait PrincipalMemoryFactory: Send + Sync {
    async fn create(
        &self,
        principal_id: &PrincipalId,
        workspace_path: &std::path::Path,
    ) -> Arc<dyn PrincipalMemory>;
}

/// Factory for building a Principal's router.
#[async_trait]
pub trait PrincipalRouterFactory: Send + Sync {
    async fn create(
        &self,
        config: &PrincipalConfig,
        memory: Arc<dyn PrincipalMemory>,
        workspace_path: &std::path::Path,
        resolver: Option<Arc<LlmResolver>>,
    ) -> Arc<dyn super::router::PrincipalRouter>;
}

/// Default memory factory: creates a `DefaultPrincipalMemory`.
pub struct DefaultPrincipalMemoryFactory;

#[async_trait]
impl PrincipalMemoryFactory for DefaultPrincipalMemoryFactory {
    async fn create(
        &self,
        _principal_id: &PrincipalId,
        workspace_path: &std::path::Path,
    ) -> Arc<dyn PrincipalMemory> {
        // Ensure the sessions directory exists.
        let memory = super::memory::DefaultPrincipalMemory::new(workspace_path.to_path_buf());
        let _ = tokio::fs::create_dir_all(memory.sessions_dir()).await;
        Arc::new(memory)
    }
}

/// Default router factory: creates the supervisor agent router for every
/// Principal.  Users can customize the supervisor by editing
/// `agents/supervisor/AGENT.md` or setting `routing.supervisor_prompt`.
pub struct DefaultPrincipalRouterFactory;

#[async_trait]
impl PrincipalRouterFactory for DefaultPrincipalRouterFactory {
    async fn create(
        &self,
        config: &PrincipalConfig,
        memory: Arc<dyn PrincipalMemory>,
        workspace_path: &std::path::Path,
        resolver: Option<Arc<LlmResolver>>,
    ) -> Arc<dyn super::router::PrincipalRouter> {
        let prompt = match config.routing.supervisor_prompt {
            Some(ref path) => super::agent_prompt::load_agent_prompt(path)
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        "Failed to load supervisor prompt from {}: {e}. Using built-in supervisor.",
                        path.display()
                    );
                    super::routers::default_supervisor_prompt()
                }),
            None => super::routers::default_supervisor_prompt(),
        };
        Arc::new(super::routers::SupervisorRouter::new(
            memory,
            resolver,
            prompt,
            workspace_path.to_path_buf(),
        ))
    }
}

/// Re-export concrete factory types for ergonomics.
pub use super::memory::DefaultPrincipalMemory;
