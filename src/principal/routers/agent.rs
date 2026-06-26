//! Agent-based Principal router.
//!
//! The `AgentRouter` runs a normal agent prompt (the built-in router agent, or a
//! user-supplied override) in a peer-scoped session.  The router agent does the
//! actual orchestration: it inspects principal memory/sessions, chooses
//! specialist agents from the catalog, and delegates via the existing `Agent`
//! tool and async task tools.
//!
//! From the Principal boundary's point of view, the router simply returns a
//! `RouteDecision::Respond` containing the router agent's final answer.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::principal::agent_prompt::{parse_agent_prompt, AgentPrompt};
use crate::principal::agent_runner::run_router_prompt;
use crate::principal::memory::{PrincipalMemory, SessionArtifact};
use crate::principal::router::{
    AgentPromptSummary, PrincipalRouter, RouteDecision, RouterContext, RouterError,
};
use crate::providers::LlmResolver;

/// Load the compiled-in router agent prompt.
pub fn default_router_prompt() -> AgentPrompt {
    let content = include_str!("../../resources/agents/router/AGENT.md");
    parse_agent_prompt("router", PathBuf::from("builtin:router"), content)
}

/// A Principal router powered by an agentic loop.
pub struct AgentRouter {
    memory: Arc<dyn PrincipalMemory>,
    resolver: Option<Arc<LlmResolver>>,
    router_prompt: AgentPrompt,
    workspace_path: PathBuf,
}

impl AgentRouter {
    /// Create a new router for the given Principal workspace.
    #[must_use]
    pub fn new(
        memory: Arc<dyn PrincipalMemory>,
        resolver: Option<Arc<LlmResolver>>,
        router_prompt: AgentPrompt,
        workspace_path: PathBuf,
    ) -> Self {
        Self {
            memory,
            resolver,
            router_prompt,
            workspace_path,
        }
    }
}

#[async_trait]
impl PrincipalRouter for AgentRouter {
    async fn route(&self, ctx: RouterContext) -> Result<RouteDecision, RouterError> {
        let peer = ctx.peer.clone();

        // Keep one router session per peer so the router conversation continues
        // across `receive` calls.  Using a stable id prevents us from accidentally
        // resuming a specialist session that happens to be the latest session for
        // this peer.
        let session_id = format!("router:{peer}");
        let sessions_dir = self.memory.sessions_dir();
        let available_agents: Vec<AgentPromptSummary> = ctx.available_agents.clone();

        let message = build_router_message(&ctx);

        let response = run_router_prompt(
            &self.router_prompt,
            &ctx.capabilities,
            peer.clone(),
            message,
            session_id.clone(),
            sessions_dir,
            self.resolver.clone(),
            self.workspace_path.clone(),
            available_agents,
            Arc::clone(&self.memory),
        )
        .await
        .map_err(|e| RouterError::AgentFailed(e.to_string()))?;

        // Record the router session artifact so future messages from this peer
        // can recall it as prior context.
        let artifact = SessionArtifact {
            session_id,
            peer,
            title: Some("router".to_string()),
            updated_at: chrono::Utc::now(),
            summary: Some(response.clone()),
        };
        if let Err(e) = self.memory.record_session(artifact).await {
            tracing::warn!("failed to record router session artifact: {e}");
        }

        Ok(RouteDecision::Respond { response })
    }
}

fn build_router_message(ctx: &RouterContext) -> String {
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

    #[test]
    fn test_default_router_prompt_loads() {
        let prompt = default_router_prompt();
        assert_eq!(prompt.name, "router");
        assert!(
            prompt.body.contains("agent_catalog"),
            "router prompt should mention agent_catalog"
        );
    }

    #[test]
    fn test_build_router_message_includes_context() {
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
        };

        let message = build_router_message(&ctx);
        assert!(message.contains("User (user:alice) says:"));
        assert!(message.contains("previous summary"));
    }
}
