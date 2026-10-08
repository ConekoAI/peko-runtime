//! Caller-aware daemon fallback for Agent calls outside an active loop.
//! Uses the same principal turn builder as peer ingress; no run's executor
//! is installed into the shared catalog.

use super::AgentTool;
use crate::principal::manager::PrincipalManager;
use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::Value;
use std::sync::{Arc, Weak};

pub(crate) struct CallerAwareAgentTool {
    manager: Weak<PrincipalManager>,
    observability: Arc<peko_observability::Observability>,
}

impl CallerAwareAgentTool {
    pub(crate) fn new(
        manager: Weak<PrincipalManager>,
        observability: Arc<peko_observability::Observability>,
    ) -> Self {
        Self {
            manager,
            observability,
        }
    }
}

#[async_trait]
impl Tool for CallerAwareAgentTool {
    fn name(&self) -> &str {
        "Agent"
    }
    fn description(&self) -> String {
        AgentTool::tool_description()
    }
    fn parameters(&self) -> Value {
        AgentTool::tool_parameters()
    }
    async fn execute(&self, _: Value) -> anyhow::Result<Value> {
        anyhow::bail!("Agent requires a calling-principal context")
    }
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        AgentTool::validate_params(&params)?;
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Agent: PrincipalManager unavailable"))?;
        let name = ctx
            .principal_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("Agent requires a calling-principal context"))?;
        let principal = manager
            .get_by_name(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Agent: unknown principal '{name}'"))?;
        let resolver = manager
            .llm_resolver()
            .ok_or_else(|| anyhow::anyhow!("Agent: no LLM resolver bound"))?;
        let tooling = manager.tooling();
        let turns = crate::principal::child_turns::PeerChildTurns::build(
            &principal,
            &resolver,
            Arc::clone(&self.observability),
            Some(manager.shared_inbox_registry()),
            Arc::clone(&tooling),
        )
        .await?;
        let surface = tooling.services().channel_port().map(|port| {
            Arc::new(crate::principal::child_turns::PeerTurnSurfaceImpl::new(
                Arc::clone(turns.session_manager()),
                port,
                principal.id.clone(),
            )) as Arc<dyn crate::agents::subagent_executor::PeerTurnSurface>
        });
        let executor = turns.executor().clone().with_peer_turn_surface(surface);
        AgentTool::new(Arc::new(
            crate::agents::subagent_runtime_impl::SubagentExecutorRuntime::new(Arc::new(executor)),
        ))
        .execute_with_context(params, ctx)
        .await
    }
}
