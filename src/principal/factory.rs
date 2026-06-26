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

/// Default router factory: creates the router specified by
/// `config.routing.strategy`. For the first slice only
/// `RoutingStrategy::BuiltinDefault` is supported.
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
        match config.routing.strategy {
            super::RoutingStrategy::BuiltinDefault => {
                Arc::new(super::routers::BuiltinDefaultRouter::new(memory))
            }
            super::RoutingStrategy::Supervisor { ref supervisor_prompt } => {
                let prompt = match supervisor_prompt {
                    Some(path) => super::agent_prompt::load_agent_prompt(path)
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
            _ => {
                // Fallback to builtin for any unimplemented strategy in the first slice.
                Arc::new(super::routers::BuiltinDefaultRouter::new(memory))
            }
        }
    }
}

/// Re-export concrete factory types for ergonomics.
pub use super::memory::DefaultPrincipalMemory;
