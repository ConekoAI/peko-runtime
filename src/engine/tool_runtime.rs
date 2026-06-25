//! ToolRuntime - Standalone tool execution environment
//!
//! Extracted from `Agent::init_builtins_async()` to allow the daemon
//! (and other non-agent contexts) to resolve and execute built-in tools.

use crate::common::paths::PathResolver;
use crate::extensions::builtin::BuiltinToolAdapter;
use crate::extensions::framework::core::{ExtensionCore, ExtensionServices};
use crate::extensions::framework::types::{tool_result_from_hook, HookInput};
use crate::extensions::framework::HookPoint;
use crate::tools::{BashTool, CronTool, EditTool, GlobTool, GrepTool, ReadTool, Tool, WriteTool};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// Canonical tool execution via ExtensionCore.
///
/// All production code should call this (or `ToolRuntime::execute_tool`) to ensure
/// consistent behavior: workspace injection, reserved params, permission checks,
/// abort/timeout handling, progress reporting, and metrics.
///
/// Returns a triplet of `(display_string, json_value, success)`.
pub async fn execute_tool_via_core(
    core: &ExtensionCore,
    tool_name: &str,
    params: serde_json::Value,
    workspace: Option<String>,
) -> Result<(String, serde_json::Value, bool)> {
    execute_tool_via_core_with_context(core, tool_name, params, workspace, None, None, None).await
}

/// Execute a tool via ExtensionCore with agent, session, and caller context.
///
/// This variant passes agent_id, session_id (for reserved parameter injection)
/// and caller_id (for per-user permission checks and audit logging — issue #17).
pub async fn execute_tool_via_core_with_context(
    core: &ExtensionCore,
    tool_name: &str,
    params: serde_json::Value,
    workspace: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    caller_id: Option<String>,
) -> Result<(String, serde_json::Value, bool)> {
    let point = HookPoint::ToolExecute {
        tool_name: tool_name.to_string(),
    };
    let input = HookInput::ToolCall {
        tool_name: tool_name.to_string(),
        params,
        workspace,
        agent_id,
        session_id,
        caller_id,
    };

    let result = core.invoke_hook(point, input).await;
    Ok(tool_result_from_hook(result, tool_name))
}

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
    /// The `task` tool is **NOT** registered here. It depends on
    /// per-agent state (AsyncExecutor + ExtensionCore for spawn-side
    /// lookups). Each agent registers its own `TaskTool` via
    /// `BuiltinToolAdapter::register_task_tool` once the agent has
    /// constructed its executor and completion queue.
    pub async fn register_builtins(
        extension_core: &ExtensionCore,
        path_resolver: &PathResolver,
    ) -> Result<()> {
        let workspace = path_resolver
            .agent_workspace(".", None)
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
            Arc::new(CronTool::new()),
        ];

        // Enable all built-in tools by default in the daemon context.
        // Use canonical extension IDs so the whitelist matches the owners stored in ToolRegistry.
        let ext_ids: Vec<String> = tools
            .iter()
            .map(|t| format!("builtin:tool:{}", t.name()))
            .collect();
        let ext_config = crate::common::types::agent_legacy::ExtensionConfig {
            enabled: ext_ids,
            http: None,
            custom: None,
            read_file: None,
            write_file: None,
            glob: None,
            grep: None,
            edit_tool: None,
        };
        extension_core.set_tool_config(ext_config).await;

        for tool in &tools {
            if let Err(e) = BuiltinToolAdapter::register_tool(extension_core, tool.clone()).await {
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

    /// Execute a tool by name with the given parameters
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool to execute
    /// * `params` - JSON parameters for the tool
    ///
    /// # Returns
    /// The JSON result of the tool execution
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.execute_tool_with_workspace(tool_name, params, &self.workspace)
            .await
    }

    /// Execute a tool with an explicit workspace override
    pub async fn execute_tool_with_workspace(
        &self,
        tool_name: &str,
        params: serde_json::Value,
        workspace: &std::path::Path,
    ) -> Result<serde_json::Value> {
        let (display, json, success) = execute_tool_via_core(
            &self.extension_core,
            tool_name,
            params,
            Some(workspace.to_string_lossy().to_string()),
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

    /// List all registered tools
    #[must_use]
    pub async fn list_tools(&self) -> Vec<crate::extensions::framework::types::ToolMetadata> {
        self.extension_core.list_tools().await
    }

    /// Check if a tool is registered
    #[must_use]
    pub async fn has_tool(&self, tool_name: &str) -> bool {
        self.extension_core
            .get_tool_metadata(tool_name)
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
        assert!(runtime.has_tool("glob").await);
        assert!(runtime.has_tool("grep").await);
        assert!(runtime.has_tool("Edit").await);
        assert!(runtime.has_tool("cron").await);
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
            .execute_tool("Bash", json!({"command": "echo hello"}))
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

        let result = runtime.execute_tool("nonexistent_tool", json!({})).await;

        assert!(result.is_err());
    }
}
