//! ToolRuntime - Standalone tool execution environment
//!
//! Phase 9b.N.2 trimmed this file: the F37 `execute_tool_via_core` and
//! `execute_tool_via_core_with_context` helpers have no `BashTool`
//! coupling, so they lifted cleanly into [`peko_engine::funnel`]
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
use crate::tools::runtime::ToolingRuntime;
use anyhow::Result;
use peko_channel::{ChannelPort, NoopChannelPort};
use std::path::PathBuf;
use std::sync::Arc;

/// Standalone tool execution environment
///
/// `ToolRuntime` provides a lightweight context for registering and
/// executing built-in tools without requiring a full `Agent` instance.
/// It is used by:
/// - `Agent` (delegated from `init_builtins_async`)
/// - The daemon (for async task execution)
/// - Future job runners (cron, webhooks, etc.)
///
/// `Debug` is intentionally not derived: the runtime holds trait-object
/// state that doesn't implement `Debug`. `Clone` is enough — runtime
/// owners rarely need formatted debug output, and the field-by-field
/// accessors below cover the diagnostic surfaces.
#[derive(Clone)]
pub struct ToolRuntime {
    tooling: Arc<ToolingRuntime>,
    path_resolver: PathResolver,
    /// Default workspace for the test-only principal-less shims.
    #[cfg_attr(not(test), allow(dead_code))]
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

    /// Create with a specific workspace. Channel port defaults to
    /// [`NoopChannelPort`] (calls into `ChannelRead` from this runtime
    /// will surface `Adapter` errors). Production callers should use
    /// [`Self::with_workspace_and_core`] (which takes the port) or
    /// [`Self::with_workspace_channel_port`].
    pub async fn with_workspace(
        path_resolver: PathResolver,
        workspace: impl Into<PathBuf>,
    ) -> Result<Self> {
        let workspace = workspace.into();
        let channel_port: Arc<dyn ChannelPort> = Arc::new(NoopChannelPort);
        Self::with_workspace_channel_port(path_resolver, workspace, channel_port).await
    }

    /// Create with a specific workspace and an existing tooling runtime.
    /// Channel port defaults to [`NoopChannelPort`]; production daemon
    /// code uses [`Self::with_workspace_and_core_and_channel_port`]
    /// (the three-arg shape) to wire the real adapter in.
    ///
    /// Used by the daemon to register tools with the shared
    /// `ToolingRuntime` so that agents created later can find them.
    pub async fn with_workspace_and_core(
        path_resolver: PathResolver,
        workspace: impl Into<PathBuf>,
        tooling: Arc<ToolingRuntime>,
    ) -> Result<Self> {
        let channel_port: Arc<dyn ChannelPort> = Arc::new(NoopChannelPort);
        Self::with_workspace_and_core_and_channel_port(
            path_resolver,
            workspace,
            tooling,
            channel_port,
        )
        .await
    }

    /// Three-arg variant that wires a real `ChannelPort` adapter in
    /// so `ChannelRead` works through this runtime. Production daemon
    /// start path uses this (`daemon/state.rs`).
    ///
    /// # Errors
    /// Returns an error if built-in tool registration fails.
    pub async fn with_workspace_and_core_and_channel_port(
        path_resolver: PathResolver,
        workspace: impl Into<PathBuf>,
        tooling: Arc<ToolingRuntime>,
        channel_port: Arc<dyn ChannelPort>,
    ) -> Result<Self> {
        let workspace = workspace.into();
        Self::register_builtins(tooling.catalog(), &path_resolver, channel_port).await?;

        Ok(Self {
            tooling,
            path_resolver,
            workspace,
        })
    }

    /// Two-arg variant of [`Self::with_workspace`] with a real port.
    ///
    /// # Errors
    /// Returns an error if built-in tool registration fails.
    pub async fn with_workspace_channel_port(
        path_resolver: PathResolver,
        workspace: impl Into<PathBuf>,
        channel_port: Arc<dyn ChannelPort>,
    ) -> Result<Self> {
        let workspace = workspace.into();
        let tooling = ToolingRuntime::standalone();
        Self::register_builtins(tooling.catalog(), &path_resolver, channel_port).await?;

        Ok(Self {
            tooling,
            path_resolver,
            workspace,
        })
    }

    /// Install runtime defaults through the central built-in composition layer.
    /// Missing tools are filled without replacing existing configured instances;
    /// daemon, principal, and run phases supply their own dependency bindings.
    pub async fn register_builtins(
        catalog: &crate::tools::catalog::ToolCatalog,
        path_resolver: &PathResolver,
        channel_port: Arc<dyn ChannelPort>,
    ) -> Result<()> {
        crate::tools::installation::install_runtime(catalog, path_resolver, channel_port).await
    }

    /// Get the shared tooling runtime.
    #[must_use]
    pub fn tooling(&self) -> &Arc<ToolingRuntime> {
        &self.tooling
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
    ///
    /// # Returns
    /// The JSON result of the tool execution
    #[cfg(test)]
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.execute_tool_with_workspace(tool_name, params, &self.workspace)
            .await
    }

    /// Execute a tool with an explicit workspace override. Test-only: it
    /// dispatches without a principal, and production calls must carry one.
    #[cfg(test)]
    pub async fn execute_tool_with_workspace(
        &self,
        tool_name: &str,
        params: serde_json::Value,
        workspace: &std::path::Path,
    ) -> Result<serde_json::Value> {
        let (display, json, success) = self
            .execute_tool_full_with_workspace(tool_name, params, workspace, None, None, None)
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

    /// Execute a tool with an explicit workspace override, returning the
    /// funnel's full `(display, json, success)` triplet instead of
    /// flattening failures into `Err`. ADR-061: the `ExecuteTool` IPC
    /// handler needs the triplet so capability-gate denials and tool
    /// errors surface to the caller as data (`success: false`), not as
    /// a transport error.
    ///
    /// `session_id` / `principal_id` / `principal_name` carry the
    /// **server-resolved** calling context (phase 2a/2b):
    /// principal-scoped tools (`ModelCall`,
    /// `Workflow`, cron) read the identity off the resulting
    /// `ToolContext`. On the `ExecuteTool` path `session_id` carries
    /// the packet's `session_key` (the workflow's attribution anchor —
    /// e.g. `agent:<principal>:workflow:<uuid>`), not a session UUID.
    /// All three are `None` for unattributed standalone calls.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_tool_full_with_workspace(
        &self,
        tool_name: &str,
        params: serde_json::Value,
        workspace: &std::path::Path,
        session_id: Option<String>,
        principal_id: Option<String>,
        principal_name: Option<String>,
    ) -> Result<(String, serde_json::Value, bool)> {
        peko_engine::funnel::execute_tool_via_core_with_context(
            &*self.tooling,
            tool_name,
            params,
            Some(workspace.to_string_lossy().to_string()),
            None,
            session_id,
            None,
            principal_id,
            principal_name,
            None,
        )
        .await
    }

    /// List all registered tools visible to the system scope
    /// (built-ins, MCP). The daemon has a single shared
    /// The runtime is shared across the daemon, so
    /// `PrincipalId::system()` is the right scope here.
    #[must_use]
    pub async fn list_tools(&self) -> Vec<crate::tools::metadata::ToolMetadata> {
        self.tooling
            .catalog()
            .list_tools(peko_subject::PrincipalId::system())
            .await
    }

    /// Check if a tool is registered under the system scope.
    #[must_use]
    pub async fn has_tool(&self, tool_name: &str) -> bool {
        self.tooling
            .catalog()
            .get(tool_name, peko_subject::PrincipalId::system())
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
        assert!(runtime.has_tool("Cron").await);
        assert!(runtime.has_tool("Read").await);
        assert!(runtime.has_tool("Write").await);
        assert!(runtime.has_tool("Glob").await);
        assert!(runtime.has_tool("Grep").await);
        assert!(runtime.has_tool("Edit").await);
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

    /// ADR-066 D1 (P2): presence = visibility = executability. A fresh
    /// principal carries no grants (`principal.toml` never persists a
    /// `[capabilities]` section), so this path exercises exactly what a
    /// fresh principal gets: the full wire catalog, and `Bash` runs.
    #[tokio::test]
    async fn test_no_grant_context_full_catalog_and_bash_executes() {
        let resolver = PathResolver::new();
        let runtime = ToolRuntime::new(resolver).await.unwrap();

        // The wire catalog contains the built-ins with no capability
        // set anywhere in the call path.
        let defs = runtime
            .tooling()
            .catalog()
            .tool_definitions(peko_subject::PrincipalId::system())
            .await;
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        for expected in ["Bash", "Read", "Write", "Glob", "Grep", "Edit"] {
            assert!(
                names.contains(&expected),
                "fresh principal sees {expected} in the wire catalog: {names:?}"
            );
        }

        // And execution works — the funnel is called with no grant
        // context at all (`execute_tool` carries no capabilities).
        let result = runtime
            .execute_tool("Bash", json!({"command": "echo hello"}))
            .await;
        assert!(
            result.is_ok(),
            "Bash executes without any grant context: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_tool_runtime_execute_unknown_tool() {
        let resolver = PathResolver::new();
        let runtime = ToolRuntime::new(resolver).await.unwrap();

        let result = runtime.execute_tool("nonexistent_tool", json!({})).await;

        assert!(result.is_err());
    }
}
