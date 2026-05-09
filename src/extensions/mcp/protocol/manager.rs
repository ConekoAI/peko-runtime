//! MCP Manager
//!
//! Manages the lifecycle of MCP servers including:
//! - Starting and stopping servers
//! - Health monitoring and automatic reconnection
//! - Tool discovery and aggregation
//! - Configuration management
//!
//! # Architecture (ADR-025 Phase 2)
//!
//! This manager is now a thin wrapper over `BackgroundRuntimeManager`:
//! - **Process supervision** (spawn, health-check, restart, crash recovery) is delegated
//!   to `BackgroundRuntimeManager` via `McpRuntimeAdapter`.
//! - **MCP-specific behaviour** (JSON-RPC init, tool discovery, client access) lives in
//!   `McpRuntimeAdapter` and the shared `McpClientRegistry`.
//! - **SSE transports** are still handled directly here because they are external
//!   connections, not supervised child processes.

use crate::daemon::background_runtime::{BackgroundRuntimeManager, RuntimeState};
use crate::extensions::mcp::protocol::{
    client::{ClientError, McpClient},
    config::{McpConfig, McpServerConfig, TransportType},
    transport::{SseTransport, StdioTransport},
    types::Tool,
};
use crate::extensions::mcp::runtime::adapter::{
    McpClientRegistry, McpRuntimeAdapter, McpServerInfo,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, trace, warn};

/// Errors that can occur in the MCP manager
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("Server not found: {0}")]
    ServerNotFound(String),

    #[error("Server already running: {0}")]
    ServerAlreadyRunning(String),

    #[error("Server not running: {0}")]
    ServerNotRunning(String),

    #[error("Client error: {0}")]
    Client(#[from] ClientError),

    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Initialization timeout")]
    InitTimeout,

    #[error("Server unhealthy after {0} attempts")]
    Unhealthy(u32),

    #[error("Runtime manager error: {0}")]
    RuntimeManager(String),
}

/// Result type for manager operations
pub type Result<T> = std::result::Result<T, ManagerError>;

/// Server runtime state
#[derive(Debug, Clone)]
pub struct ServerState {
    /// Server name
    pub name: String,
    /// Whether the server is running
    pub running: bool,
    /// Whether the server is healthy
    pub healthy: bool,
    /// Number of restart attempts
    pub restart_count: u32,
    /// Last error (if any)
    pub last_error: Option<String>,
    /// Server info from initialization
    pub server_info: Option<String>,
    /// Available tools
    pub tools: Vec<Tool>,
}

/// Internal server handle
struct ServerHandle {
    /// Server configuration
    config: McpServerConfig,
    /// MCP client (if running directly, e.g. SSE)
    client: Option<Arc<RwLock<McpClient>>>,
    /// Health check task (for SSE/direct clients)
    health_task: Option<JoinHandle<()>>,
    /// Current state
    state: ServerState,
    /// Whether this server is managed by BackgroundRuntimeManager
    managed: bool,
}

/// MCP Manager
///
/// Manages multiple MCP servers, handling their lifecycle and providing
/// aggregated access to their tools.
///
/// Internally delegates process-based servers to `BackgroundRuntimeManager`
/// via `McpRuntimeAdapter`. SSE servers are still managed directly.
///
/// # Shared vs Standalone Mode
///
/// In **standalone mode** (default, `McpManager::new`), the manager owns its
/// own `BackgroundRuntimeManager` and `McpClientRegistry`. This is used by
/// tests and legacy code paths.
///
/// In **shared mode** (`McpManager::with_shared_resources`), the manager uses
/// a daemon-wide `BackgroundRuntimeManager` and `McpClientRegistry` injected
/// from `AppState`. This is the production path (ADR-025 Phase 2+).
#[derive(Clone)]
pub struct McpManager {
    /// Server configurations
    config: Arc<RwLock<McpConfig>>,
    /// Running server handles
    servers: Arc<RwLock<HashMap<String, ServerHandle>>>,
    /// Default working directory for stdio servers
    default_cwd: Option<PathBuf>,
    /// Background runtime manager for process-based MCP servers.
    /// `None` in standalone mode (owns its own); `Some` in shared mode.
    shared_runtime_manager: Option<Arc<BackgroundRuntimeManager>>,
    /// Owned runtime manager for standalone mode.
    owned_runtime_manager: Arc<BackgroundRuntimeManager>,
    /// Shared client registry — populated by McpRuntimeAdapter.
    /// `None` in standalone mode (owns its own); `Some` in shared mode.
    shared_client_registry: Option<Arc<McpClientRegistry>>,
    /// Owned client registry for standalone mode.
    owned_client_registry: Arc<McpClientRegistry>,
}

impl McpManager {
    /// Create a new MCP manager in **standalone mode**.
    ///
    /// The manager owns its own `BackgroundRuntimeManager` and
    /// `McpClientRegistry`. Use this for tests and isolated usage.
    #[must_use]
    pub fn new(config: McpConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            servers: Arc::new(RwLock::new(HashMap::new())),
            default_cwd: None,
            shared_runtime_manager: None,
            owned_runtime_manager: Arc::new(BackgroundRuntimeManager::new()),
            shared_client_registry: None,
            owned_client_registry: Arc::new(McpClientRegistry::new()),
        }
    }

    /// Create a new MCP manager with a default working directory (standalone mode).
    pub fn with_cwd(config: McpConfig, cwd: impl Into<PathBuf>) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            servers: Arc::new(RwLock::new(HashMap::new())),
            default_cwd: Some(cwd.into()),
            shared_runtime_manager: None,
            owned_runtime_manager: Arc::new(BackgroundRuntimeManager::new()),
            shared_client_registry: None,
            owned_client_registry: Arc::new(McpClientRegistry::new()),
        }
    }

    /// Create a new MCP manager in **shared mode**.
    ///
    /// Uses the daemon-wide `BackgroundRuntimeManager` and `McpClientRegistry`
    /// so that MCP servers started by this manager are visible to
    /// `pekobot ext status` and can be controlled via `pekobot ext start/stop`.
    ///
    /// # Arguments
    /// * `config` — Initial MCP server configurations
    /// * `runtime_manager` — Shared background runtime manager from `AppState`
    /// * `client_registry` — Shared client registry from `AppState`
    #[must_use]
    pub fn with_shared_resources(
        config: McpConfig,
        runtime_manager: Arc<BackgroundRuntimeManager>,
        client_registry: Arc<McpClientRegistry>,
    ) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            servers: Arc::new(RwLock::new(HashMap::new())),
            default_cwd: None,
            shared_runtime_manager: Some(runtime_manager),
            owned_runtime_manager: Arc::new(BackgroundRuntimeManager::new()),
            shared_client_registry: Some(client_registry),
            owned_client_registry: Arc::new(McpClientRegistry::new()),
        }
    }

    /// Get the effective background runtime manager.
    ///
    /// Returns the shared one if in shared mode, otherwise the owned one.
    fn runtime_manager(&self) -> Arc<BackgroundRuntimeManager> {
        self.shared_runtime_manager
            .clone()
            .unwrap_or_else(|| self.owned_runtime_manager.clone())
    }

    /// Get the effective client registry.
    ///
    /// Returns the shared one if in shared mode, otherwise the owned one.
    fn client_registry(&self) -> Arc<McpClientRegistry> {
        self.shared_client_registry
            .clone()
            .unwrap_or_else(|| self.owned_client_registry.clone())
    }

    /// Initialize the manager and start auto-start servers
    pub async fn init(&self) -> Result<()> {
        info!("Initializing MCP manager");

        // Collect servers to auto-start
        let servers_to_start: Vec<String> = {
            let config = self.config.read().await;

            // Create handles for all configured servers
            for server_config in &config.servers {
                let handle = ServerHandle {
                    config: server_config.clone(),
                    client: None,
                    health_task: None,
                    state: ServerState {
                        name: server_config.name.clone(),
                        running: false,
                        healthy: false,
                        restart_count: 0,
                        last_error: None,
                        server_info: None,
                        tools: Vec::new(),
                    },
                    managed: server_config.transport == TransportType::Stdio,
                };

                self.servers
                    .write()
                    .await
                    .insert(server_config.name.clone(), handle);
            }

            // Collect auto-start servers (server auto_start takes precedence)
            config
                .servers
                .iter()
                .filter(|s| s.auto_start)
                .map(|s| s.name.clone())
                .collect()
        };

        // Auto-start servers (outside of config lock)
        info!("Auto-starting {} MCP servers...", servers_to_start.len());
        for name in servers_to_start {
            info!("Starting MCP server '{}'...", name);
            if let Err(e) = self.start_server(&name).await {
                warn!("Failed to auto-start server '{}': {}", name, e);
            } else {
                info!("MCP server '{}' started successfully", name);
            }
        }

        // Log final status
        let final_servers = self.servers.read().await;
        for (name, handle) in final_servers.iter() {
            info!(
                "MCP server '{}' status: running={}, healthy={}, tools={}",
                name,
                handle.state.running,
                handle.state.healthy,
                handle.state.tools.len()
            );
        }

        info!(
            "MCP manager initialized with {} servers",
            final_servers.len()
        );
        Ok(())
    }

    /// Start a specific server
    pub async fn start_server(&self, name: &str) -> Result<()> {
        let mut servers = self.servers.write().await;

        let handle = servers
            .get_mut(name)
            .ok_or_else(|| ManagerError::ServerNotFound(name.to_string()))?;

        if handle.state.running {
            return Err(ManagerError::ServerAlreadyRunning(name.to_string()));
        }

        info!("Starting MCP server: {}", name);

        match handle.config.transport {
            TransportType::Stdio => {
                // Delegate to BackgroundRuntimeManager via McpRuntimeAdapter
                self.start_managed_server(name, &handle.config).await?;
                handle.state.running = true;
                handle.state.healthy = true;
                handle.state.last_error = None;
                handle.managed = true;
            }
            TransportType::Sse => {
                // SSE servers are still handled directly (external connection)
                let mut client = self.start_sse_client(&handle.config).await?;

                let init_timeout = Duration::from_secs(handle.config.init_timeout_secs);
                let server_info =
                    match tokio::time::timeout(init_timeout, client.initialize()).await {
                        Ok(Ok(info)) => info,
                        Ok(Err(e)) => return Err(ManagerError::Client(e)),
                        Err(_) => return Err(ManagerError::InitTimeout),
                    };

                let server_info_str = format!(
                    "{} v{}",
                    server_info.server_info.name, server_info.server_info.version
                );

                let tools = if client.supports_capability("tools") {
                    match client.list_tools().await {
                        Ok(tools) => tools,
                        Err(_e) => Vec::new(),
                    }
                } else {
                    Vec::new()
                };

                let client_arc = Arc::new(RwLock::new(client));
                handle.client = Some(client_arc.clone());
                handle.state.running = true;
                handle.state.healthy = true;
                handle.state.server_info = Some(server_info_str);
                handle.state.tools = tools;
                handle.state.last_error = None;
                handle.managed = false;

                // Start health check task for SSE
                let health_task = self.start_health_check(name, client_arc);
                handle.health_task = Some(health_task);
            }
        }

        info!("MCP server '{}' started successfully", name);
        Ok(())
    }

    /// Stop a specific server
    pub async fn stop_server(&self, name: &str) -> Result<()> {
        let mut servers = self.servers.write().await;

        let handle = servers
            .get_mut(name)
            .ok_or_else(|| ManagerError::ServerNotFound(name.to_string()))?;

        if !handle.state.running {
            return Err(ManagerError::ServerNotRunning(name.to_string()));
        }

        info!("Stopping MCP server: {}", name);

        if handle.managed {
            // Stop via BackgroundRuntimeManager
            drop(servers); // release lock before async call
            if let Err(e) = self.runtime_manager().stop(name).await {
                warn!("Error stopping managed runtime '{}': {}", name, e);
            }
            // Re-acquire lock to update state
            let mut servers = self.servers.write().await;
            if let Some(handle) = servers.get_mut(name) {
                handle.state.running = false;
                handle.state.healthy = false;
            }
        } else {
            // Stop health check task
            if let Some(task) = handle.health_task.take() {
                task.abort();
            }

            // Shutdown client
            if let Some(client) = handle.client.take() {
                let mut client = client.write().await;
                if let Err(e) = client.shutdown().await {
                    warn!("Error shutting down client for '{}': {}", name, e);
                }
            }

            handle.state.running = false;
            handle.state.healthy = false;
        }

        info!("MCP server '{}' stopped", name);
        Ok(())
    }

    /// Restart a server
    pub async fn restart_server(&self, name: &str) -> Result<()> {
        info!("Restarting MCP server: {}", name);

        // Stop if running
        match self.stop_server(name).await {
            Ok(()) | Err(ManagerError::ServerNotRunning(_)) => {}
            Err(e) => return Err(e),
        }

        // Wait a bit
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Start again
        self.start_server(name).await
    }

    /// Get client for a specific server
    pub async fn get_client(&self, name: &str) -> Result<Arc<RwLock<McpClient>>> {
        let servers = self.servers.read().await;

        let handle = servers
            .get(name)
            .ok_or_else(|| ManagerError::ServerNotFound(name.to_string()))?;

        if !handle.state.running {
            return Err(ManagerError::ServerNotRunning(name.to_string()));
        }

        if handle.managed {
            // Client lives in the shared registry
            drop(servers);
            self.client_registry()
                .get_client(name)
                .await
                .ok_or_else(|| ManagerError::ServerNotRunning(name.to_string()))
        } else {
            // Client lives directly in the handle
            handle
                .client
                .clone()
                .ok_or_else(|| ManagerError::ServerNotRunning(name.to_string()))
        }
    }

    /// Get server state
    pub async fn get_server_state(&self, name: &str) -> Result<ServerState> {
        let mut servers = self.servers.write().await;

        let handle = servers
            .get_mut(name)
            .ok_or_else(|| ManagerError::ServerNotFound(name.to_string()))?;

        // For managed servers, sync state from BackgroundRuntimeManager
        if handle.managed && handle.state.running {
            if let Some(runtime_state) = self.runtime_manager().get_state(name).await {
                match runtime_state {
                    RuntimeState::Healthy => {
                        handle.state.healthy = true;
                    }
                    RuntimeState::Running => {
                        handle.state.healthy = true;
                    }
                    RuntimeState::Unhealthy | RuntimeState::Crashed => {
                        handle.state.healthy = false;
                    }
                    _ => {}
                }
            }

            // Sync tools and server_info from registry if not already populated
            if handle.state.tools.is_empty() || handle.state.server_info.is_none() {
                if let Some(info) = self.client_registry().get(name).await {
                    if handle.state.tools.is_empty() {
                        handle.state.tools = info.tools;
                    }
                    if handle.state.server_info.is_none() {
                        handle.state.server_info = info.server_info;
                    }
                }
            }
        }

        Ok(handle.state.clone())
    }

    /// List all servers and their states
    pub async fn list_servers(&self) -> Vec<ServerState> {
        let servers = self.servers.read().await;
        servers.values().map(|h| h.state.clone()).collect()
    }

    /// List all tools from all running servers
    pub async fn list_all_tools(&self) -> Vec<(String, Tool)> {
        let mut all_tools = Vec::new();

        // Tools from managed servers (via registry)
        let managed_tools = self.client_registry().list_all_tools().await;
        all_tools.extend(managed_tools);

        // Tools from directly-managed servers (SSE)
        let servers = self.servers.read().await;
        for (name, handle) in servers.iter() {
            if !handle.managed && handle.state.running && handle.state.healthy {
                for tool in &handle.state.tools {
                    all_tools.push((name.clone(), tool.clone()));
                }
            }
        }

        all_tools
    }

    /// Get all tools as Pekobot Tool trait objects
    ///
    /// This allows MCP tools to be used seamlessly with Pekobot's agent system.
    /// The tools are wrapped in `McpToolProxy` or `InjectableMcpToolProxy` (if the
    /// server has `reserved_parameters` configured) which implement the Tool trait.
    ///
    /// # Returns
    /// A vector of `Arc<dyn Tool>` containing all MCP tools from running servers
    pub async fn get_tools(&self) -> Vec<Arc<dyn crate::tools::Tool>> {
        use crate::extensions::mcp::runtime::{
            injectable_proxy::InjectableMcpToolProxy, tool_proxy::McpToolProxy,
        };

        let servers = self.servers.read().await;
        let manager_arc = Arc::new(RwLock::new(self.clone()));
        let mut tools: Vec<Arc<dyn crate::tools::Tool>> = Vec::new();

        for (server_name, handle) in servers.iter() {
            if !handle.state.running || !handle.state.healthy {
                continue;
            }

            // Check if this server has reserved parameters configured
            let has_reserved = !handle.config.reserved_parameters.is_empty();

            let server_tools = if handle.managed {
                // Get tools from registry for managed servers
                if let Some(info) = self.client_registry().get(server_name).await {
                    info.tools
                } else {
                    Vec::new()
                }
            } else {
                handle.state.tools.clone()
            };

            for tool in server_tools {
                let proxy: Arc<dyn crate::tools::Tool> = if has_reserved {
                    // Use InjectableMcpToolProxy to inject reserved params
                    Arc::new(InjectableMcpToolProxy::new(
                        server_name.clone(),
                        tool.clone(),
                        manager_arc.clone(),
                        handle.config.reserved_parameters.clone(),
                    ))
                } else {
                    // Use standard McpToolProxy (no injection needed)
                    Arc::new(McpToolProxy::new(
                        server_name.clone(),
                        tool.clone(),
                        manager_arc.clone(),
                    ))
                };
                tools.push(proxy);
            }
        }

        tools
    }

    /// Call a tool on a specific server
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<crate::extensions::mcp::protocol::types::CallToolResult> {
        let client = self.get_client(server_name).await?;
        let client = client.read().await;

        let timeout = Duration::from_secs(
            self.get_server_config(server_name)
                .await
                .map_or(60, |c| c.tool_timeout_secs),
        );

        match tokio::time::timeout(timeout, client.call_tool(tool_name, arguments)).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(ManagerError::Client(e)),
            Err(_) => Err(ManagerError::Transport("Tool call timeout".to_string())),
        }
    }

    /// Shutdown all servers and the manager
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down MCP manager");

        let names: Vec<String> = {
            let servers = self.servers.read().await;
            servers.keys().cloned().collect()
        };

        for name in names {
            if let Err(e) = self.stop_server(&name).await {
                warn!("Error stopping server '{}': {}", name, e);
            }
        }

        info!("MCP manager shut down");
        Ok(())
    }

    // =========================================================================
    // Private methods
    // =========================================================================

    /// Start a managed (stdio) server via BackgroundRuntimeManager
    async fn start_managed_server(&self, name: &str, config: &McpServerConfig) -> Result<()> {
        use crate::common::process::{ProcessSpawnConfig, RestartPolicy, RuntimeSpawnConfig};

        let command = config
            .command
            .as_ref()
            .ok_or_else(|| ManagerError::Config("Missing command".to_string()))?;

        let cwd = config.cwd.clone().or_else(|| self.default_cwd.clone());

        let process_config = ProcessSpawnConfig::new(command)
            .args(config.args.clone())
            .cwd(cwd.unwrap_or_else(|| PathBuf::from(".")));

        let spawn_config = RuntimeSpawnConfig::Process(process_config);

        let adapter = Arc::new(McpRuntimeAdapter::new(
            config.clone(),
            self.client_registry(),
        ));

        let restart_policy = RestartPolicy {
            max_restarts: if config.max_restarts == 0 {
                u32::MAX
            } else {
                config.max_restarts
            },
            ..Default::default()
        };

        self.runtime_manager()
            .start(name.to_string(), spawn_config, adapter, restart_policy)
            .await
            .map_err(|e| ManagerError::RuntimeManager(e.to_string()))?;

        Ok(())
    }

    /// Start an SSE transport client
    async fn start_sse_client(&self, config: &McpServerConfig) -> Result<McpClient> {
        let endpoint = config
            .endpoint
            .as_ref()
            .ok_or_else(|| ManagerError::Config("Missing endpoint".to_string()))?;

        let transport = SseTransport::connect(endpoint)
            .await
            .map_err(|e| ManagerError::Transport(e.to_string()))?;

        Ok(McpClient::new(Box::new(transport)))
    }

    /// Get server configuration
    pub async fn get_server_config(&self, name: &str) -> Option<McpServerConfig> {
        let servers = self.servers.read().await;
        servers.get(name).map(|h| h.config.clone())
    }

    /// Add a server configuration dynamically
    ///
    /// This allows adding MCP servers at runtime without reloading the entire config.
    /// Returns true if the server was added, false if it already exists.
    pub async fn add_server_config(&self, config: McpServerConfig) -> Result<bool> {
        let mut servers = self.servers.write().await;

        if servers.contains_key(&config.name) {
            return Ok(false); // Server already exists
        }

        let handle = ServerHandle {
            config: config.clone(),
            client: None,
            health_task: None,
            state: ServerState {
                name: config.name.clone(),
                running: false,
                healthy: false,
                restart_count: 0,
                last_error: None,
                server_info: None,
                tools: Vec::new(),
            },
            managed: config.transport == TransportType::Stdio,
        };

        servers.insert(config.name.clone(), handle);
        info!(server_name = %config.name, "Added MCP server configuration");
        Ok(true)
    }

    /// Start health check task for a server (used for SSE/direct clients)
    fn start_health_check(&self, name: &str, client: Arc<RwLock<McpClient>>) -> JoinHandle<()> {
        let name = name.to_string();
        let servers = self.servers.clone();

        tokio::spawn(async move {
            let interval = {
                let servers_guard = servers.read().await;
                if let Some(handle) = servers_guard.get(&name) {
                    Duration::from_secs(handle.config.health_check_interval_secs)
                } else {
                    Duration::from_secs(30)
                }
            };

            loop {
                tokio::time::sleep(interval).await;

                // Check health by pinging
                let client_guard = client.read().await;
                let healthy = client_guard.ping().await.is_ok();
                drop(client_guard);

                // Update state
                let mut servers_guard = servers.write().await;
                if let Some(handle) = servers_guard.get_mut(&name) {
                    if handle.state.healthy != healthy {
                        if healthy {
                            info!("MCP server '{}' is now healthy", name);
                        } else {
                            warn!("MCP server '{}' is now unhealthy", name);
                        }
                        handle.state.healthy = healthy;
                    }
                }
            }
        })
    }
}

impl Drop for McpManager {
    fn drop(&mut self) {
        // This is a best-effort cleanup
        // Proper shutdown should be done via shutdown().await
        trace!("McpManager dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_new() {
        let config = McpConfig::default();
        let manager = McpManager::new(config);

        let servers = manager.list_servers().await;
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn test_server_not_found() {
        let config = McpConfig::default();
        let manager = McpManager::new(config);

        assert!(matches!(
            manager.get_server_state("nonexistent").await.unwrap_err(),
            ManagerError::ServerNotFound(_)
        ));
    }

    #[tokio::test]
    async fn test_server_not_running() {
        let mut config = McpConfig::default();
        config.add_server(McpServerConfig::stdio("test", "echo", vec![]));

        let manager = McpManager::new(config);
        manager.init().await.unwrap();

        assert!(matches!(
            manager.get_client("test").await.unwrap_err(),
            ManagerError::ServerNotRunning(_)
        ));
    }
}
