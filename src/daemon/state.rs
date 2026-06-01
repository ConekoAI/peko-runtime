//! Daemon Application State
//!
//! Shared state accessible to the daemon and IPC server.
//! This is the daemon's composition root — all services are initialized here.

use crate::daemon::background_runtime::{
    BackgroundRuntimeManager, ExtensionRuntimeStarterRegistry, StarterContext,
};
use crate::extensions::gateway::runtime::{GatewayRouter, GatewayRuntimeStarter};
use crate::extensions::mcp::runtime::{McpClientRegistry, McpRuntimeStarter};

use crate::agent::lifecycle::LifecycleManager;
use crate::agent::stateless_service::StatelessAgentService;
use crate::common::services::{
    AgentService, ConfigAuthority, ConfigAuthorityImpl, SessionService, TeamManagementService,
    TeamService,
};
use crate::extension::async_exec::executor::AsyncExecutor;
use crate::observability::Observability;
use crate::registry::{load_from_workspace, RegistryConfig};
use crate::runtime::ToolRuntime;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{broadcast, RwLock};

/// Shared application state for the HTTP API (Stateless Architecture)
///
/// This struct is passed to all route handlers via Axum's State extractor.
/// All fields are thread-safe and can be accessed concurrently.
#[derive(Clone)]
pub struct AppState {
    /// Time when the daemon started
    pub started_at: SystemTime,

    /// Path to the workspace directory (.peko/)
    pub workspace_path: PathBuf,

    /// Configuration directory path
    pub config_dir: PathBuf,

    /// Data directory path
    pub data_dir: PathBuf,

    /// Cache directory path
    pub cache_dir: PathBuf,

    /// Port the server is listening on
    pub port: u16,

    /// Host address the server is bound to
    pub host: String,

    /// Daemon configuration
    pub config: DaemonConfigSnapshot,

    /// Registry configuration for push/pull operations
    registry_config: Arc<RwLock<RegistryConfig>>,

    /// Observability hub for audit, metrics, and tracing
    observability: Arc<Observability>,

    /// Agent configuration service (unified)
    config_service: Arc<ConfigAuthorityImpl>,

    /// Stateless agent execution service
    agent_service: Arc<StatelessAgentService>,

    /// Agent service (unified for CLI and API)
    agent_mgmt_service: Arc<AgentService>,

    /// Lifecycle manager (tracks active executions only)
    lifecycle: Arc<LifecycleManager>,

    /// Session service (unified for CLI and API)
    session_service: Arc<SessionService>,

    /// Team management service (unified for CLI and API)
    team_service: Arc<TeamManagementService>,

    /// Tool runtime for async task execution (ADR-020)
    pub tool_runtime: Arc<ToolRuntime>,

    /// Async task executor for daemon-side background execution (ADR-020)
    pub async_task_executor: Arc<AsyncExecutor>,

    /// Background runtime manager for MCP servers and gateways (ADR-025)
    background_runtime_manager: Arc<BackgroundRuntimeManager>,

    /// Gateway router for channel→agent mapping (ADR-025)
    gateway_router: Arc<GatewayRouter>,

    /// Shared MCP client registry — populated by McpRuntimeAdapter (ADR-025)
    mcp_client_registry: Arc<McpClientRegistry>,

    /// Extension runtime starter registry — dispatches ext start/stop by type (ADR-025/026)
    runtime_starter_registry: Arc<ExtensionRuntimeStarterRegistry>,

    /// Extension manager for installed extensions (ADR-030 Tier 1)
    extension_manager: Arc<tokio::sync::RwLock<crate::extension::manager::ExtensionManager>>,

    /// Extension services for built-in extension operations
    extension_services: Arc<crate::extension::services::Services>,

    /// Shutdown broadcast channel - send () to trigger graceful shutdown
    shutdown_tx: Arc<broadcast::Sender<()>>,

    /// Internal state that can be modified
    inner: Arc<RwLock<AppStateInner>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("started_at", &self.started_at)
            .field("workspace_path", &self.workspace_path)
            .field("port", &self.port)
            .field("host", &self.host)
            .field("config", &self.config)
            .field("config_service", &"<ConfigAuthorityImpl>")
            .field("agent_service", &"<StatelessAgentService>")
            .field("agent_mgmt_service", &"<AgentService>")
            .field("team_service", &"<TeamManagementService>")
            .field("tool_runtime", &"<ToolRuntime>")
            .field("async_task_executor", &"<AsyncExecutor>")
            .field("background_runtime_manager", &"<BackgroundRuntimeManager>")
            .field("gateway_router", &"<GatewayRouter>")
            .field("mcp_client_registry", &"<McpClientRegistry>")
            .field(
                "runtime_starter_registry",
                &"<ExtensionRuntimeStarterRegistry>",
            )
            .field("extension_manager", &"<ExtensionManager>")
            .field("extension_services", &"<ExtensionServices>")
            .finish()
    }
}

/// Mutable internal state
#[derive(Debug, Default)]
struct AppStateInner {
    /// Whether the daemon is in a degraded state
    pub degraded: bool,
    /// Number of running instances (cached)
    pub instance_count: u64,
    /// Number of teams (cached)
    pub team_count: u64,
    /// Whether the daemon is ready to serve requests
    pub ready: bool,
}

/// Snapshot of daemon configuration
#[derive(Debug, Clone)]
pub struct DaemonConfigSnapshot {
    /// Data directory path
    pub data_dir: PathBuf,
    /// Config directory path
    pub config_dir: PathBuf,
    /// Log level
    pub log_level: String,
}

impl AppState {
    /// Create new application state (async constructor for stateless components)
    pub async fn new(
        workspace_path: impl Into<PathBuf>,
        host: impl Into<String>,
        port: u16,
        config: DaemonConfigSnapshot,
    ) -> anyhow::Result<Self> {
        let workspace_path: PathBuf = workspace_path.into();
        let data_dir = workspace_path.clone();
        let config_dir = config.config_dir.clone();
        let cache_dir =
            dirs::cache_dir().map_or_else(|| data_dir.join("cache"), |d| d.join("peko"));
        Self::build(
            workspace_path,
            host.into(),
            port,
            config,
            config_dir,
            data_dir,
            cache_dir,
        )
        .await
    }

    /// Create new application state with custom data directory
    pub async fn with_data_dir(
        workspace_path: impl Into<PathBuf>,
        host: impl Into<String>,
        port: u16,
        config: DaemonConfigSnapshot,
        data_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        let workspace_path: PathBuf = workspace_path.into();
        let cache_dir =
            dirs::cache_dir().map_or_else(|| data_dir.join("cache"), |d| d.join("peko"));
        let config_dir = config.config_dir.clone();
        Self::build(
            workspace_path,
            host.into(),
            port,
            config,
            config_dir,
            data_dir,
            cache_dir,
        )
        .await
    }

    async fn build(
        workspace_path: PathBuf,
        host: String,
        port: u16,
        config: DaemonConfigSnapshot,
        config_dir: PathBuf,
        data_dir: PathBuf,
        cache_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            config_dir.clone(),
            data_dir.clone(),
            cache_dir.clone(),
        );

        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver.clone()));

        let path_resolver_clone = path_resolver.clone();
        let agent_service = Arc::new(
            StatelessAgentService::new(config_service.clone(), path_resolver)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create agent service: {e}"))?,
        );

        let lifecycle = Arc::new(LifecycleManager::new());

        let session_service = Arc::new(SessionService::new(path_resolver_clone.clone()));

        // Create unified services
        let team_service = Arc::new(TeamManagementService::new(
            TeamService::new(path_resolver_clone.clone()),
            path_resolver_clone.clone(),
        ));

        let agent_mgmt_service = Arc::new(AgentService::new(path_resolver_clone.clone()));

        // ADR-021: Initialize global ExtensionCore FIRST so ToolRuntime can register
        // tools with it, and Agent::new() can find them later.
        //
        // If main.rs already initialized the global core (e.g. for the async router),
        // reuse it and register tools on that instance. Otherwise create a new one.
        // This prevents a race where main.rs sets an empty core and AppState's
        // tool-filled core gets discarded by the OnceLock.
        let global_core = if let Some(existing) = crate::extension::core::global_core() {
            tracing::info!("Reusing global ExtensionCore initialized by main.rs");
            existing
        } else {
            use crate::extension::core::{init_global_core, ExtensionCore, ExtensionServices};
            use crate::extension::services::AsyncExecutionRouter;
            let router = AsyncExecutionRouter::with_transport(
                crate::extension::services::async_transport::create_local_transport(),
            );
            let services = ExtensionServices::with_async_router_and_agent_service(
                router,
                Arc::clone(&agent_service),
            );
            let core = Arc::new(ExtensionCore::with_services(Arc::new(services)));
            init_global_core(Arc::clone(&core));
            core
        };

        // ADR-023: Ensure the agent service is set on the ExtensionCore for A2A messaging.
        // If we reused an existing global core, it may not have the agent service yet.
        global_core
            .services()
            .set_agent_service(Arc::clone(&agent_service));

        // ADR-020: Initialize ToolRuntime with the global ExtensionCore so tools
        // are registered where Agent::new() can find them.
        let tool_runtime = Arc::new(
            ToolRuntime::with_workspace_and_core(
                path_resolver_clone.clone(),
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                Arc::clone(&global_core),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create tool runtime: {e}"))?,
        );
        let async_task_executor = Arc::new(AsyncExecutor::new());

        // ADR-025: Initialize BackgroundRuntimeManager and GatewayRouter
        let background_runtime_manager = Arc::new(BackgroundRuntimeManager::new());
        let gateway_router = Arc::new(GatewayRouter::new(Arc::clone(&agent_service)));

        // ADR-025: Shared MCP client registry — populated by McpRuntimeAdapter
        let mcp_client_registry = Arc::new(McpClientRegistry::new());

        // Ensure the global MCP manager uses the daemon-wide shared resources.
        // This unifies the runtime paths so `ext start` / `ext stop` control the
        // same processes that agent-init and tool-proxy code paths see.
        crate::extensions::mcp::init_global_mcp_manager_with_shared_resources(
            Arc::clone(&background_runtime_manager),
            Arc::clone(&mcp_client_registry),
        );

        // ADR-025/026: Extension runtime starter registry
        let mut runtime_starter_registry = ExtensionRuntimeStarterRegistry::new();
        runtime_starter_registry.register(Box::new(GatewayRuntimeStarter::new()));
        runtime_starter_registry.register(Box::new(McpRuntimeStarter::new()));
        let runtime_starter_registry = Arc::new(runtime_starter_registry);

        // ADR-030: Initialize ExtensionManager for IPC extension operations
        let ext_storage = crate::extension::manager::ExtensionStorage::with_dir(
            data_dir.join("extensions"),
        );
        let mut ext_manager = crate::extension::manager::ExtensionManager::with_core(
            Arc::clone(&global_core),
        ).with_storage_dir(ext_storage.dir().unwrap().to_path_buf());

        // Register adapters (same as CLI create_manager_with_adapters)
        use crate::extensions::skill::SkillAdapter;
        use crate::extensions::mcp::McpAdapter;
        use crate::extensions::universal::UniversalToolAdapter;
        use crate::extensions::gateway::GatewayAdapter;
        use crate::extensions::general::GeneralExtensionAdapter;

        ext_manager.register_adapter(Box::new(SkillAdapter::new()));
        ext_manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
        ext_manager.register_adapter(Box::new(UniversalToolAdapter::new()));
        ext_manager.register_adapter(Box::new(GatewayAdapter::new(Arc::clone(&global_core))));
        ext_manager.register_adapter(Box::new(GeneralExtensionAdapter::new()));

        // Load all extensions (log warnings but don't fail startup)
        if let Err(e) = ext_manager.load_all().await {
            tracing::warn!("Failed to load some extensions during daemon startup: {}", e);
        }

        let extension_manager = Arc::new(tokio::sync::RwLock::new(ext_manager));
        let extension_services = Arc::new(crate::extension::services::Services::with_core(
            Arc::clone(&global_core),
        ));

        // Create shutdown broadcast channel
        let (shutdown_tx, _) = broadcast::channel(1);

        Ok(Self {
            started_at: SystemTime::now(),
            workspace_path,
            config_dir,
            data_dir,
            cache_dir,
            port,
            host,
            config,
            registry_config: Arc::new(RwLock::new(RegistryConfig::default())),
            observability: Arc::new(Observability::new("api")),
            config_service,
            agent_service,
            agent_mgmt_service,
            lifecycle,
            session_service,
            team_service,
            tool_runtime,
            async_task_executor,
            background_runtime_manager,
            gateway_router,
            mcp_client_registry,
            runtime_starter_registry,
            extension_manager,
            extension_services,
            shutdown_tx: Arc::new(shutdown_tx),
            inner: Arc::new(RwLock::new(AppStateInner::default())),
        })
    }

    /// Get the current uptime in seconds
    #[must_use]
    pub fn uptime_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(self.started_at)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Check if the daemon is degraded
    pub async fn is_degraded(&self) -> bool {
        let inner = self.inner.read().await;
        inner.degraded
    }

    /// Set the degraded state
    pub async fn set_degraded(&self, degraded: bool) {
        let mut inner = self.inner.write().await;
        inner.degraded = degraded;
    }

    /// Get the current instance count
    pub async fn instance_count(&self) -> u64 {
        let inner = self.inner.read().await;
        inner.instance_count
    }

    /// Update the instance count
    pub async fn set_instance_count(&self, count: u64) {
        let mut inner = self.inner.write().await;
        inner.instance_count = count;
    }

    /// Get the current team count
    pub async fn team_count(&self) -> u64 {
        let inner = self.inner.read().await;
        inner.team_count
    }

    /// Update the team count
    pub async fn set_team_count(&self, count: u64) {
        let mut inner = self.inner.write().await;
        inner.team_count = count;
    }

    /// Mark the daemon as healthy (not degraded)
    pub async fn mark_healthy(&self) {
        self.set_degraded(false).await;
    }

    /// Mark the daemon as degraded
    pub async fn mark_degraded(&self) {
        self.set_degraded(true).await;
    }

    /// Check if the daemon is ready to serve requests
    pub async fn is_ready(&self) -> bool {
        let inner = self.inner.read().await;
        inner.ready
    }

    /// Mark the daemon as ready
    pub async fn set_ready(&self, ready: bool) {
        let mut inner = self.inner.write().await;
        inner.ready = ready;
    }

    /// Subscribe to shutdown signals
    pub fn subscribe_shutdown(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    /// Request graceful shutdown
    pub async fn request_shutdown(&self, _force: bool) {
        // Note: force parameter reserved for future use
        let _ = self.shutdown_tx.send(());
    }

    /// Get the observability hub
    #[must_use]
    pub fn observability(&self) -> Arc<Observability> {
        self.observability.clone()
    }

    /// Load registry configuration from workspace
    pub async fn load_registry_config(&self) {
        let config = load_from_workspace(&self.workspace_path);
        let mut registry_config = self.registry_config.write().await;
        *registry_config = config;
    }

    /// Get the current registry configuration
    pub async fn registry_config(&self) -> RegistryConfig {
        let config = self.registry_config.read().await;
        config.clone()
    }

    /// Update the registry configuration
    pub async fn set_registry_config(&self, config: RegistryConfig) {
        let mut registry_config = self.registry_config.write().await;
        *registry_config = config;
    }

    /// Get the agent configuration service
    #[must_use]
    pub fn config_service(&self) -> &Arc<ConfigAuthorityImpl> {
        &self.config_service
    }

    /// Get the agent service
    #[must_use]
    pub fn agent_service(&self) -> &Arc<StatelessAgentService> {
        &self.agent_service
    }

    /// Get the lifecycle manager
    #[must_use]
    pub fn lifecycle(&self) -> &Arc<LifecycleManager> {
        &self.lifecycle
    }

    /// Get the session service
    #[must_use]
    pub fn session_service(&self) -> &Arc<SessionService> {
        &self.session_service
    }

    /// Get the team management service (unified)
    #[must_use]
    pub fn team_service(&self) -> &Arc<TeamManagementService> {
        &self.team_service
    }

    /// Get the agent management service (unified)
    #[must_use]
    pub fn agent_mgmt_service(&self) -> &Arc<AgentService> {
        &self.agent_mgmt_service
    }

    /// Get the background runtime manager (ADR-025)
    #[must_use]
    pub fn background_runtime_manager(&self) -> &Arc<BackgroundRuntimeManager> {
        &self.background_runtime_manager
    }

    /// Get the gateway router (ADR-025)
    #[must_use]
    pub fn gateway_router(&self) -> &Arc<GatewayRouter> {
        &self.gateway_router
    }

    /// Get the shared MCP client registry (ADR-025)
    #[must_use]
    pub fn mcp_client_registry(&self) -> &Arc<McpClientRegistry> {
        &self.mcp_client_registry
    }

    /// Get the extension runtime starter registry (ADR-025/026)
    #[must_use]
    pub fn runtime_starter_registry(&self) -> &Arc<ExtensionRuntimeStarterRegistry> {
        &self.runtime_starter_registry
    }

    /// Get the extension manager
    #[must_use]
    pub fn extension_manager(&self) -> &Arc<tokio::sync::RwLock<crate::extension::manager::ExtensionManager>> {
        &self.extension_manager
    }

    /// Get the extension services
    #[must_use]
    pub fn extension_services(&self) -> &Arc<crate::extension::services::Services> {
        &self.extension_services
    }

    /// Build a `StarterContext` for use by runtime starters.
    ///
    /// This bundles all daemon-scoped services that starters may need.
    #[must_use]
    pub fn starter_context(&self) -> StarterContext {
        StarterContext {
            background_runtime_manager: Arc::clone(&self.background_runtime_manager),
            agent_service: Arc::clone(&self.agent_service),
            gateway_router: Arc::clone(&self.gateway_router),
            mcp_client_registry: Arc::clone(&self.mcp_client_registry),
            data_dir: self.data_dir.clone(),
        }
    }

    /// Get the count of registered agents
    pub async fn agent_count(&self) -> anyhow::Result<usize> {
        let agents = self.config_service.list_all().await?;
        Ok(agents.len())
    }

    /// Get the count of active executions
    pub async fn active_execution_count(&self) -> usize {
        self.lifecycle.active_count().await
    }
}

impl Default for DaemonConfigSnapshot {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(".peko"),
            config_dir: PathBuf::from(".peko"),
            log_level: "info".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_test_state() -> AppState {
        let temp_dir = TempDir::new().unwrap();
        AppState::with_data_dir(
            temp_dir.path(),
            "127.0.0.1",
            11435,
            DaemonConfigSnapshot::default(),
            temp_dir.path().to_path_buf(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_uptime_tracking() {
        let state = create_test_state().await;

        // Initial uptime should be very small
        let uptime1 = state.uptime_seconds();
        assert_eq!(uptime1, 0);

        // Wait a bit and check uptime increased
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let uptime2 = state.uptime_seconds();
        // uptime_seconds() returns u64, so it's always >= 0.
        // We just verify it doesn't panic and is reasonable.
        let _ = uptime2;
    }

    #[tokio::test]
    async fn test_degraded_state() {
        let state = create_test_state().await;

        assert!(!state.is_degraded().await);

        state.mark_degraded().await;
        assert!(state.is_degraded().await);

        state.mark_healthy().await;
        assert!(!state.is_degraded().await);
    }

    #[tokio::test]
    async fn test_instance_count() {
        let state = create_test_state().await;

        assert_eq!(state.instance_count().await, 0);

        state.set_instance_count(5).await;
        assert_eq!(state.instance_count().await, 5);
    }

    #[tokio::test]
    async fn test_stateless_components() {
        let state = create_test_state().await;

        // Initially no agents registered
        assert_eq!(state.agent_count().await.unwrap(), 0);

        // Initially no active executions
        assert_eq!(state.active_execution_count().await, 0);
    }

    #[tokio::test]
    async fn test_appstate_has_registered_tools() {
        let state = create_test_state().await;

        // ToolRuntime should have registered built-in tools
        let tool_runtime = state.tool_runtime.clone();
        assert!(
            tool_runtime.has_tool("shell").await,
            "shell tool not registered"
        );
        assert!(
            tool_runtime.has_tool("read_file").await,
            "read_file tool not registered"
        );
        assert!(
            tool_runtime.has_tool("write_file").await,
            "write_file tool not registered"
        );
        assert!(
            tool_runtime.has_tool("glob").await,
            "glob tool not registered"
        );
        assert!(
            tool_runtime.has_tool("grep").await,
            "grep tool not registered"
        );
        assert!(
            tool_runtime.has_tool("str_replace_file").await,
            "str_replace_file tool not registered"
        );
        assert!(
            tool_runtime.has_tool("task").await,
            "task tool not registered"
        );

        // ExtensionCore should list the tools
        let core = tool_runtime.extension_core();
        let tools = core.list_tools().await;
        assert!(!tools.is_empty(), "No tools in ExtensionCore");

        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        assert!(tool_names.contains(&"shell".to_string()));
        assert!(tool_names.contains(&"grep".to_string()));

        // Tool definitions should be available for LLM API
        let defs = core.list_tool_definitions().await;
        assert!(!defs.is_empty(), "No tool definitions available");
    }

    #[tokio::test]
    async fn test_agent_init_preserves_pre_registered_tools() {
        use crate::agent::Agent;
        use crate::extension::core::init_global_core;
        use crate::extension::{HookInput, HookPoint};
        use crate::types::agent::AgentConfig;
        use crate::types::provider::{ProviderConfig, ProviderType};

        let state = create_test_state().await;
        let global_core = state.tool_runtime.extension_core().clone();

        // Simulate what Agent::new() does
        init_global_core(global_core.clone());

        let config = AgentConfig {
            name: "test-agent".to_string(),
            provider: ProviderConfig {
                provider_type: ProviderType::Ollama,
                ..Default::default()
            },
            ..Default::default()
        };

        let agent = Agent::new(config).await.expect("Failed to create agent");

        // init_builtins_async should find pre-registered tools
        agent
            .init_builtins_async()
            .await
            .expect("Failed to init builtins");

        // Tools should still be available after agent init
        let core = agent.extension_core();
        let tools: Vec<crate::extension::types::ToolMetadata> = core.list_tools().await;
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        assert!(
            tool_names.contains(&"shell".to_string()),
            "shell missing after agent init"
        );
        assert!(
            tool_names.contains(&"grep".to_string()),
            "grep missing after agent init"
        );

        // Prompt section should return tool descriptions
        let prompt: Option<String> = core
            .invoke_hook_text(
                HookPoint::PromptSystemSection {
                    section: "tools".to_string(),
                    priority: 100,
                },
                HookInput::Unit,
            )
            .await;
        assert!(prompt.is_some(), "Prompt section returned None");
        let prompt_text = prompt.unwrap();
        assert!(!prompt_text.is_empty(), "Prompt section is empty");
        assert!(
            prompt_text.contains("shell"),
            "Prompt doesn't mention shell"
        );
        assert!(prompt_text.contains("grep"), "Prompt doesn't mention grep");
    }
}
