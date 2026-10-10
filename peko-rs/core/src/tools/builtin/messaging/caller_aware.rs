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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::{DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory};
    use serde_json::json;

    fn new_args() -> Value {
        json!({"action": "new", "path": "worker", "prompt": "go", "role": "primary"})
    }

    /// The daemon fallback needs a calling principal: no context, or a
    /// blank principal name, is refused before any principal lookup; a
    /// named caller is looked up.
    #[tokio::test]
    async fn requires_a_named_calling_principal() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(PrincipalManager::with_path_resolver(
            crate::common::paths::PathResolver::with_dirs(
                dir.path().join("config"),
                dir.path().join("data"),
                dir.path().join("cache"),
            ),
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::async_exec::executor::standalone_inbox_registry(),
        ));
        let tool = CallerAwareAgentTool::new(
            Arc::downgrade(&manager),
            Arc::new(peko_observability::Observability::new("test")),
        );

        let error = tool.execute(new_args()).await.unwrap_err();
        assert!(
            error.to_string().contains("calling-principal context"),
            "{error}"
        );

        let ctx = |name: &str| {
            ToolContext::for_hook_run("run", "call", "Agent").with_principal_name(name)
        };
        let error = tool
            .execute_with_context(new_args(), &ctx("   "))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("calling-principal context"),
            "{error}"
        );
        let error = tool
            .execute_with_context(new_args(), &ctx("alice"))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("unknown principal 'alice'"),
            "{error}"
        );
    }
}
