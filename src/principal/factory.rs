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

/// Default router factory: creates the root-agent router for every
/// Principal. Resolution order for the root agent's prompt body:
///
///   1. `config.routing.supervisor_prompt` — explicit absolute path to
///      a Markdown file (legacy knob, retained for now).
///   2. `<workspace>/agents/<name>.md` — workspace-relative Markdown
///      file matching the principal's configured root agent name.
///   3. Compiled-in default (`builtin:agent:supervisor`).
///
/// Phase 5 will rename "supervisor" → "root" across this surface and
/// promote step 2's `<name>.md` resolution into the canonical
/// extension lookup keyed by `config.agent.root`.
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
        let prompt = Self::resolve_root_agent_prompt(config, workspace_path);
        // Phase 4b: copy the principal's stable DID into the router
        // so `principal_send` is registered on this Principal's
        // agents. The runtime_id is left as `None`; the daemon-state
        // bootstrap sets it once `start_tunnel` finishes and the
        // `CrossRuntimeA2aCtx` is installed.
        let principal_caller_did = config.did.clone().map(|d| d.0);
        Arc::new(super::routers::SupervisorRouter::new(
            memory,
            resolver,
            prompt,
            workspace_path.to_path_buf(),
            config.preferred_provider_id.clone(),
            config.preferred_model_id.clone(),
            principal_caller_did,
        ))
    }
}

impl DefaultPrincipalRouterFactory {
    /// Resolve the principal's root agent prompt body.
    ///
    /// See [`DefaultPrincipalRouterFactory`] for the resolution order.
    pub fn resolve_root_agent_prompt(
        config: &PrincipalConfig,
        workspace_path: &std::path::Path,
    ) -> super::agent_prompt::AgentPrompt {
        // 1. Explicit override from principal.toml.
        if let Some(ref path) = config.routing.supervisor_prompt {
            match super::agent_prompt::load_agent_prompt(path) {
                Ok(prompt) => return prompt,
                Err(e) => tracing::warn!(
                    "Failed to load supervisor prompt from {}: {e}. Falling back to defaults.",
                    path.display()
                ),
            }
        }

        // 2. Workspace-relative Markdown. Check both layouts so users
        //    can put the file at either `agents/supervisor/AGENT.md`
        //    or flat `agents/supervisor.md`.
        let workspace_candidates = [
            workspace_path.join("agents").join("supervisor").join("AGENT.md"),
            workspace_path.join("agents").join("supervisor.md"),
        ];
        for candidate in &workspace_candidates {
            if candidate.exists() {
                match super::agent_prompt::load_agent_prompt(candidate) {
                    Ok(prompt) => return prompt,
                    Err(e) => tracing::warn!(
                        "Failed to load workspace root agent prompt from {}: {e}. \
                         Falling back to built-in default.",
                        candidate.display()
                    ),
                }
            }
        }

        // 3. Compiled-in default.
        super::routers::default_supervisor_prompt()
    }
}

/// Re-export concrete factory types for ergonomics.
pub use super::memory::DefaultPrincipalMemory;
