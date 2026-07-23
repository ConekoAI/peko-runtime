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

use crate::common::vault::Vault;
use crate::daemon::background_runtime::{BackgroundRuntimeManager, RuntimeState};
use crate::extensions::framework::services::{ParamSource, ReservedParamsConfig};
use crate::extensions::mcp::protocol::{
    client::{ClientError, McpClient, ServerRequestHandler},
    config::{McpConfig, McpServerConfig, TransportType},
    sampling::SamplingRequestHandler,
    transport::SseTransport,
    types::{GetPromptResult, Prompt, Resource, ResourceContents, Tool},
};
use crate::extensions::mcp::runtime::adapter::{McpClientRegistry, McpRuntimeAdapter};
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
    /// Optional instructions from the server's initialize response
    pub instructions: Option<String>,
    /// Available tools
    pub tools: Vec<Tool>,
    /// Available resources
    pub resources: Vec<Resource>,
    /// Available prompts
    pub prompts: Vec<Prompt>,
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
    /// Optional LLM resolver used to handle server-to-client sampling requests.
    llm_resolver: Option<Arc<peko_providers::LlmResolver>>,
    /// F19: principal manager for per-server sampling attribution.
    /// When set, `sampling_handler_for(principal_id)` looks up the
    /// principal's quota meter and binds it to the
    /// `SamplingRequestHandler`. When unset, all sampling runs
    /// against an unlimited meter (no charging).
    principal_manager: Option<Arc<crate::principal::manager::PrincipalManager>>,
    /// Optional encrypted vault used for OAuth token storage.
    vault: Option<Arc<Vault>>,
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
            llm_resolver: None,
            principal_manager: None,
            vault: None,
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
            llm_resolver: None,
            principal_manager: None,
            vault: None,
        }
    }

    /// Create a new MCP manager in **shared mode**.
    ///
    /// Uses the daemon-wide `BackgroundRuntimeManager` and `McpClientRegistry`
    /// so that MCP servers started by this manager are visible to
    /// `peko ext status` and can be controlled via `peko ext start/stop`.
    ///
    /// # Arguments
    /// * `config` — Initial MCP server configurations
    /// * `runtime_manager` — Shared background runtime manager from `AppState`
    /// * `client_registry` — Shared client registry from `AppState`
    /// * `llm_resolver` — Optional resolver for `sampling/createMessage` requests
    /// * `principal_manager` — F19: optional principal manager so the
    ///   manager can build per-server sampling handlers bound to the
    ///   right quota meter.
    /// * `vault` — Optional encrypted vault for OAuth token storage
    #[must_use]
    pub fn with_shared_resources(
        config: McpConfig,
        runtime_manager: Arc<BackgroundRuntimeManager>,
        client_registry: Arc<McpClientRegistry>,
        llm_resolver: Option<Arc<peko_providers::LlmResolver>>,
        vault: Option<Arc<Vault>>,
    ) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            servers: Arc::new(RwLock::new(HashMap::new())),
            default_cwd: None,
            shared_runtime_manager: Some(runtime_manager),
            owned_runtime_manager: Arc::new(BackgroundRuntimeManager::new()),
            shared_client_registry: Some(client_registry),
            owned_client_registry: Arc::new(McpClientRegistry::new()),
            llm_resolver,
            principal_manager: None,
            vault,
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

    /// RP3C: expose the vault so injectable proxies can resolve
    /// vault-backed reserved parameters.
    pub(crate) fn vault(&self) -> Option<Arc<Vault>> {
        self.vault.clone()
    }

    /// RP3C: verify that any vault-backed reserved parameters for this
    /// server have a corresponding credential before starting.
    fn validate_vault_reserved_params(
        &self,
        server_name: &str,
        reserved: &ReservedParamsConfig,
    ) -> Result<()> {
        let Some(vault) = &self.vault else {
            for (name, source) in reserved.params.iter() {
                if matches!(source, ParamSource::Vault { .. }) {
                    return Err(ManagerError::Config(format!(
                        "MCP server '{server_name}' reserved param '{name}' uses source = \"vault\" but no vault is available"
                    )));
                }
            }
            return Ok(());
        };

        for (name, source) in reserved.params.iter() {
            if let ParamSource::Vault {
                namespace,
                name: param_name,
            } = source
            {
                match vault.get_material_for(namespace, param_name) {
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        return Err(ManagerError::Config(format!(
                            "MCP server '{server_name}' reserved param '{name}' has no vault credential at {namespace}/{param_name}; \
                             run `peko credential set {namespace} {param_name} api_key`"
                        )));
                    }
                    Err(e) => {
                        return Err(ManagerError::Config(format!(
                            "MCP server '{server_name}' reserved param '{name}' vault lookup failed: {e}"
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    /// Build a server-request handler for sampling when a resolver is configured.
    ///
    /// F19: per-server handler. The meter is resolved from the
    /// principal when `principal_id` is `Some`; otherwise an
    /// unlimited meter is used (daemon-level auto-start). The
    /// handler is constructed fresh on every call so each server
    /// gets its own binding.
    async fn sampling_handler(
        &self,
        principal_id: Option<&str>,
    ) -> Option<Arc<dyn ServerRequestHandler>> {
        let resolver = self.llm_resolver.as_ref()?;
        let meter = self.resolve_quota_meter(principal_id).await;
        Some(
            Arc::new(SamplingRequestHandler::new(Arc::clone(resolver), meter))
                as Arc<dyn ServerRequestHandler>,
        )
    }

    /// F19: resolve the principal's quota meter from
    /// `principal_manager`. When the manager isn't configured, or the
    /// principal can't be found, fall back to an unlimited meter so
    /// sampling keeps working without charging.
    async fn resolve_quota_meter(
        &self,
        principal_id: Option<&str>,
    ) -> Arc<crate::quota::QuotaMeter> {
        let Some(pm) = &self.principal_manager else {
            return Arc::new(crate::quota::QuotaMeter::unlimited());
        };
        let Some(pid) = principal_id else {
            return Arc::new(crate::quota::QuotaMeter::unlimited());
        };
        match pm.get_by_name(pid).await {
            Some(p) => Arc::clone(&p.quota_meter),
            None => Arc::new(crate::quota::QuotaMeter::unlimited()),
        }
    }

    /// F19: bind a principal manager so the manager can resolve per-
    /// principal quota meters for sampling attribution. Builder-style.
    #[must_use]
    pub fn with_principal_manager(
        mut self,
        pm: Arc<crate::principal::manager::PrincipalManager>,
    ) -> Self {
        self.principal_manager = Some(pm);
        self
    }

    /// RP3C: attach a vault so injectable proxies can resolve vault-backed
    /// reserved parameters. Builder-style.
    #[must_use]
    pub fn with_vault(mut self, vault: Arc<Vault>) -> Self {
        self.vault = Some(vault);
        self
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
                        instructions: None,
                        tools: Vec::new(),
                        resources: Vec::new(),
                        prompts: Vec::new(),
                    },
                    managed: self.shared_runtime_manager.is_some(),
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
            // F19: daemon-level auto-start has no principal scope, so
            // sampling runs with an unlimited meter. Tool-call-driven
            // auto-start (McpToolProxy / McpToolExecuteHandler) passes
            // a `Some(principal_id)` instead.
            if let Err(e) = self.start_server(&name, None).await {
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

    /// Start a specific server.
    ///
    /// F19: `principal_id` is the principal whose quota meter should
    /// be used for sampling calls issued by this server. Pass `None`
    /// when starting outside any principal scope (daemon-level
    /// auto-start, `peko ext start`, etc.) — sampling will run
    /// without charging. Tool-call-driven auto-start passes the
    /// caller's `principal_id` so sampling charges the right meter.
    pub async fn start_server(&self, name: &str, principal_id: Option<&str>) -> Result<()> {
        // First, check and fix stale state without holding a long-lived borrow.
        let is_managed: bool;
        {
            let mut servers = self.servers.write().await;
            let handle = servers
                .get_mut(name)
                .ok_or_else(|| ManagerError::ServerNotFound(name.to_string()))?;

            if handle.state.running {
                // Stale state: verify with BackgroundRuntimeManager before failing.
                // The runtime may have been stopped externally (e.g. via `peko ext stop`)
                // without updating our cached state.
                if handle.managed {
                    is_managed = true;
                } else {
                    return Err(ManagerError::ServerAlreadyRunning(name.to_string()));
                }
            } else {
                is_managed = handle.managed;
            }
            // Drop the lock here; we'll re-acquire it below.
        }

        // If we suspect stale state for a managed server, verify with the runtime manager.
        if is_managed {
            let actually_running = matches!(
                self.runtime_manager().get_state(name).await,
                Some(RuntimeState::Healthy | RuntimeState::Running | RuntimeState::Starting)
            );
            if actually_running {
                return Err(ManagerError::ServerAlreadyRunning(name.to_string()));
            }
            // Runtime is gone — reset cached state
            let mut servers = self.servers.write().await;
            if let Some(handle) = servers.get_mut(name) {
                handle.state.running = false;
                handle.state.healthy = false;
            }
        }

        info!("Starting MCP server: {}", name);

        let mut servers = self.servers.write().await;
        let handle = servers
            .get_mut(name)
            .ok_or_else(|| ManagerError::ServerNotFound(name.to_string()))?;

        // RP3C: refuse to start if vault-backed reserved params are missing.
        self.validate_vault_reserved_params(name, &handle.config.reserved_parameters)?;

        match handle.config.transport {
            TransportType::Stdio => {
                // Delegate to BackgroundRuntimeManager via McpRuntimeAdapter
                let config = handle.config.clone();
                drop(servers); // release lock before async call
                self.start_managed_server(name, &config, principal_id)
                    .await?;
                let mut servers = self.servers.write().await;
                if let Some(handle) = servers.get_mut(name) {
                    handle.state.running = true;
                    handle.state.healthy = true;
                    handle.state.last_error = None;
                    handle.managed = true;
                }
            }
            TransportType::Sse => {
                if is_managed {
                    // SSE server is supervised by the shared BackgroundRuntimeManager
                    let config = handle.config.clone();
                    drop(servers); // release lock before async call
                    self.start_managed_external_server(name, &config, principal_id)
                        .await?;
                    let mut servers = self.servers.write().await;
                    if let Some(handle) = servers.get_mut(name) {
                        handle.state.running = true;
                        handle.state.healthy = true;
                        handle.state.last_error = None;
                        handle.managed = true;
                    }
                } else {
                    // Standalone mode: handle SSE connection directly
                    let mut client = self.start_sse_client(&handle.config, principal_id).await?;

                    let init_timeout = Duration::from_secs(handle.config.init_timeout_secs);
                    let server_info =
                        match tokio::time::timeout(init_timeout, client.initialize()).await {
                            Ok(Ok(info)) => info,
                            Ok(Err(e)) => return Err(ManagerError::Client(e)),
                            Err(_) => return Err(ManagerError::InitTimeout),
                        };

                    // Clone owned data from the initialize response before we move
                    // `client` into the Arc/RwLock below.
                    let server_name = server_info.server_info.name.clone();
                    let server_version = server_info.server_info.version.clone();
                    let instructions = server_info.instructions.clone();
                    let server_info_str = format!("{} v{}", server_name, server_version);

                    let tools = if client.supports_capability("tools") {
                        match client.list_tools().await {
                            Ok(tools) => tools,
                            Err(_e) => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };

                    let resources = if client.supports_capability("resources") {
                        match client.list_resources().await {
                            Ok(resources) => resources,
                            Err(_e) => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };

                    let prompts = if client.supports_capability("prompts") {
                        match client.list_prompts().await {
                            Ok(prompts) => prompts,
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
                    handle.state.instructions = instructions;
                    handle.state.tools = tools;
                    handle.state.resources = resources;
                    handle.state.prompts = prompts;
                    handle.state.last_error = None;
                    handle.managed = false;

                    // Start health check task for SSE
                    let health_task = self.start_health_check(name, client_arc);
                    handle.health_task = Some(health_task);
                }
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
        // F19: `restart_server` is invoked from CLI (`peko ext restart`)
        // and external supervisors — neither carries a principal scope.
        self.start_server(name, None).await
    }

    /// Get client for a specific server
    pub async fn get_client(&self, name: &str) -> Result<Arc<RwLock<McpClient>>> {
        let servers = self.servers.read().await;

        let handle = servers
            .get(name)
            .ok_or_else(|| ManagerError::ServerNotFound(name.to_string()))?;

        if handle.managed {
            // For managed servers, the client lives in the shared registry.
            // The server may have been started by BackgroundRuntimeManager
            // (e.g. via `peko ext start`) rather than by McpManager::start_server(),
            // so handle.state.running may be false even though the runtime is alive.
            drop(servers);
            self.client_registry()
                .get_client(name)
                .await
                .ok_or_else(|| ManagerError::ServerNotRunning(name.to_string()))
        } else {
            if !handle.state.running {
                return Err(ManagerError::ServerNotRunning(name.to_string()));
            }
            // Client lives directly in the handle (SSE/direct mode)
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
        if handle.managed {
            // Check if the runtime is actually running in BackgroundRuntimeManager,
            // even if handle.state.running is false (server started via `peko ext start`
            // rather than McpManager::start_server()).
            if let Some(runtime_state) = self.runtime_manager().get_state(name).await {
                match runtime_state {
                    RuntimeState::Healthy | RuntimeState::Running | RuntimeState::Starting => {
                        handle.state.running = true;
                        handle.state.healthy =
                            matches!(runtime_state, RuntimeState::Healthy | RuntimeState::Running);
                    }
                    RuntimeState::Unhealthy | RuntimeState::Crashed => {
                        if handle.state.running {
                            handle.state.healthy = false;
                        }
                    }
                    RuntimeState::Stopped | RuntimeState::Stopping => {
                        handle.state.running = false;
                        handle.state.healthy = false;
                    }
                }
            }

            // Sync tools, server_info and instructions from registry if not already populated
            if handle.state.tools.is_empty()
                || handle.state.resources.is_empty()
                || handle.state.prompts.is_empty()
                || handle.state.server_info.is_none()
                || handle.state.instructions.is_none()
            {
                if let Some(info) = self.client_registry().get(name).await {
                    if handle.state.tools.is_empty() {
                        handle.state.tools = info.tools;
                    }
                    if handle.state.resources.is_empty() {
                        handle.state.resources = info.resources;
                    }
                    if handle.state.prompts.is_empty() {
                        handle.state.prompts = info.prompts;
                    }
                    if handle.state.server_info.is_none() {
                        handle.state.server_info = info.server_info;
                    }
                    if handle.state.instructions.is_none() {
                        handle.state.instructions = info.instructions;
                    }
                }
            }
        }

        // Fallback to the on-disk capability cache if any metadata is still missing
        // (e.g. the server is offline at agent-init time).
        let needs_cache = handle.state.tools.is_empty()
            || handle.state.resources.is_empty()
            || handle.state.prompts.is_empty()
            || handle.state.server_info.is_none()
            || handle.state.instructions.is_none();
        if needs_cache {
            if let Some(ref cwd) = handle.config.cwd {
                if let Ok(Some(cache)) =
                    crate::extensions::mcp::runtime::McpCapabilityCache::read(cwd, name).await
                {
                    if handle.state.tools.is_empty() {
                        handle.state.tools = cache.tools;
                    }
                    if handle.state.resources.is_empty() {
                        handle.state.resources = cache.resources;
                    }
                    if handle.state.prompts.is_empty() {
                        handle.state.prompts = cache.prompts;
                    }
                    if handle.state.server_info.is_none() {
                        handle.state.server_info = cache.server_info;
                    }
                    if handle.state.instructions.is_none() {
                        handle.state.instructions = cache.instructions;
                    }
                }
            }
        }

        Ok(handle.state.clone())
    }

    /// List context information for all configured servers, suitable for
    /// surfacing in the system prompt.  Includes running status, server info,
    /// instructions, and discovered tool names.
    pub async fn list_server_prompt_context(&self) -> Vec<ServerState> {
        let names: Vec<String> = {
            let servers = self.servers.read().await;
            servers.keys().cloned().collect()
        };

        let mut contexts = Vec::new();
        for name in names {
            if let Ok(state) = self.get_server_state(&name).await {
                contexts.push(state);
            }
        }
        contexts
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

    /// List all resources from all running servers
    pub async fn list_all_resources(&self) -> Vec<(String, Resource)> {
        let mut all_resources = Vec::new();

        // Resources from managed servers (via registry)
        let managed_resources = self.client_registry().list_all_resources().await;
        all_resources.extend(managed_resources);

        // Resources from directly-managed servers (SSE)
        let servers = self.servers.read().await;
        for (name, handle) in servers.iter() {
            if !handle.managed && handle.state.running && handle.state.healthy {
                for resource in &handle.state.resources {
                    all_resources.push((name.clone(), resource.clone()));
                }
            }
        }

        all_resources
    }

    /// List all prompts from all running servers
    pub async fn list_all_prompts(&self) -> Vec<(String, Prompt)> {
        let mut all_prompts = Vec::new();

        // Prompts from managed servers (via registry)
        let managed_prompts = self.client_registry().list_all_prompts().await;
        all_prompts.extend(managed_prompts);

        // Prompts from directly-managed servers (SSE)
        let servers = self.servers.read().await;
        for (name, handle) in servers.iter() {
            if !handle.managed && handle.state.running && handle.state.healthy {
                for prompt in &handle.state.prompts {
                    all_prompts.push((name.clone(), prompt.clone()));
                }
            }
        }

        all_prompts
    }

    /// Read a specific resource from a server
    pub async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> Result<Vec<ResourceContents>> {
        let client = self.get_client(server_name).await?;
        let client = client.read().await;

        match client.read_resource(uri).await {
            Ok(contents) => Ok(contents),
            Err(e) => Err(ManagerError::Client(e)),
        }
    }

    /// Get a specific prompt from a server
    pub async fn get_prompt(
        &self,
        server_name: &str,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<GetPromptResult> {
        let client = self.get_client(server_name).await?;
        let client = client.read().await;

        match client.get_prompt(name, arguments).await {
            Ok(result) => Ok(result),
            Err(e) => Err(ManagerError::Client(e)),
        }
    }

    /// Get all tools as Peko Tool trait objects
    ///
    /// This allows MCP tools to be used seamlessly with Peko's agent system.
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
    async fn start_managed_server(
        &self,
        name: &str,
        config: &McpServerConfig,
        principal_id: Option<&str>,
    ) -> Result<()> {
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
            self.sampling_handler(principal_id).await,
            self.vault.clone(),
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

    /// Start a managed SSE server via BackgroundRuntimeManager.
    async fn start_managed_external_server(
        &self,
        name: &str,
        config: &McpServerConfig,
        principal_id: Option<&str>,
    ) -> Result<()> {
        use crate::common::process::RestartPolicy;
        use crate::common::process::RuntimeSpawnConfig;

        let endpoint = config
            .endpoint
            .as_ref()
            .ok_or_else(|| ManagerError::Config("Missing endpoint".to_string()))?;

        let spawn_config = RuntimeSpawnConfig::External {
            endpoint: endpoint.clone(),
            connect_timeout: Duration::from_secs(config.init_timeout_secs),
        };

        let adapter = Arc::new(McpRuntimeAdapter::new(
            config.clone(),
            self.client_registry(),
            self.sampling_handler(principal_id).await,
            self.vault.clone(),
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
    async fn start_sse_client(
        &self,
        config: &McpServerConfig,
        principal_id: Option<&str>,
    ) -> Result<McpClient> {
        let endpoint = config
            .endpoint
            .as_ref()
            .ok_or_else(|| ManagerError::Config("Missing endpoint".to_string()))?;

        let transport = SseTransport::connect_with_auth(
            endpoint,
            config.auth.clone(),
            self.vault.clone(),
            config.name.clone(),
        )
        .await
        .map_err(|e| ManagerError::Transport(e.to_string()))?;

        Ok(match self.sampling_handler(principal_id).await {
            Some(handler) => McpClient::with_handler(Box::new(transport), handler),
            None => McpClient::new(Box::new(transport)),
        })
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
                instructions: None,
                tools: Vec::new(),
                resources: Vec::new(),
                prompts: Vec::new(),
            },
            managed: self.shared_runtime_manager.is_some(),
        };

        servers.insert(config.name.clone(), handle);
        info!(server_name = %config.name, "Added MCP server configuration");
        Ok(true)
    }

    /// Replace an existing server configuration, stopping it first if running.
    pub async fn replace_server_config(&self, config: McpServerConfig) -> Result<bool> {
        let existed = {
            let servers = self.servers.read().await;
            servers.contains_key(&config.name)
        };

        if existed {
            let was_running = {
                let servers = self.servers.read().await;
                servers
                    .get(&config.name)
                    .map(|h| h.state.running)
                    .unwrap_or(false)
            };
            if was_running {
                self.stop_server(&config.name).await?;
            }
            let mut servers = self.servers.write().await;
            servers.remove(&config.name);
        }

        let mut servers = self.servers.write().await;
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
                instructions: None,
                tools: Vec::new(),
                resources: Vec::new(),
                prompts: Vec::new(),
            },
            managed: self.shared_runtime_manager.is_some(),
        };

        servers.insert(config.name.clone(), handle);
        info!(server_name = %config.name, replaced = existed, "Updated MCP server configuration");
        Ok(existed)
    }

    /// Remove a server configuration, stopping it first if running.
    pub async fn remove_server_config(&self, name: &str) -> Result<bool> {
        let existed = {
            let servers = self.servers.read().await;
            servers.contains_key(name)
        };

        if existed {
            let was_running = {
                let servers = self.servers.read().await;
                servers.get(name).map(|h| h.state.running).unwrap_or(false)
            };
            if was_running {
                self.stop_server(name).await?;
            }
            let mut servers = self.servers.write().await;
            servers.remove(name);
        }

        if existed {
            info!(server_name = %name, "Removed MCP server configuration");
        }
        Ok(existed)
    }

    /// Reload MCP server configuration from a TOML file.
    ///
    /// Adds new servers, updates existing ones, and removes servers that are no
    /// longer present in the file.
    pub async fn reload_config(&self, path: &std::path::Path) -> Result<usize> {
        let config = if path.exists() {
            McpConfig::from_file(path)
                .await
                .map_err(|e| ManagerError::Config(format!("failed to read {path:?}: {e}")))?
        } else {
            McpConfig::default()
        };

        let new_names: std::collections::HashSet<String> =
            config.servers.iter().map(|s| s.name.clone()).collect();

        // Remove servers no longer in the file.
        let old_names: Vec<String> = {
            let servers = self.servers.read().await;
            servers.keys().cloned().collect()
        };
        for name in old_names {
            if !new_names.contains(&name) {
                self.remove_server_config(&name).await?;
            }
        }

        // Add or update servers from the file.
        for server_config in config.servers {
            let exists = {
                let servers = self.servers.read().await;
                servers.contains_key(&server_config.name)
            };
            if exists {
                self.replace_server_config(server_config).await?;
            } else {
                self.add_server_config(server_config).await?;
            }
        }

        let count = self.servers.read().await.len();
        Ok(count)
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

        let servers = manager.list_server_prompt_context().await;
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

    #[tokio::test]
    async fn test_list_server_prompt_context_includes_offline_server() {
        let mut config = McpConfig::default();
        config.add_server(McpServerConfig::stdio("offline-server", "echo", vec![]));

        let manager = McpManager::new(config);
        manager.init().await.unwrap();

        let contexts = manager.list_server_prompt_context().await;
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].name, "offline-server");
        assert!(!contexts[0].running);
        assert!(contexts[0].instructions.is_none());
    }

    /// RP3C: a server with a vault-backed reserved parameter must not
    /// start when the credential is absent.
    #[tokio::test]
    async fn start_server_rejects_missing_vault_reserved_param() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::for_test(tmp.path(), "test-passphrase"));

        let mut config = McpConfig::default();
        config.add_server(
            McpServerConfig::sse("remote", "http://localhost:9999/mcp").with_reserved_parameters(
                ReservedParamsConfig::new().with_vault("api_key", "mcp:remote", "default"),
            ),
        );

        let manager = McpManager::new(config).with_vault(vault);
        manager.init().await.unwrap();

        let err = manager.start_server("remote", None).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no vault credential"),
            "expected missing-credential error, got: {msg}"
        );
        assert!(
            msg.contains("mcp:remote/default"),
            "error should name the slot: {msg}"
        );
    }
}
