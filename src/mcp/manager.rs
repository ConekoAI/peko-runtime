//! MCP Manager
//!
//! Manages the lifecycle of MCP servers including:
//! - Starting and stopping servers
//! - Health monitoring and automatic reconnection
//! - Tool discovery and aggregation
//! - Configuration management

use crate::mcp::{
    client::{ClientError, McpClient},
    config::{McpConfig, McpServerConfig, TransportType},
    transport::{SseTransport, StdioTransport},
    types::Tool,
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
    /// MCP client (if running)
    client: Option<Arc<RwLock<McpClient>>>,
    /// Health check task
    health_task: Option<JoinHandle<()>>,
    /// Current state
    state: ServerState,
}

/// MCP Manager
///
/// Manages multiple MCP servers, handling their lifecycle and providing
/// aggregated access to their tools.
#[derive(Clone)]
pub struct McpManager {
    /// Server configurations
    config: Arc<RwLock<McpConfig>>,
    /// Running server handles
    servers: Arc<RwLock<HashMap<String, ServerHandle>>>,
    /// Default working directory for stdio servers
    default_cwd: Option<PathBuf>,
}

impl McpManager {
    /// Create a new MCP manager with the given configuration
    #[must_use]
    pub fn new(config: McpConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            servers: Arc::new(RwLock::new(HashMap::new())),
            default_cwd: None,
        }
    }

    /// Create a new MCP manager with a default working directory
    pub fn with_cwd(config: McpConfig, cwd: impl Into<PathBuf>) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            servers: Arc::new(RwLock::new(HashMap::new())),
            default_cwd: Some(cwd.into()),
        }
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

        // Create client based on transport type
        let mut client = match handle.config.transport {
            TransportType::Stdio => self.start_stdio_client(&handle.config).await?,
            TransportType::Sse => self.start_sse_client(&handle.config).await?,
        };

        // Initialize the client
        let init_timeout = Duration::from_secs(handle.config.init_timeout_secs);
        let server_info = match tokio::time::timeout(init_timeout, client.initialize()).await {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => {
                return Err(ManagerError::Client(e));
            }
            Err(_) => {
                return Err(ManagerError::InitTimeout);
            }
        };

        // Extract server info before releasing mutable borrow
        let server_info_str = format!(
            "{} v{}",
            server_info.server_info.name.clone(),
            server_info.server_info.version.clone()
        );

        // Discover tools
        let tools = if client.supports_capability("tools") {
            match client.list_tools().await {
                Ok(tools) => tools,
                Err(_e) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        // Update state
        let client_arc = Arc::new(RwLock::new(client));
        handle.client = Some(client_arc.clone());
        handle.state.running = true;
        handle.state.healthy = true;
        handle.state.server_info = Some(server_info_str);
        handle.state.tools = tools;
        handle.state.last_error = None;

        // Start health check task
        let health_task = self.start_health_check(name, client_arc);
        handle.health_task = Some(health_task);

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

        // Update state
        handle.state.running = false;
        handle.state.healthy = false;

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

        handle
            .client
            .clone()
            .ok_or_else(|| ManagerError::ServerNotRunning(name.to_string()))
    }

    /// Get server state
    pub async fn get_server_state(&self, name: &str) -> Result<ServerState> {
        let servers = self.servers.read().await;

        let handle = servers
            .get(name)
            .ok_or_else(|| ManagerError::ServerNotFound(name.to_string()))?;

        Ok(handle.state.clone())
    }

    /// List all servers and their states
    pub async fn list_servers(&self) -> Vec<ServerState> {
        let servers = self.servers.read().await;
        servers.values().map(|h| h.state.clone()).collect()
    }

    /// List all tools from all running servers
    pub async fn list_all_tools(&self) -> Vec<(String, Tool)> {
        let servers = self.servers.read().await;
        let mut all_tools = Vec::new();

        for (name, handle) in servers.iter() {
            if handle.state.running && handle.state.healthy {
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
    /// server has reserved_parameters configured) which implement the Tool trait.
    ///
    /// # Returns
    /// A vector of Arc<dyn Tool> containing all MCP tools from running servers
    pub async fn get_tools(&self) -> Vec<Arc<dyn crate::tools::Tool>> {
        use crate::mcp::injectable_proxy::InjectableMcpToolProxy;
        use crate::mcp::tool_proxy::McpToolProxy;

        let servers = self.servers.read().await;
        let manager_arc = Arc::new(RwLock::new(self.clone()));
        let mut tools: Vec<Arc<dyn crate::tools::Tool>> = Vec::new();

        for (server_name, handle) in servers.iter() {
            if handle.state.running && handle.state.healthy {
                // Check if this server has reserved parameters configured
                let has_reserved = !handle.config.reserved_parameters.is_empty();

                for tool in &handle.state.tools {
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
        }

        tools
    }

    /// Call a tool on a specific server
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<crate::mcp::types::CallToolResult> {
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

    /// Start a stdio transport client
    async fn start_stdio_client(&self, config: &McpServerConfig) -> Result<McpClient> {
        let command = config
            .command
            .as_ref()
            .ok_or_else(|| ManagerError::Config("Missing command".to_string()))?;

        let cwd = config.cwd.clone().or_else(|| self.default_cwd.clone());

        let transport = StdioTransport::spawn(command, &config.args, &config.env, cwd.as_deref())
            .await
            .map_err(|e| ManagerError::Transport(e.to_string()))?;

        Ok(McpClient::new(Box::new(transport)))
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
        };

        servers.insert(config.name.clone(), handle);
        info!(server_name = %config.name, "Added MCP server configuration");
        Ok(true)
    }

    /// Start health check task for a server
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
