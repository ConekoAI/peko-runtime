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

use crate::tools::catalog::ToolCatalog;
use crate::tools::metadata::ToolSource;

/// Adapter for registering built-in tools with the catalog.
#[derive(Debug)]
pub struct BuiltinToolAdapter;

impl BuiltinToolAdapter {
    /// Register a built-in tool under the given principal scope.
    /// Explicitly replaces an existing `(name, principal_id)` binding.
    /// Production defaults use the non-replacing installation functions.
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

    /// Register a principal-owned async binding in the supplied catalog.
    /// Production installation shares one executor across the principal's turns.
    pub async fn register_async_spawn_tool(
        catalog: &ToolCatalog,
        tool: Arc<crate::tools::builtin::AsyncSpawnTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(catalog, tool, principal_id).await
    }

    /// Register the companion principal-owned async output binding.
    pub async fn register_async_output_tool(
        catalog: &ToolCatalog,
        tool: Arc<crate::tools::builtin::AsyncOutputTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(catalog, tool, principal_id).await
    }

    /// Register a run-owned ModelList binding. Supply the run overlay catalog
    /// so one run's model-list configuration cannot change another run's tools.
    pub async fn register_model_list_tool(
        catalog: &ToolCatalog,
        tool: Arc<crate::tools::builtin::ModelListTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(catalog, tool, principal_id).await
    }

    /// Get list of globally-registered built-in tool names.
    ///
    /// These defaults are assembled by the runtime and daemon phases of
    /// tools::installation, and inherited by every principal/run.
    #[must_use]
    pub fn global_tool_names() -> Vec<&'static str> {
        crate::tools::installation::names_for_scope(
            crate::tools::installation::BuiltinScope::Runtime,
        )
    }

    /// Get list of agent-specific built-in tool names.
    ///
    /// These tools bind a run's executor or model configuration in its
    /// private catalog overlay. Principal services have their own lifetime.
    #[must_use]
    pub fn agent_specific_tool_names() -> Vec<&'static str> {
        crate::tools::installation::AGENT_SPECIFIC_TOOL_NAMES.to_vec()
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
