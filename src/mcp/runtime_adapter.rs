//! MCP Runtime Adapter
//!
//! Bridges MCP servers with the `BackgroundRuntimeManager` by implementing
//! `BackgroundRuntimeAdapter`. This adapter:
//! - Extracts stdin/stdout from a `RuntimeKind::Process` managed runtime
//! - Wraps them in an `McpClient`
//! - Performs JSON-RPC initialization and tool discovery
//! - Stores the client in a shared registry for later use by `McpManager`

use crate::daemon::background_runtime::adapter::{BackgroundRuntimeAdapter, CrashAction};
use crate::daemon::background_runtime::supervisor::ManagedRuntime;
use crate::mcp::{
    client::{ClientError, McpClient},
    config::McpServerConfig,
    transport::{StdioTransport, TransportError},
    types::Tool,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Errors that can occur in the MCP runtime adapter
#[derive(Debug, thiserror::Error)]
pub enum McpRuntimeAdapterError {
    #[error("Runtime is not a process (expected RuntimeKind::Process)")]
    NotAProcess,

    #[error("Transport error: {0}")]
    Transport(#[from] TransportError),

    #[error("Client error: {0}")]
    Client(#[from] ClientError),

    #[error("Initialization timeout")]
    InitTimeout,

    #[error("Client not found in registry for runtime: {0}")]
    ClientNotFound(String),
}

/// Result type for adapter operations
pub type Result<T> = std::result::Result<T, McpRuntimeAdapterError>;

/// Information about a running MCP server, stored in the registry.
#[derive(Clone, Debug)]
pub struct McpServerInfo {
    /// The MCP client for this server
    pub client: Arc<RwLock<McpClient>>,
    /// Discovered tools (cached after initialization)
    pub tools: Vec<Tool>,
    /// Server info string (name + version)
    pub server_info: Option<String>,
}

/// Shared registry that maps runtime IDs to `McpServerInfo`.
///
/// This allows the `McpManager` (and other callers) to access clients
/// and discovered tools after the `BackgroundRuntimeManager` has started
/// the runtime.
#[derive(Clone, Debug)]
pub struct McpClientRegistry {
    servers: Arc<RwLock<HashMap<String, McpServerInfo>>>,
}

impl McpClientRegistry {
    /// Create a new empty registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Insert server info for the given runtime ID
    pub async fn insert(&self, runtime_id: String, info: McpServerInfo) {
        let mut servers = self.servers.write().await;
        servers.insert(runtime_id, info);
    }

    /// Get server info by runtime ID
    pub async fn get(&self, runtime_id: &str) -> Option<McpServerInfo> {
        let servers = self.servers.read().await;
        servers.get(runtime_id).cloned()
    }

    /// Get just the client by runtime ID
    pub async fn get_client(&self, runtime_id: &str) -> Option<Arc<RwLock<McpClient>>> {
        let servers = self.servers.read().await;
        servers.get(runtime_id).map(|info| info.client.clone())
    }

    /// Remove a server by runtime ID
    pub async fn remove(&self, runtime_id: &str) -> Option<McpServerInfo> {
        let mut servers = self.servers.write().await;
        servers.remove(runtime_id)
    }

    /// Check if a server exists for the given runtime ID
    pub async fn contains(&self, runtime_id: &str) -> bool {
        let servers = self.servers.read().await;
        servers.contains_key(runtime_id)
    }

    /// List all registered runtime IDs
    pub async fn list(&self) -> Vec<String> {
        let servers = self.servers.read().await;
        servers.keys().cloned().collect()
    }

    /// Get all tools from all registered servers
    pub async fn list_all_tools(&self) -> Vec<(String, Tool)> {
        let servers = self.servers.read().await;
        let mut all_tools = Vec::new();
        for (runtime_id, info) in servers.iter() {
            for tool in &info.tools {
                all_tools.push((runtime_id.clone(), tool.clone()));
            }
        }
        all_tools
    }
}

impl Default for McpClientRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Adapter that implements `BackgroundRuntimeAdapter` for MCP servers.
///
/// This adapter is given to `BackgroundRuntimeManager::start()` when
/// launching an MCP server process. It handles:
/// - JSON-RPC initialization after the OS process starts
/// - Tool discovery
/// - Periodic health checks via JSON-RPC ping
/// - Graceful shutdown via JSON-RPC exit notification
#[derive(Debug, Clone)]
pub struct McpRuntimeAdapter {
    /// Server configuration (timeouts, capabilities, etc.)
    server_config: McpServerConfig,
    /// Shared registry where the initialized client is stored
    client_registry: Arc<McpClientRegistry>,
}

impl McpRuntimeAdapter {
    /// Create a new MCP runtime adapter
    ///
    /// # Arguments
    /// * `server_config` - Configuration for this MCP server
    /// * `client_registry` - Shared registry to store the client after initialization
    #[must_use]
    pub fn new(server_config: McpServerConfig, client_registry: Arc<McpClientRegistry>) -> Self {
        Self {
            server_config,
            client_registry,
        }
    }

    /// Get the server configuration
    #[must_use]
    pub fn config(&self) -> &McpServerConfig {
        &self.server_config
    }

    /// Get the client registry
    #[must_use]
    pub fn registry(&self) -> Arc<McpClientRegistry> {
        self.client_registry.clone()
    }
}

#[async_trait]
impl BackgroundRuntimeAdapter for McpRuntimeAdapter {
    fn clone_box(&self) -> Arc<dyn BackgroundRuntimeAdapter> {
        Arc::new(self.clone())
    }

    /// Called after the OS process has started.
    ///
    /// 1. Extract stdin/stdout from `RuntimeKind::Process`
    /// 2. Create `StdioTransport::from_handles`
    /// 3. Create `McpClient`, perform JSON-RPC initialization
    /// 4. Discover tools
    /// 5. Store the client in the shared registry
    async fn initialize(&self, runtime: &mut ManagedRuntime) -> anyhow::Result<()> {
        info!(
            "Initializing MCP runtime adapter for '{}'",
            runtime.id
        );

        // Extract stdin/stdout from the process
        let (stdin, stdout, pid) = match &mut runtime.kind {
            crate::daemon::background_runtime::supervisor::RuntimeKind::Process {
                stdin,
                stdout,
                pid,
                ..
            } => {
                // Take ownership of stdin/stdout from the runtime.
                // The supervisor no longer needs them directly after initialization;
                // the transport now owns them.
                let stdin = stdin.take().ok_or_else(|| {
                    anyhow::anyhow!("MCP runtime '{}': stdin already taken", runtime.id)
                })?;
                let stdout = stdout.take().ok_or_else(|| {
                    anyhow::anyhow!("MCP runtime '{}': stdout already taken", runtime.id)
                })?;
                let pid = *pid;
                (stdin, stdout, pid)
            }
            _ => {
                anyhow::bail!(
                    "McpRuntimeAdapter only supports RuntimeKind::Process, got {:?}",
                    runtime.kind
                );
            }
        };

        // Create transport from the raw handles
        let transport = StdioTransport::from_handles(stdin, stdout, pid);

        // Create and initialize the MCP client
        let mut client = McpClient::new(Box::new(transport));

        let init_timeout = Duration::from_secs(self.server_config.init_timeout_secs);
        match tokio::time::timeout(init_timeout, client.initialize()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                anyhow::bail!("MCP client initialization failed for '{}': {}", runtime.id, e);
            }
            Err(_) => {
                anyhow::bail!("MCP client initialization timed out for '{}'", runtime.id);
            }
        };

        // After initialize(), server_info is stored inside the client
        let (server_name, server_version) = match client.server_info() {
            Some(info) => (info.server_info.name.clone(), info.server_info.version.clone()),
            None => ("unknown".to_string(), "unknown".to_string()),
        };

        info!(
            "MCP server '{}' initialized: {} v{}",
            runtime.id, server_name, server_version
        );

        // Discover tools if supported
        let tools = if client.supports_capability("tools") {
            match client.list_tools().await {
                Ok(tools) => {
                    debug!(
                        "MCP server '{}' discovered {} tools",
                        runtime.id,
                        tools.len()
                    );
                    tools
                }
                Err(e) => {
                    warn!(
                        "MCP server '{}' tool discovery failed: {}",
                        runtime.id, e
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Store client and discovered tools in registry so McpManager can access them
        let server_info_str = format!("{} v{}", server_name, server_version);
        let info = McpServerInfo {
            client: Arc::new(RwLock::new(client)),
            tools,
            server_info: Some(server_info_str),
        };
        self.client_registry
            .insert(runtime.id.clone(), info)
            .await;

        info!(
            "MCP runtime adapter for '{}' initialized successfully",
            runtime.id
        );
        Ok(())
    }

    /// Periodic health check — ping the MCP server via JSON-RPC.
    async fn health_check(&self, runtime: &ManagedRuntime) -> bool {
        let Some(info) = self.client_registry.get(&runtime.id).await else {
            warn!(
                "Health check for '{}': client not found in registry",
                runtime.id
            );
            return false;
        };

        let client_guard = info.client.read().await;
        let healthy = client_guard.ping().await.is_ok();
        drop(client_guard);

        if !healthy {
            warn!("MCP server '{}' health check failed", runtime.id);
        }

        healthy
    }

    /// When the runtime crashes, always request a restart.
    ///
    /// MCP servers are expected to be long-running; a crash is usually
    /// transient (e.g. OOM, segfault) and should be retried.
    async fn on_crash(&self, _runtime: &mut ManagedRuntime) -> CrashAction {
        info!("MCP runtime crashed — requesting restart");
        CrashAction::Restart
    }

    /// Graceful shutdown — send JSON-RPC exit notification and close client.
    async fn shutdown(&self, runtime: &mut ManagedRuntime) -> anyhow::Result<()> {
        info!("Shutting down MCP runtime '{}'", runtime.id);

        if let Some(info) = self.client_registry.remove(&runtime.id).await {
            let mut client_guard = info.client.write().await;
            if let Err(e) = client_guard.shutdown().await {
                warn!(
                    "Error shutting down MCP client for '{}': {}",
                    runtime.id, e
                );
            }
        } else {
            debug!(
                "No client found in registry for '{}' during shutdown",
                runtime.id
            );
        }

        info!("MCP runtime '{}' shut down", runtime.id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_insert_get_remove() {
        use crate::mcp::transport::InMemoryTransport;

        let registry = McpClientRegistry::new();

        // Create a dummy client
        let (transport, _other) = InMemoryTransport::pair();
        let client = McpClient::new(Box::new(transport));
        let info = McpServerInfo {
            client: Arc::new(RwLock::new(client)),
            tools: Vec::new(),
            server_info: None,
        };

        // Insert
        registry.insert("test-server".to_string(), info).await;
        assert!(registry.contains("test-server").await);

        // Get client
        let got = registry.get_client("test-server").await;
        assert!(got.is_some());

        // Remove
        let removed = registry.remove("test-server").await;
        assert!(removed.is_some());
        assert!(!registry.contains("test-server").await);
    }

    #[tokio::test]
    async fn test_registry_list() {
        let registry = McpClientRegistry::new();
        let list = registry.list().await;
        assert!(list.is_empty());
    }

    #[test]
    fn test_adapter_new() {
        let registry = Arc::new(McpClientRegistry::new());
        let config = McpServerConfig::stdio("test", "echo", vec![]);
        let adapter = McpRuntimeAdapter::new(config, registry);

        assert_eq!(adapter.config().name, "test");
    }
}
