use async_trait::async_trait;
use std::sync::Arc;

use crate::auth::Subject;
use crate::principal::{
    memory::{PrincipalMemory, SessionArtifact},
    router::{PrincipalRouter, RouteDecision, RouterContext, RouterError},
};

/// Deterministic `builtin:default` router.
///
/// - Resumes the most recent peer-specific session if one exists.
/// - Otherwise starts a fresh session with `principal.routing.default_agent`.
/// - Always rewrites the input message to include the caller identity.
pub struct BuiltinDefaultRouter {
    memory: Arc<dyn PrincipalMemory>,
}

impl BuiltinDefaultRouter {
    pub fn new(memory: Arc<dyn PrincipalMemory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl PrincipalRouter for BuiltinDefaultRouter {
    async fn route(
        &self,
        ctx: RouterContext,
    ) -> Result<RouteDecision, RouterError> {
        let default_agent = ctx.routing.default_agent.clone();

        // Find the most recent session for this peer, if any.
        let latest = self
            .memory
            .find_latest_session_for_peer(&ctx.peer)
            .await
            .map_err(|e| RouterError::AgentFailed(e.to_string()))?;

        let input_message = build_input_message(&ctx.peer, ctx.message);

        if let Some(SessionArtifact { session_id, .. }) = latest {
            return Ok(RouteDecision::Continue {
                target_agent: default_agent,
                input_message,
                resume_session_id: Some(session_id),
                context_injection: Vec::new(),
                synthesize: false,
                async_execution: false,
                timeout_seconds: None,
            });
        }

        Ok(RouteDecision::Spawn {
            target_agent: default_agent,
            input_message,
            context_injection: Vec::new(),
            synthesize: false,
            async_execution: false,
            timeout_seconds: None,
        })
    }
}

/// Build the input message seen by the target agent prompt.
/// Includes the caller identity so the agent prompt can personalize.
fn build_input_message(peer: &Subject, user_message: String) -> String {
    format!("Caller: {}\n\n{}", peer, user_message)
}
