use std::sync::Arc;

use async_trait::async_trait;

use crate::principal::memory::PrincipalMemory;
use crate::principal::PrincipalConfig;
use peko_providers::LlmResolver;
use peko_subject::PrincipalId;
// F19: removed `use peko_quota::QuotaMeter;` — the factory no
// longer threads the meter through to the router.

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
        // PR #2 wiring: per-Principal plan DAG port. Threaded into
        // `RootRouter::new` and then into every `PrincipalContext`
        // produced by `RootRouter::build_context` so the seven
        // `PePlan*` tools can be wired into the principal's agents.
        plan_port: Arc<dyn peko_plan::PlanPort>,
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
        let memory =
            crate::principal::memory::DefaultPrincipalMemory::new(workspace_path.to_path_buf());
        let _ = tokio::fs::create_dir_all(memory.sessions_dir()).await;
        Arc::new(memory)
    }
}

/// Default router factory: creates the root-agent router for every
/// Principal. Resolution order for the root agent's prompt body:
///
///   1. `config.routing.root_prompt` — explicit absolute path to a
///      Markdown file (legacy knob, retained for now).
///   2. `<workspace>/agents/<name>.md` — workspace-relative Markdown
///      file matching the principal's configured root agent name.
///   3. Compiled-in default (`builtin:agent:root`).
pub struct DefaultPrincipalRouterFactory;

#[async_trait]
impl PrincipalRouterFactory for DefaultPrincipalRouterFactory {
    async fn create(
        &self,
        config: &PrincipalConfig,
        memory: Arc<dyn PrincipalMemory>,
        workspace_path: &std::path::Path,
        resolver: Option<Arc<LlmResolver>>,
        plan_port: Arc<dyn peko_plan::PlanPort>,
    ) -> Arc<dyn super::router::PrincipalRouter> {
        // Phase A: derive the typed agents dir from the Shared
        // tier root (the `workspace_path` parameter is the
        // Shared root under Phase A).
        let agents_dir = workspace_path.join("agents");
        let prompt = Self::resolve_root_agent_prompt(config, &agents_dir);
        // Phase 4b: copy the principal's stable DID into the router
        // so `principal_send` is registered on this Principal's
        // agents. The runtime_id is left as `None`; the daemon-state
        // bootstrap sets it once `start_tunnel` finishes and the
        // `CrossRuntimeA2aCtx` is installed.
        let principal_caller_did = config.did.clone().map(|d| d.0);
        // F19: removed `quota_meter` parameter — the engine loop
        // fetches the principal's meter directly from
        // `Principal.quota_meter` at run entrypoint.
        Arc::new(super::routers::RootRouter::new(
            memory,
            resolver,
            prompt,
            workspace_path.to_path_buf(),
            config.preferred_model_id.clone(),
            principal_caller_did,
            plan_port,
        ))
    }
}

impl DefaultPrincipalRouterFactory {
    /// Resolve the principal's root agent prompt body.
    ///
    /// See [`DefaultPrincipalRouterFactory`] for the resolution order.
    ///
    /// **Phase A.** Callers pass the typed `SharedLayout::agents_dir`
    /// directly rather than the Shared root + a hand-rolled
    /// `"agents"` suffix.
    pub fn resolve_root_agent_prompt(
        config: &PrincipalConfig,
        agents_dir: &std::path::Path,
    ) -> crate::principal::agent_prompt::AgentPrompt {
        // 1. Explicit override from principal.toml.
        if let Some(ref path) = config.routing.root_prompt {
            match crate::principal::agent_prompt::load_agent_prompt(path) {
                Ok(prompt) => return prompt,
                Err(e) => tracing::warn!(
                    "Failed to load root prompt from {}: {e}. Falling back to defaults.",
                    path.display()
                ),
            }
        }

        // 2. Workspace-relative Markdown. Check both layouts so users
        //    can put the file at either `agents/root/AGENT.md` or flat
        //    `agents/root.md`.
        let workspace_candidates = [
            agents_dir.join("root").join("AGENT.md"),
            agents_dir.join("root.md"),
        ];
        for candidate in &workspace_candidates {
            if candidate.exists() {
                match crate::principal::agent_prompt::load_agent_prompt(candidate) {
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
        super::routers::default_root_prompt()
    }
}
