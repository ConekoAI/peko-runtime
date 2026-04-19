//! RuntimeFacade — Centralised tool runtime for the daemon
//!
//! Consolidates `ExtensionCore`, `ExtensionManager`, `ToolRuntime`, and
//! `UnifiedAsyncExecutor` into a single component that owns the full tool
//! registry (built-in + MCP + universal) and all tool execution.
//!
//! This is the implementation of ADR-021 Phase 1.

use crate::agent::async_tool_framework::{AsyncTaskReceipt, AsyncToolConfig, UnifiedAsyncExecutor};
use crate::common::paths::PathResolver;
use crate::extensions::adapters::BuiltInAdapters;
use crate::extensions::core::ExtensionCore;
use crate::extensions::manager::ExtensionManager;
use crate::extensions::types::ToolMetadata;
use crate::runtime::ToolRuntime;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Centralised runtime facade for all tool execution in the daemon.
///
/// `RuntimeFacade` owns:
/// - The full tool registry (built-in + MCP + universal)
/// - The async task executor
/// - Synchronous tool execution delegation
///
/// It does **not** own agent-specific tools (`AgentSpawnTool`, `SessionStatusTool`,
/// etc.) — those are registered per-agent by `Agent::init_builtins_async()` on top
/// of the shared `ExtensionCore`.
#[derive(Debug, Clone)]
pub struct RuntimeFacade {
    extension_core: Arc<ExtensionCore>,
    extension_manager: Arc<RwLock<ExtensionManager>>,
    tool_runtime: Arc<ToolRuntime>,
    async_task_executor: Arc<UnifiedAsyncExecutor>,
    path_resolver: PathResolver,
}

impl RuntimeFacade {
    /// Create a new `RuntimeFacade` with the given path resolver.
    ///
    /// This creates the underlying `ExtensionCore`, `ToolRuntime`,
    /// `ExtensionManager`, and `UnifiedAsyncExecutor`, but does **not**
    /// register any tools yet. Call `initialise_full_registry().await`
    /// to load all tools.
    ///
    /// **Note:** This creates a fresh `ExtensionCore`. In daemon mode, you
    /// typically want `with_global_core()` to reuse the global core that
    /// was already initialised in `main.rs` with the async transport.
    pub async fn new(path_resolver: PathResolver) -> Result<Self> {
        let extension_core = Arc::new(ExtensionCore::new());
        Self::with_core(path_resolver, extension_core).await
    }

    /// Create a `RuntimeFacade` wrapping an existing `ExtensionCore`.
    ///
    /// This is the preferred constructor in daemon mode, where the global
    /// `ExtensionCore` is already initialised in `main.rs` with the
    /// appropriate async transport (daemon uses `LocalAsyncTransport`;
    /// CLI uses `UnavailableAsyncTransport` which fails fast when daemon
    /// is unreachable).
    pub async fn with_core(
        path_resolver: PathResolver,
        extension_core: Arc<ExtensionCore>,
    ) -> Result<Self> {
        let tool_runtime = Arc::new(
            ToolRuntime::with_workspace(
                path_resolver.clone(),
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .await?,
        );
        let extension_manager = Arc::new(RwLock::new(ExtensionManager::with_core(
            extension_core.clone(),
        )));
        let async_task_executor = Arc::new(UnifiedAsyncExecutor::new());

        Ok(Self {
            extension_core,
            extension_manager,
            tool_runtime,
            async_task_executor,
            path_resolver,
        })
    }

    /// Create with custom extension services (e.g. for testing).
    pub async fn with_services(
        path_resolver: PathResolver,
        extension_core: Arc<ExtensionCore>,
        tool_runtime: Arc<ToolRuntime>,
        async_task_executor: Arc<UnifiedAsyncExecutor>,
    ) -> Self {
        let extension_manager = Arc::new(RwLock::new(ExtensionManager::with_core(
            extension_core.clone(),
        )));

        Self {
            extension_core,
            extension_manager,
            tool_runtime,
            async_task_executor,
            path_resolver,
        }
    }

    /// Initialise the full tool registry: built-in + universal + MCP.
    ///
    /// This is the single initialisation path that replaces the three
    /// divergent paths identified in ADR-021.
    pub async fn initialise_full_registry(&self) -> Result<()> {
        info!("Initialising full tool registry in RuntimeFacade...");

        // 1. Register built-in tools via ToolRuntime
        ToolRuntime::register_builtins(&self.extension_core, &self.path_resolver).await?;
        info!("Built-in tools registered");

        // 2. Register adapters for universal tools and MCP
        {
            let mut manager = self.extension_manager.write().await;
            for adapter in BuiltInAdapters::new().adapters() {
                manager.register_adapter(adapter);
            }
        }

        // 3. Load universal tools from extensions directory
        let extensions_dir = self.path_resolver.data_dir().join("extensions");
        if extensions_dir.exists() {
            info!("Loading extensions from: {}", extensions_dir.display());
            let mut manager = self.extension_manager.write().await;
            match manager.load_from_directory(&extensions_dir).await {
                Ok(loaded_ids) => {
                    if loaded_ids.is_empty() {
                        info!("No extensions found in {}", extensions_dir.display());
                    } else {
                        info!("Loaded {} extensions: {:?}", loaded_ids.len(), loaded_ids);
                    }
                }
                Err(e) => {
                    warn!("Failed to load extensions from {}: {}", extensions_dir.display(), e);
                }
            }
        } else {
            info!(
                "Extensions directory not found at {} — skipping universal/MCP tool loading",
                extensions_dir.display()
            );
        }

        // 4. Also scan discovery paths for system-wide extensions
        {
            let mut manager = self.extension_manager.write().await;
            match manager.load_all().await {
                Ok(report) => {
                    info!(
                        "Discovery-path scan complete: {} loaded, {} failed",
                        report.loaded.len(),
                        report.failed.len()
                    );
                    for (path, err) in report.failed {
                        warn!("Failed to load extension from {:?}: {}", path, err);
                    }
                }
                Err(e) => {
                    warn!("Failed to scan discovery paths: {}", e);
                }
            }
        }

        let tool_count = self.extension_core.tool_count().await;
        info!("Full tool registry initialised with {} tools", tool_count);

        Ok(())
    }

    /// Execute a tool synchronously.
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.tool_runtime.execute_tool(tool_name, params).await
    }

    /// Execute a tool synchronously with an explicit workspace.
    pub async fn execute_tool_with_workspace(
        &self,
        tool_name: &str,
        params: serde_json::Value,
        workspace: &Path,
    ) -> Result<serde_json::Value> {
        self.tool_runtime
            .execute_tool_with_workspace(tool_name, params, workspace)
            .await
    }

    /// Execute a tool asynchronously.
    pub async fn execute_tool_async(
        &self,
        task_id: String,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: std::path::PathBuf,
        config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        let tool_runtime = Arc::clone(&self.tool_runtime);
        let tool_name_clone = tool_name.clone();
        let params_clone = params.clone();

        let execution_fn: Box<
            dyn FnOnce()
                -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<crate::agent::async_tool_framework::AsyncTaskResult>> + Send>,
                > + Send,
        > = Box::new(move || {
            let tool_runtime = Arc::clone(&tool_runtime);
            Box::pin(async move {
                tool_runtime
                    .execute_tool_with_workspace(&tool_name_clone, params_clone, &workspace)
                    .await
                    .map(|result| crate::agent::async_tool_framework::AsyncTaskResult::Generic { data: result })
            })
        });

        self.async_task_executor
            .execute_boxed(task_id, tool_name, params, session_key, config, execution_fn)
            .await
    }

    /// List all registered tools.
    pub async fn list_tools(&self) -> Vec<ToolMetadata> {
        self.tool_runtime.list_tools().await
    }

    /// Check if a tool is registered.
    pub async fn has_tool(&self, tool_name: &str) -> bool {
        self.tool_runtime.has_tool(tool_name).await
    }

    /// Get the underlying `ExtensionCore`.
    #[must_use]
    pub fn extension_core(&self) -> &Arc<ExtensionCore> {
        &self.extension_core
    }

    /// Get the underlying `ToolRuntime`.
    #[must_use]
    pub fn tool_runtime(&self) -> &Arc<ToolRuntime> {
        &self.tool_runtime
    }

    /// Get the async task executor.
    #[must_use]
    pub fn async_task_executor(&self) -> &Arc<UnifiedAsyncExecutor> {
        &self.async_task_executor
    }

    /// Get the extension manager.
    #[must_use]
    pub fn extension_manager(&self) -> &Arc<RwLock<ExtensionManager>> {
        &self.extension_manager
    }

    /// Get the global MCP manager singleton.
    ///
    /// This is the same manager shared across all `McpAdapter` instances.
    #[must_use]
    pub fn mcp_manager(&self) -> Arc<RwLock<crate::mcp::McpManager>> {
        crate::extensions::adapters::mcp_adapter::get_global_mcp_manager()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;

    #[tokio::test]
    async fn test_runtime_facade_creation() {
        let resolver = PathResolver::new();
        let facade = RuntimeFacade::new(resolver).await;
        assert!(facade.is_ok());
    }

    #[tokio::test]
    async fn test_runtime_facade_has_builtin_tools_after_init() {
        let resolver = PathResolver::new();
        let facade = RuntimeFacade::new(resolver).await.unwrap();
        facade.initialise_full_registry().await.unwrap();

        assert!(facade.has_tool("shell").await);
        assert!(facade.has_tool("read_file").await);
        assert!(facade.has_tool("write_file").await);
        assert!(facade.has_tool("glob").await);
        assert!(facade.has_tool("grep").await);
        assert!(facade.has_tool("str_replace_file").await);
    }

    #[tokio::test]
    async fn test_runtime_facade_execute_sync() {
        let resolver = PathResolver::new();
        let facade = RuntimeFacade::new(resolver).await.unwrap();
        facade.initialise_full_registry().await.unwrap();

        let result = facade
            .execute_tool("shell", serde_json::json!({"command": "echo hello"}))
            .await;

        assert!(result.is_ok(), "Expected shell execution to succeed: {:?}", result);
    }
}
