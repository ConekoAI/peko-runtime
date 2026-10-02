//! Built-in Tool Adapter
//!
//! Registers native Tool trait implementations with the
//! [`ToolCatalog`](crate::tools::catalog::ToolCatalog) (ADR-066 D2 —
//! the pre-P3 path built a `BuiltinExecuteHandler` per tool and fired
//! it through the hook registry; tools now dispatch straight from the
//! catalog via `ToolDispatcher`).
//!
//! ## Usage
//! ```rust,ignore
//! let bash = Arc::new(BashTool::new());
//! BuiltinToolAdapter::register_tool(&catalog, bash, PrincipalId::system()).await?;
//! ```

use anyhow::Result;
use peko_subject::PrincipalId;
use peko_tools_core::Tool;
use std::sync::Arc;

use crate::extensions::framework::types::ToolSource;
use crate::tools::catalog::ToolCatalog;

/// Adapter for registering built-in tools with the catalog.
#[derive(Debug)]
pub struct BuiltinToolAdapter;

impl BuiltinToolAdapter {
    /// Register a built-in tool under the given principal scope.
    /// Idempotent — re-registering the same `(name, principal_id)`
    /// overwrites.
    pub async fn register_tool(
        catalog: &ToolCatalog,
        tool: Arc<dyn Tool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        catalog
            .register(tool, ToolSource::BuiltIn, principal_id)
            .await;
        Ok(())
    }

    /// Register multiple tools under the same principal scope.
    pub async fn register_tools(
        catalog: &ToolCatalog,
        tools: Vec<Arc<dyn Tool>>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        for tool in tools {
            Self::register_tool(catalog, tool, principal_id).await?;
        }
        Ok(())
    }

    /// Register a single global built-in tool under
    /// [`PrincipalId::system`](peko_subject::PrincipalId::system).
    ///
    /// This is the canonical call shape for the daemon-init path: built-ins
    /// are visible to every principal and registered exactly once on the
    /// shared catalog.
    pub async fn register_tool_system(catalog: &ToolCatalog, tool: Arc<dyn Tool>) -> Result<()> {
        Self::register_tool(catalog, tool, PrincipalId::system()).await
    }

    /// Register multiple global built-in tools.
    pub async fn register_tools_system(
        catalog: &ToolCatalog,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Result<()> {
        Self::register_tools(catalog, tools, PrincipalId::system()).await
    }

    /// Register `AsyncSpawn` with per-agent wiring.
    ///
    /// Registered under the calling agent's `principal_id` rather than
    /// the system scope, so each agent gets its own
    /// `(AsyncSpawn, principal_id)` entry — the per-agent async family
    /// introspection scoping.
    pub async fn register_async_spawn_tool(
        catalog: &ToolCatalog,
        tool: Arc<crate::tools::builtin::AsyncSpawnTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(catalog, tool, principal_id).await
    }

    /// Register `AsyncOutput` with per-agent wiring.
    ///
    /// Registered under the calling agent's `principal_id` (see
    /// `register_async_spawn_tool` for rationale).
    pub async fn register_async_output_tool(
        catalog: &ToolCatalog,
        tool: Arc<crate::tools::builtin::AsyncOutputTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(catalog, tool, principal_id).await
    }

    /// Phase 2 of `feature/multi-model-subagents`: register the
    /// `model_list` builtin so the parent agent can discover what
    /// models the principal has configured before picking which one
    /// to spawn a subagent against.
    ///
    /// Registered under the calling agent's `principal_id` (not the
    /// system scope) so each agent gets its own `model_list`
    /// instance whose `Weak<ModelCatalog>` upgrades to the
    /// principal's catalog at execute time.
    pub async fn register_model_list_tool(
        catalog: &ToolCatalog,
        tool: Arc<crate::tools::builtin::ModelListTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(catalog, tool, principal_id).await
    }

    /// Get list of globally-registered built-in tool names.
    ///
    /// These tools are registered once at daemon startup by
    /// `engine::ToolRuntime::register_builtins` and are shared across
    /// all agents.
    #[must_use]
    pub fn global_tool_names() -> Vec<&'static str> {
        crate::principal::runtime::builtin_tools::GLOBAL_TOOL_NAMES.to_vec()
    }

    /// Get list of agent-specific built-in tool names.
    ///
    /// These tools require agent-specific runtime dependencies
    /// (e.g. `SubagentExecutor`, caller identity) and are registered
    /// per-agent in `Agent::init_builtins_async()`.
    #[must_use]
    pub fn agent_specific_tool_names() -> Vec<&'static str> {
        crate::principal::runtime::builtin_tools::AGENT_SPECIFIC_TOOL_NAMES.to_vec()
    }

    /// Get list of ALL built-in tool names (global + agent-specific).
    #[must_use]
    pub fn all_tool_names() -> Vec<&'static str> {
        crate::principal::runtime::builtin_tools::all_tool_names()
    }

    /// Check if a tool name is a built-in tool (global or agent-specific).
    #[must_use]
    pub fn is_builtin(name: &str) -> bool {
        crate::principal::runtime::builtin_tools::is_builtin_tool(name)
    }

    /// Check if a tool name is an agent-specific built-in (registered per-agent).
    #[must_use]
    pub fn is_agent_specific_builtin(name: &str) -> bool {
        crate::principal::runtime::builtin_tools::is_agent_specific_builtin_tool(name)
    }
}
