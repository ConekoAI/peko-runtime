//! ToolRuntime - Standalone tool execution environment
//!
//! Phase 9b.N.2 trimmed this file: the F37 `execute_tool_via_core` and
//! `execute_tool_via_core_with_context` helpers have no `BashTool`
//! coupling, so they lifted cleanly into [`crate::engine::funnel`]
//! (re-exported from `peko_engine::funnel`) — see PR #266. The
//! surrounding `ToolRuntime` struct + `register_builtins` stay in root
//! because the concrete `BashTool` registration still references
//! `src/tools/builtin/bash.rs` (Phase 10 didn't move BashTool);
//! lifting the whole file would require lifting BashTool into
//! `peko-tools-builtin` first.
//!
//! Extracted from `Agent::init_builtins_async()` to allow the daemon
//! (and other non-agent contexts) to resolve and execute built-in tools.

use crate::common::paths::PathResolver;
use crate::extensions::builtin::BuiltinToolAdapter;
use crate::extensions::framework::core::{ExtensionCore, ExtensionServices};
use crate::tools::{
    BashTool, CronCreateTool, CronDeleteTool, CronListTool, EditTool, GlobTool, GrepTool, ReadTool,
    WriteTool,
};
use anyhow::Result;
use peko_tools_core::Tool;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// Standalone tool execution environment
///
/// `ToolRuntime` provides a lightweight context for registering and
/// executing built-in tools without requiring a full `Agent` instance.
/// It is used by:
/// - `Agent` (delegated from `init_builtins_async`)
/// - The daemon (for async task execution)
/// - Future job runners (cron, webhooks, etc.)
#[derive(Debug, Clone)]
pub struct ToolRuntime {
    extension_core: Arc<ExtensionCore>,
    path_resolver: PathResolver,
    workspace: PathBuf,
}

impl ToolRuntime {
    /// Create a new `ToolRuntime` with the given path resolver
    ///
    /// # Errors
    /// Returns an error if built-in tool registration fails
    pub async fn new(path_resolver: PathResolver) -> Result<Self> {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_workspace(path_resolver, workspace).await
    }

    /// Create with a specific workspace
    pub async fn with_workspace(
        path_resolver: PathResolver,
        workspace: impl Into<PathBuf>,
    ) -> Result<Self> {
        let workspace = workspace.into();
        let extension_core = Arc::new(ExtensionCore::new());
        Self::register_builtins(&extension_core, &path_resolver).await?;

        Ok(Self {
            extension_core,
            path_resolver,
            workspace,
        })
    }

    /// Create with a specific workspace and an existing ExtensionCore
    ///
    /// Used by the daemon to register tools with the global ExtensionCore
    /// so that agents created later can find them.
    pub async fn with_workspace_and_core(
        path_resolver: PathResolver,
        workspace: impl Into<PathBuf>,
        extension_core: Arc<ExtensionCore>,
    ) -> Result<Self> {
        let workspace = workspace.into();
        Self::register_builtins(&extension_core, &path_resolver).await?;

        Ok(Self {
            extension_core,
            path_resolver,
            workspace,
        })
    }

    /// Create a new `ToolRuntime` with custom extension services
    ///
    /// This is useful when the caller wants to inject a pre-configured
    /// `ExtensionServices` (e.g. with a custom `AsyncExecutionRouter`).
    pub async fn with_services(
        path_resolver: PathResolver,
        services: Arc<ExtensionServices>,
    ) -> Result<Self> {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_services_and_workspace(path_resolver, services, workspace).await
    }

    /// Create with custom services and workspace
    pub async fn with_services_and_workspace(
        path_resolver: PathResolver,
        services: Arc<ExtensionServices>,
        workspace: impl Into<PathBuf>,
    ) -> Result<Self> {
        let workspace = workspace.into();
        let extension_core = Arc::new(ExtensionCore::with_services(services));
        Self::register_builtins(&extension_core, &path_resolver).await?;

        Ok(Self {
            extension_core,
            path_resolver,
            workspace,
        })
    }

    /// Register built-in tools with the given `ExtensionCore`
    ///
    /// This logic is extracted from `Agent::init_builtins_async()`.
    ///
    /// `AsyncSpawn` and `AsyncOutput` are **NOT** registered here. They
    /// depend on per-agent state (AsyncExecutor + ExtensionCore for
    /// spawn-side lookups). Each agent registers its own via
    /// `BuiltinToolAdapter::register_async_spawn_tool` and
    /// `BuiltinToolAdapter::register_async_output_tool` once the agent
    /// has constructed its executor and completion queue.
    pub async fn register_builtins(
        extension_core: &ExtensionCore,
        path_resolver: &PathResolver,
    ) -> Result<()> {
        let workspace = path_resolver
            .agent_workspace(".")
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(BashTool::new().with_workspace(workspace.clone())),
            Arc::new(ReadTool::new().with_workspace(workspace.clone())),
            Arc::new(WriteTool::new().with_workspace(workspace.clone())),
            Arc::new(GlobTool::new().with_workspace(workspace.clone())),
            Arc::new(GrepTool::new().with_workspace(workspace.clone())),
            Arc::new(EditTool::new().with_workspace(workspace.clone())),
            Arc::new(CronCreateTool::new()),
            Arc::new(CronDeleteTool::new()),
            Arc::new(CronListTool::new()),
        ];

        // Built-in tools are visible to every principal and registered exactly
        // once per process under PrincipalId::system(). The `register_builtins`
        // call shape is the daemon-init path.
        for tool in &tools {
            if let Err(e) =
                BuiltinToolAdapter::register_tool_system(extension_core, tool.clone()).await
            {
                tracing::warn!(
                    "Failed to register built-in tool '{}' with ExtensionCore: {}",
                    tool.name(),
                    e
                );
            } else {
                tracing::debug!(
                    "Registered built-in tool '{}' with ExtensionCore",
                    tool.name()
                );
            }
        }

        info!("Registered {} built-in tools with ToolRuntime", tools.len());
        Ok(())
    }

    /// Get a reference to the underlying `ExtensionCore`
    #[must_use]
    pub fn extension_core(&self) -> &Arc<ExtensionCore> {
        &self.extension_core
    }

    /// Get the path resolver
    #[must_use]
    pub fn path_resolver(&self) -> &PathResolver {
        &self.path_resolver
    }

    /// Execute a tool by name with the given parameters.
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool to execute
    /// * `params` - JSON parameters for the tool
    /// * `capabilities` - Optional per-call capability grants. When `None`,
    ///   the execution gate is fail-closed.
    /// * `active_extensions` - Optional active extension IDs for the current
    ///   Principal; when present, the tool's owning extension must be active.
    ///
    /// # Returns
    /// The JSON result of the tool execution
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        params: serde_json::Value,
        capabilities: Option<Vec<String>>,
        active_extensions: Option<Vec<String>>,
    ) -> Result<serde_json::Value> {
        self.execute_tool_with_workspace(
            tool_name,
            params,
            &self.workspace,
            capabilities,
            active_extensions,
        )
        .await
    }

    /// Execute a tool with an explicit workspace override
    pub async fn execute_tool_with_workspace(
        &self,
        tool_name: &str,
        params: serde_json::Value,
        workspace: &std::path::Path,
        capabilities: Option<Vec<String>>,
        active_extensions: Option<Vec<String>>,
    ) -> Result<serde_json::Value> {
        let (display, json, success) = crate::engine::funnel::execute_tool_via_core_with_context(
            &*self.extension_core,
            tool_name,
            params,
            Some(workspace.to_string_lossy().to_string()),
            None,
            None,
            None,
            None,
            None,
            capabilities,
            active_extensions,
            None,
        )
        .await?;

        if !success {
            return Err(anyhow::anyhow!(display));
        }

        // For backward compatibility: if the result is a simple string, wrap it
        if let Some(s) = json.as_str() {
            if s == display {
                return Ok(serde_json::json!({"result": s}));
            }
        }

        Ok(json)
    }

    /// List all registered tools visible to the system scope
    /// (built-ins, universal, MCP). The daemon has a single shared
    /// `ExtensionCore` and `ToolRuntime` is process-scoped, so
    /// `PrincipalId::system()` is the right scope here.
    #[must_use]
    pub async fn list_tools(&self) -> Vec<crate::extensions::framework::types::ToolMetadata> {
        self.extension_core
            .list_tools(peko_subject::PrincipalId::system())
            .await
    }

    /// Check if a tool is registered under the system scope.
    #[must_use]
    pub async fn has_tool(&self, tool_name: &str) -> bool {
        self.extension_core
            .get_tool_metadata(tool_name, peko_subject::PrincipalId::system())
            .await
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use serde_json::json;

    #[tokio::test]
    async fn test_tool_runtime_creation() {
        let resolver = PathResolver::new();
        let runtime = ToolRuntime::new(resolver).await;
        assert!(runtime.is_ok());
    }

    #[tokio::test]
    async fn test_tool_runtime_has_builtin_tools() {
        let resolver = PathResolver::new();
        let runtime = ToolRuntime::new(resolver).await.unwrap();

        assert!(runtime.has_tool("Bash").await);
        assert!(runtime.has_tool("Read").await);
        assert!(runtime.has_tool("Write").await);
        assert!(runtime.has_tool("Glob").await);
        assert!(runtime.has_tool("Grep").await);
        assert!(runtime.has_tool("Edit").await);
        assert!(runtime.has_tool("CronCreate").await);
        assert!(runtime.has_tool("CronDelete").await);
        assert!(runtime.has_tool("CronList").await);
    }

    #[tokio::test]
    async fn test_tool_runtime_lists_tools() {
        let resolver = PathResolver::new();
        let runtime = ToolRuntime::new(resolver).await.unwrap();
        let tools = runtime.list_tools().await;

        let tool_names: Vec<String> = tools.into_iter().map(|t| t.name).collect();
        assert!(tool_names.contains(&"Bash".to_string()));
        assert!(tool_names.contains(&"Read".to_string()));
    }

    #[tokio::test]
    async fn test_tool_runtime_execute_shell() {
        let resolver = PathResolver::new();
        let runtime = ToolRuntime::new(resolver).await.unwrap();

        let result = runtime
            .execute_tool(
                "Bash",
                json!({"command": "echo hello"}),
                Some(vec!["tool:Bash".to_string()]),
                None,
            )
            .await;

        assert!(
            result.is_ok(),
            "Expected shell execution to succeed: {:?}",
            result
        );
        let output = result.unwrap();
        assert!(output.get("stdout").is_some() || output.get("result").is_some());
    }

    #[tokio::test]
    async fn test_tool_runtime_execute_unknown_tool() {
        let resolver = PathResolver::new();
        let runtime = ToolRuntime::new(resolver).await.unwrap();

        let result = runtime
            .execute_tool("nonexistent_tool", json!({}), None, None)
            .await;

        assert!(result.is_err());
    }
}
