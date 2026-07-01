//! Root-agent-based Principal router (formerly the "supervisor" router).
//!
//! The `SupervisorRouter` runs a normal agent prompt (the built-in root
//! agent, or a user-supplied override) in a peer-scoped session.  The
//! root agent does the actual orchestration: it inspects principal
//! memory/sessions, chooses specialist agents from the catalog, and
//! delegates via the existing `Agent` tool and async task tools.
//!
//! From the Principal boundary's point of view, the root agent simply
//! returns a `RouteDecision::Respond` containing the agent's final
//! answer.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::engine::AgenticEvent;
use crate::principal::agent_prompt::{parse_agent_prompt, AgentPrompt};
use crate::principal::agent_runner::{run_supervisor_prompt, run_supervisor_prompt_streaming};
use crate::principal::context::PrincipalContext;
use crate::principal::memory::{PrincipalMemory, SessionArtifact};
use crate::principal::router::{
    AgentPromptSummary, PrincipalRouter, RouteDecision, RouterContext, RouterError,
};
use crate::providers::LlmResolver;

/// Load the compiled-in root agent prompt.
pub fn default_supervisor_prompt() -> AgentPrompt {
    let content = include_str!("../../resources/agents/supervisor/AGENT.md");
    parse_agent_prompt("supervisor", PathBuf::from("builtin:supervisor"), content)
}

/// Stable root-agent session id for a peer.
#[must_use]
pub fn supervisor_session_id(peer: &crate::auth::Subject) -> String {
    format!("supervisor:{peer}")
}

/// A Principal router powered by a root-agent agentic loop.
///
/// Holds a cached `PrincipalContext` for the principal's lifetime; the
/// shared per-principal `ExtensionCore` lives on the context and is
/// reused across messages.
pub struct SupervisorRouter {
    memory: Arc<dyn PrincipalMemory>,
    resolver: Option<Arc<LlmResolver>>,
    supervisor_prompt: AgentPrompt,
    workspace_path: PathBuf,
    /// Per-Principal provider preference from `principal.toml`. When
    /// `Some`, it overrides the global catalog default for any LLM call
    /// routed through this Principal's root agent. When `None`, the
    /// catalog default wins.
    principal_provider_id: Option<String>,
    principal_model_id: Option<String>,
}

impl SupervisorRouter {
    /// Create a new supervisor router for the given Principal workspace.
    ///
    /// `principal_provider_id` / `principal_model_id` are the values from
    /// `PrincipalConfig::preferred_provider_id` / `preferred_model_id`.
    /// Pass `None, None` to inherit the global catalog default.
    #[must_use]
    pub fn new(
        memory: Arc<dyn PrincipalMemory>,
        resolver: Option<Arc<LlmResolver>>,
        supervisor_prompt: AgentPrompt,
        workspace_path: PathBuf,
        principal_provider_id: Option<String>,
        principal_model_id: Option<String>,
    ) -> Self {
        Self {
            memory,
            resolver,
            supervisor_prompt,
            workspace_path,
            principal_provider_id,
            principal_model_id,
        }
    }

    /// Build a `PrincipalContext` from the router's already-resolved
    /// state plus the per-call `RouterContext` (which carries the
    /// per-message pieces: inbox registry, session-creation lock, and
    /// the current capabilities snapshot).
    fn build_context(
        &self,
        ctx: &RouterContext,
    ) -> PrincipalContext {
        let principal_ctx = PrincipalContext::new(
            self.workspace_path.clone(),
            Arc::clone(&self.memory),
            Arc::clone(&ctx.inbox_registry),
            Arc::clone(&ctx.session_creation_lock),
            Arc::new(ctx.capabilities.clone()),
            self.resolver.clone(),
            (
                self.principal_provider_id.clone(),
                self.principal_model_id.clone(),
            ),
        );
        principal_ctx.set_root_prompt(self.supervisor_prompt.clone());
        principal_ctx
    }
}

#[async_trait]
impl PrincipalRouter for SupervisorRouter {
    async fn route(&self, ctx: RouterContext) -> Result<RouteDecision, RouterError> {
        let peer = ctx.peer.clone();
        let session_id = supervisor_session_id(&peer);
        let available_agents: Vec<AgentPromptSummary> = ctx.available_agents.clone();
        let message = build_supervisor_message(&ctx);
        let principal_ctx = self.build_context(&ctx);

        let response = run_supervisor_prompt(
            &self.supervisor_prompt,
            peer.clone(),
            message,
            session_id.clone(),
            available_agents,
            &principal_ctx,
        )
        .await
        .map_err(|e| RouterError::AgentFailed(e.to_string()))?;

        // Record the root-agent session artifact so future messages
        // from this peer can recall it as prior context.
        let artifact = SessionArtifact {
            session_id,
            peer,
            title: Some("supervisor".to_string()),
            updated_at: chrono::Utc::now(),
            summary: Some(response.clone()),
        };
        if let Err(e) = self.memory.record_session(artifact).await {
            tracing::warn!("failed to record root-agent session artifact: {e}");
        }

        Ok(RouteDecision::Respond { response })
    }

    async fn route_streaming(
        &self,
        ctx: RouterContext,
        on_event: Box<dyn Fn(AgenticEvent) + Send + Sync>,
    ) -> Result<RouteDecision, RouterError> {
        let peer = ctx.peer.clone();
        let session_id = supervisor_session_id(&peer);
        let available_agents: Vec<AgentPromptSummary> = ctx.available_agents.clone();
        let message = build_supervisor_message(&ctx);
        let principal_ctx = self.build_context(&ctx);

        let response = run_supervisor_prompt_streaming(
            &self.supervisor_prompt,
            peer.clone(),
            message,
            session_id.clone(),
            available_agents,
            &principal_ctx,
            on_event,
        )
        .await
        .map_err(|e| RouterError::AgentFailed(e.to_string()))?;

        // Record the root-agent session artifact so future messages
        // from this peer can recall it as prior context.
        let artifact = SessionArtifact {
            session_id,
            peer,
            title: Some("supervisor".to_string()),
            updated_at: chrono::Utc::now(),
            summary: Some(response.clone()),
        };
        if let Err(e) = self.memory.record_session(artifact).await {
            tracing::warn!("failed to record root-agent session artifact: {e}");
        }

        Ok(RouteDecision::Respond { response })
    }
}

fn build_supervisor_message(ctx: &RouterContext) -> String {
    let mut parts = Vec::new();

    if !ctx.recalled_context.is_empty() {
        parts.push("Prior context:".to_string());
        for injection in &ctx.recalled_context {
            parts.push(format!(
                "- [{} {}]: {}",
                format!("{:?}", injection.kind).to_lowercase(),
                injection.id,
                injection.content
            ));
        }
    }

    parts.push(format!("User ({}) says:\n{}", ctx.peer, ctx.message));

    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Subject;
    use crate::principal::config::{
        AgentRole, PrincipalCapabilities, PrincipalGovernanceConfig, PrincipalIntentConfig,
        PrincipalRoutingConfig,
    };
    use crate::principal::router::{ChannelContext, ChannelKind, ContextInjectionKind};
    use crate::session::InboxRegistry;

    #[test]
    fn test_default_supervisor_prompt_loads() {
        let prompt = default_supervisor_prompt();
        assert_eq!(prompt.name, "supervisor");
        assert!(
            prompt.body.contains("agent_catalog"),
            "root agent prompt should mention agent_catalog"
        );
    }

    #[test]
    fn test_build_supervisor_message_includes_context() {
        let ctx = RouterContext {
            principal_id: crate::principal::PrincipalId::generate(),
            principal_name: "test".to_string(),
            peer: Subject::User("alice".to_string()),
            message: "hello".to_string(),
            channel: ChannelContext {
                kind: ChannelKind::Cli,
                streaming: false,
            },
            routing: PrincipalRoutingConfig::default(),
            recalled_context: vec![crate::principal::router::ContextInjection {
                kind: ContextInjectionKind::Session,
                id: "s1".to_string(),
                content: "previous summary".to_string(),
            }],
            available_agents: vec![AgentPromptSummary {
                name: "primary".to_string(),
                role: AgentRole::Default,
                description: Some("Generalist".to_string()),
            }],
            capabilities: PrincipalCapabilities::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            inbox_registry: Arc::new(InboxRegistry::new()),
            session_creation_lock: Arc::new(tokio::sync::Mutex::new(())),
        };

        let message = build_supervisor_message(&ctx);
        assert!(message.contains("User (user:alice) says:"));
        assert!(message.contains("previous summary"));
    }
}
