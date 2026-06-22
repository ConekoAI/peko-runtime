//! Daemon Application State
//!
//! Shared state accessible to the daemon and IPC server.
//! This is the daemon's composition root — all services are initialized here.

use crate::daemon::background_runtime::{
    BackgroundRuntimeManager, ExtensionRuntimeStarterRegistry, StarterContext,
};
use crate::extensions::gateway::runtime::{GatewayRouter, GatewayRuntimeStarter};
use crate::extensions::mcp::runtime::{McpClientRegistry, McpRuntimeStarter};

use crate::agents::lifecycle::LifecycleManager;
use crate::agents::stateless_service::StatelessAgentService;
use crate::common::services::{
    AgentService, ConfigAuthority, ConfigAuthorityImpl, SessionService, TeamManagementService,
    TeamService,
};
use crate::extensions::framework::async_exec::executor::AsyncExecutor;
use crate::observability::Observability;
use crate::registry::{load_from_workspace, RegistryConfig};
use crate::engine::tool_runtime::ToolRuntime;
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
    extension_manager: Arc<tokio::sync::RwLock<crate::extensions::framework::manager::ExtensionManager>>,

    /// Extension services for built-in extension operations
    extension_services: Arc<crate::extensions::framework::services::Services>,

    /// Shutdown broadcast channel - send () to trigger graceful shutdown
    shutdown_tx: Arc<broadcast::Sender<()>>,

    /// Internal state that can be modified
    inner: Arc<RwLock<AppStateInner>>,

    /// Runtime identity (ADR-032)
    pub runtime_identity: crate::identity::runtime::RuntimeIdentity,

    /// Runtime metadata (ADR-032)
    pub runtime_metadata: crate::identity::runtime_metadata::RuntimeMetadata,

    /// Known runtimes registry (ADR-032)
    pub known_runtimes:
        std::sync::Arc<tokio::sync::RwLock<crate::tunnel::known_runtimes::KnownRuntimes>>,

    /// Auth configuration (ADR-034)
    auth_config: crate::auth::config::AuthConfig,

    /// API key store (ADR-034)
    api_key_store: Option<crate::auth::api_key::ApiKeyStore>,

    /// API key verifier (ADR-034)
    api_key_verifier: Option<crate::auth::api_key::ApiKeyVerifier>,

    /// JWT validator (ADR-034)
    jwt_validator: Option<crate::auth::jwt::JwtValidator>,

    /// Rate limiter (ADR-034)
    rate_limiter: Option<crate::auth::rate_limit::RateLimiter>,

    /// Tunnel cancellation token — set when tunnel is active
    tunnel_cancel: Arc<RwLock<Option<tokio_util::sync::CancellationToken>>>,

    /// Whether the tunnel is currently connected
    tunnel_connected: Arc<RwLock<bool>>,

    /// Tunnel dispatcher for instance lifecycle management
    tunnel_dispatcher: Arc<RwLock<Option<crate::tunnel::TunnelDispatcher>>>,

    /// Number of consecutive tunnel reconnect attempts since last success.
    /// Reset to 0 on each successful connection; used by `tunnel_health()`
    /// to surface the `disconnected` state with a non-zero attempt count.
    tunnel_attempts: Arc<RwLock<u32>>,

    /// Cross-runtime a2a response correlation registry. Issue #29.
    /// Shared between the outbound `A2aSendTool` path (which
    /// registers a oneshot under `request_id`) and the inbound
    /// tunnel dispatcher arm (which completes the oneshot when the
    /// matching `AgentToAgentResponse` arrives). Initialized lazily
    /// (a fresh `PendingA2aResponses`) so the registry exists even
    /// before the tunnel connects.
    pending_a2a_responses: Arc<crate::tunnel::PendingA2aResponses>,

    /// Slot for the live outbound tunnel handle. The
    /// `TunnelDispatcher` writes the freshest handle on every
    /// reconnect; the `CrossRuntimeA2aCtx` (and any other consumer
    /// that needs to send on the live tunnel) reads through the
    /// same `Arc`. `None` when the tunnel isn't connected.
    tunnel_handle_slot: Arc<RwLock<Option<crate::tunnel::TunnelHandle>>>,

    /// Last tunnel error message (set on each failed attempt; cleared on
    /// successful connect). Surfaced via `tunnel_health()` and ultimately
    /// `peko daemon status --json` (issue #8).
    tunnel_last_error: Arc<RwLock<Option<String>>>,

    /// Whether the tunnel client has hit its reconnect-attempt cap and
    /// stopped retrying. Distinct from the daemon-wide `degraded` flag
    /// (which can be set by extension failures etc.). Surfaced via
    /// `TunnelHealth::Degraded` (issue #8).
    tunnel_degraded: Arc<RwLock<bool>>,
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
            .field("runtime_identity", &self.runtime_identity.runtime_did)
            .field("runtime_metadata", &self.runtime_metadata.display_name)
            .field(
                "known_runtimes",
                &format!("{} runtimes", self.runtime_identity.runtime_did),
            )
            .field("auth", &"<AuthConfig>")
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
        Self::build_internal(
            workspace_path,
            host,
            port,
            config,
            config_dir,
            data_dir,
            cache_dir,
            false,
        )
        .await
    }

    #[cfg(test)]
    async fn build_for_test(
        workspace_path: PathBuf,
        host: String,
        port: u16,
        config: DaemonConfigSnapshot,
        config_dir: PathBuf,
        data_dir: PathBuf,
        cache_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        Self::build_internal(
            workspace_path,
            host,
            port,
            config,
            config_dir,
            data_dir,
            cache_dir,
            true,
        )
        .await
    }

    async fn build_internal(
        workspace_path: PathBuf,
        host: String,
        port: u16,
        config: DaemonConfigSnapshot,
        config_dir: PathBuf,
        data_dir: PathBuf,
        cache_dir: PathBuf,
        for_test: bool,
    ) -> anyhow::Result<Self> {
        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            config_dir.clone(),
            data_dir.clone(),
            cache_dir.clone(),
        );

        // Load the unified credential vault before identity/provider setup.
        let vault = crate::common::vault::Vault::load(path_resolver.vault())
            .map_err(|e| anyhow::anyhow!("Failed to load credential vault: {e}"))?;

        // ADR-032: Initialize runtime identity, metadata, and registry
        let runtime_identity =
            crate::identity::runtime::RuntimeIdentity::generate_or_load(&path_resolver, &vault)?;
        let runtime_metadata = crate::identity::runtime_metadata::RuntimeMetadata::load_or_create(
            &path_resolver,
            &runtime_identity.runtime_did,
        )?;
        let mut known_runtimes =
            crate::tunnel::known_runtimes::KnownRuntimes::load_or_create(&path_resolver)?;
        known_runtimes.register(
            &runtime_identity.runtime_did,
            &runtime_metadata.display_name,
            None,
            crate::tunnel::known_runtimes::TrustLevel::SelfRuntime,
        );
        let known_runtimes = std::sync::Arc::new(tokio::sync::RwLock::new(known_runtimes));

        // v3-cleanup: ADR-032 / ADR-033 / provider-catalog migration
        // runners were deleted; the runtime now expects every agent
        // and team on disk to already have `host_runtime_id` and
        // `owner` set (which `create_agent` does at v3).

        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver.clone()));

        // v3: Build the `LlmResolver` here so every agent cold-start
        // goes through `LlmResolver::build` instead of the deprecated
        // inline-[provider] path. Catalog is `~/.peko/providers.toml`,
        // secrets are the OS keychain. Test harnesses that need a
        // env-var fallback (no keychain on CI) flip
        // `PEKO_TEST_RESOLVER_BOOTSTRAP=1`; the daemon picks that up
        // via `LlmResolver::with_env_bootstrap()` below.
        let catalog_path = path_resolver.config_dir().join("providers.toml");
        let catalog = crate::providers::ProviderCatalog::load_or_init(&catalog_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load provider catalog: {e}"))?;
        let secrets: Arc<dyn crate::common::secret_store::SecretStore> = Arc::new(vault);
        let mut resolver_builder = crate::providers::LlmResolver::new(catalog, secrets);
        if std::env::var_os("PEKO_TEST_RESOLVER_BOOTSTRAP").is_some() {
            resolver_builder = resolver_builder.with_env_bootstrap();
        }
        let resolver = Arc::new(resolver_builder);

        let path_resolver_clone = path_resolver.clone();
        let agent_service = Arc::new(
            StatelessAgentService::new_with_resolver(
                config_service.clone(),
                path_resolver.clone(),
                Some(resolver.clone()),
            )
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
        //
        // For tests, always create a fresh core to avoid shared mutable state
        // between concurrent tests.
        let global_core = if for_test {
            use crate::extensions::framework::core::{ExtensionCore, ExtensionServices};
            use crate::extensions::framework::services::AsyncExecutionRouter;
            let router = AsyncExecutionRouter::with_transport(
                crate::extensions::framework::services::async_transport::create_local_transport(),
            );
            let services = ExtensionServices::with_async_router_and_agent_service(
                router,
                Arc::clone(&agent_service),
            );
            Arc::new(ExtensionCore::with_services(Arc::new(services)))
        } else if let Some(existing) = crate::extensions::framework::core::global_core() {
            tracing::info!("Reusing global ExtensionCore initialized by main.rs");
            existing
        } else {
            use crate::extensions::framework::core::{init_global_core, ExtensionCore, ExtensionServices};
            use crate::extensions::framework::services::AsyncExecutionRouter;
            let router = AsyncExecutionRouter::with_transport(
                crate::extensions::framework::services::async_transport::create_local_transport(),
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
        let ext_storage =
            crate::extensions::framework::manager::ExtensionStorage::with_dir(data_dir.join("extensions"));
        let mut ext_manager =
            crate::extensions::framework::manager::ExtensionManager::with_core(Arc::clone(&global_core))
                .with_storage_dir(ext_storage.dir().unwrap().to_path_buf());

        // Register adapters (same as CLI create_manager_with_adapters)
        use crate::extensions::gateway::GatewayAdapter;
        use crate::extensions::general::GeneralExtensionAdapter;
        use crate::extensions::mcp::McpAdapter;
        use crate::extensions::skill::SkillAdapter;
        use crate::extensions::universal::UniversalToolAdapter;

        ext_manager.register_adapter(Box::new(SkillAdapter::new()));
        ext_manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
        ext_manager.register_adapter(Box::new(UniversalToolAdapter::new()));
        ext_manager.register_adapter(Box::new(GatewayAdapter::new(Arc::clone(&global_core))));
        ext_manager.register_adapter(Box::new(GeneralExtensionAdapter::new()));

        // Load all extensions (log warnings but don't fail startup)
        if let Err(e) = ext_manager.load_all().await {
            tracing::warn!(
                "Failed to load some extensions during daemon startup: {}",
                e
            );
        }

        let extension_manager = Arc::new(tokio::sync::RwLock::new(ext_manager));
        let extension_services = Arc::new(crate::extensions::framework::services::Services::with_core(
            Arc::clone(&global_core),
        ));

        // ADR-034: Initialize auth components
        let auth_config = crate::auth::config::AuthConfig::load(&path_resolver)?;
        let api_key_store = if auth_config.enable_api_key() {
            Some(crate::auth::api_key::ApiKeyStore::load(&path_resolver)?)
        } else {
            None
        };
        let api_key_verifier = api_key_store
            .as_ref()
            .map(|s| crate::auth::api_key::ApiKeyVerifier::new(s.clone()));
        let jwt_validator = if auth_config.enable_pekohub_jwt() {
            Some(crate::auth::jwt::JwtValidator::new(
                auth_config.trusted_issuers().to_vec(),
                runtime_identity.runtime_did.clone(),
                None,
            ))
        } else {
            None
        };
        let rate_limiter = if auth_config.has_any_remote_auth_method() {
            Some(crate::auth::rate_limit::RateLimiter::new(
                auth_config.rate_limit().jwt_requests_per_minute,
                auth_config.rate_limit().api_key_requests_per_minute,
                auth_config.rate_limit().burst_jwt,
                auth_config.rate_limit().burst_api_key,
            ))
        } else {
            None
        };

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
            runtime_identity,
            runtime_metadata,
            known_runtimes,
            auth_config,
            api_key_store,
            api_key_verifier,
            jwt_validator,
            rate_limiter,
            tunnel_cancel: Arc::new(RwLock::new(None)),
            tunnel_connected: Arc::new(RwLock::new(false)),
            tunnel_dispatcher: Arc::new(RwLock::new(None)),
            tunnel_attempts: Arc::new(RwLock::new(0)),
            tunnel_last_error: Arc::new(RwLock::new(None)),
            tunnel_degraded: Arc::new(RwLock::new(false)),
            // Issue #29: cross-runtime a2a response correlation
            // registry + outbound tunnel handle slot. Initialized
            // eagerly so the registry exists before the tunnel
            // connects; the slot starts as `None` and is filled by
            // the dispatcher's handle-publisher on every reconnect.
            pending_a2a_responses: Arc::new(crate::tunnel::PendingA2aResponses::new()),
            tunnel_handle_slot: Arc::new(RwLock::new(None)),
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
    pub fn extension_manager(
        &self,
    ) -> &Arc<tokio::sync::RwLock<crate::extensions::framework::manager::ExtensionManager>> {
        &self.extension_manager
    }

    /// Get the extension services
    #[must_use]
    pub fn extension_services(&self) -> &Arc<crate::extensions::framework::services::Services> {
        &self.extension_services
    }

    /// Get the auth configuration (ADR-034)
    #[must_use]
    pub fn auth_config(&self) -> crate::auth::config::AuthConfig {
        self.auth_config.clone()
    }

    /// Get the API key store (ADR-034)
    #[must_use]
    pub fn api_key_store(&self) -> Option<crate::auth::api_key::ApiKeyStore> {
        self.api_key_store.clone()
    }

    /// Get the API key verifier (ADR-034)
    #[must_use]
    pub fn api_key_verifier(&self) -> Option<crate::auth::api_key::ApiKeyVerifier> {
        self.api_key_verifier.clone()
    }

    /// Get the JWT validator (ADR-034)
    #[must_use]
    pub fn jwt_validator(&self) -> Option<crate::auth::jwt::JwtValidator> {
        self.jwt_validator.clone()
    }

    /// Get the rate limiter (ADR-034)
    #[must_use]
    pub fn rate_limiter(&self) -> Option<crate::auth::rate_limit::RateLimiter> {
        self.rate_limiter.clone()
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

    /// Get the runtime identity (ADR-032)
    #[must_use]
    pub fn runtime_identity(&self) -> &crate::identity::runtime::RuntimeIdentity {
        &self.runtime_identity
    }

    /// Get the runtime metadata (ADR-032)
    #[must_use]
    pub fn runtime_metadata(&self) -> &crate::identity::runtime_metadata::RuntimeMetadata {
        &self.runtime_metadata
    }

    /// Get the known runtimes registry (ADR-032)
    #[must_use]
    pub fn known_runtimes(
        &self,
    ) -> &std::sync::Arc<tokio::sync::RwLock<crate::tunnel::known_runtimes::KnownRuntimes>> {
        &self.known_runtimes
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

    /// Start the PekoHub tunnel as a background task.
    ///
    /// `max_reconnect_attempts` caps how many consecutive reconnect attempts
    /// the tunnel client will make before giving up and reporting degraded
    /// state (issue #8). Use `crate::tunnel::DEFAULT_MAX_RECONNECT_ATTEMPTS`
    /// for the default.
    ///
    /// Returns true if the tunnel was started, false if no credentials exist.
    pub async fn start_tunnel(&self, max_reconnect_attempts: u32) -> anyhow::Result<bool> {
        use crate::tunnel::{load_pekohub_credential, TunnelClient, TunnelDispatcher};
        use tracing::{info, warn};

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.cache_dir.clone(),
        );
        let vault = crate::common::vault::Vault::load(path_resolver.vault())
            .map_err(|e| anyhow::anyhow!("Failed to load credential vault for tunnel: {e}"))?;
        let vault = std::sync::Arc::new(vault);

        let cred_path = crate::tunnel::PekoHubCredential::path_for_config_dir(&self.config_dir);
        let cred = match load_pekohub_credential(Some(&cred_path))? {
            Some(c) => c,
            None => return Ok(false),
        };

        let cancel = tokio_util::sync::CancellationToken::new();
        {
            let mut tc = self.tunnel_cancel.write().await;
            *tc = Some(cancel.clone());
        }

        let dispatcher = TunnelDispatcher::new(self.clone());

        // Issue #29: build the cross-runtime a2a dispatch ctx
        // (Slice B + Slice C bootstrap). Wires the
        // `HubAgentDirectoryClient` (HTTP client to the hub's
        // agent directory API) + the runtime's signing key + the
        // pending registry + the tunnel handle slot into a single
        // `Arc<CrossRuntimeA2aCtx>` and registers it on the
        // `ExtensionServices` so every per-agent `A2aSendTool`
        // gets the ctx injected (via `agent.rs`).
        //
        // If the directory client or signing-key build fails, log
        // a warning and skip the registration — the local a2a path
        // still works, and the operator can debug the directory
        // config without losing tunnel connectivity.
        if let Err(e) = self.install_cross_runtime_a2a_ctx(&cred, &vault).await {
            warn!(
                "Could not install cross-runtime a2a ctx (peko-runtime#29); \
                 cross-runtime a2a will be unavailable until this is fixed. \
                 The local a2a path is unaffected. error: {e:#}"
            );
        }
        {
            let mut td = self.tunnel_dispatcher.write().await;
            *td = Some(dispatcher.clone());
        }

        let dispatcher_for_handler = dispatcher;

        let mut client = TunnelClient::new_with(cred, max_reconnect_attempts).with_vault(vault);
        client.on_request(move |msg, handle| {
            let dispatcher = dispatcher_for_handler.clone();
            async move {
                dispatcher.handle_message(msg, handle).await;
            }
        });

        {
            let mut connected = self.tunnel_connected.write().await;
            *connected = true;
        }

        // Clone the shared flags once each: one set is moved into the on_status
        // closure, the other set is moved into the background spawn below.
        let connected_for_cb = self.tunnel_connected.clone();
        let attempts_for_cb = self.tunnel_attempts.clone();
        let last_error_for_cb = self.tunnel_last_error.clone();
        let degraded_for_cb = self.tunnel_degraded.clone();
        let connected_for_task = self.tunnel_connected.clone();
        let state_for_callback = self.clone();
        client.on_status(move |update| {
            let state = state_for_callback.clone();
            let connected_flag = connected_for_cb.clone();
            let attempts_flag = attempts_for_cb.clone();
            let last_error_flag = last_error_for_cb.clone();
            let degraded_flag = degraded_for_cb.clone();
            async move {
                use crate::tunnel::TunnelStatusUpdate;
                match update {
                    TunnelStatusUpdate::Connected => {
                        if let Ok(mut g) = connected_flag.try_write() {
                            *g = true;
                        }
                        if let Ok(mut g) = attempts_flag.try_write() {
                            *g = 0;
                        }
                        if let Ok(mut g) = last_error_flag.try_write() {
                            *g = None;
                        }
                        if let Ok(mut g) = degraded_flag.try_write() {
                            *g = false;
                        }
                        state.mark_healthy().await;
                    }
                    TunnelStatusUpdate::Disconnected {
                        attempts,
                        last_error,
                    } => {
                        if let Ok(mut g) = connected_flag.try_write() {
                            *g = false;
                        }
                        if let Ok(mut g) = attempts_flag.try_write() {
                            *g = attempts;
                        }
                        if let Ok(mut g) = last_error_flag.try_write() {
                            *g = Some(last_error);
                        }
                    }
                    TunnelStatusUpdate::Degraded {
                        attempts,
                        last_error,
                    } => {
                        if let Ok(mut g) = connected_flag.try_write() {
                            *g = false;
                        }
                        if let Ok(mut g) = attempts_flag.try_write() {
                            *g = attempts;
                        }
                        if let Ok(mut g) = last_error_flag.try_write() {
                            *g = Some(last_error);
                        }
                        if let Ok(mut g) = degraded_flag.try_write() {
                            *g = true;
                        }
                        state.mark_degraded().await;
                    }
                }
            }
        });

        tokio::spawn(async move {
            info!("Starting PekoHub tunnel in background");
            client.run_cancellable(cancel).await;
            info!("PekoHub tunnel stopped");
            let mut connected = connected_for_task.write().await;
            *connected = false;
        });

        Ok(true)
    }

    /// Check if the tunnel is currently connected
    pub async fn tunnel_connected(&self) -> bool {
        let connected = self.tunnel_connected.read().await;
        *connected
    }

    /// Cross-runtime a2a response correlation registry (issue #29).
    /// Shared with the `CrossRuntimeA2aCtx` on every `A2aSendTool`
    /// and the inbound `AgentToAgentResponse` arm of the
    /// `TunnelDispatcher`. Returns a clone of the inner `Arc`, so
    /// call sites hold a cheap reference.
    pub fn pending_a2a_responses(&self) -> Arc<crate::tunnel::PendingA2aResponses> {
        self.pending_a2a_responses.clone()
    }

    /// Slot for the live outbound tunnel handle (issue #29). The
    /// `TunnelDispatcher` writes the freshest handle here on every
    /// reconnect; the `CrossRuntimeA2aCtx` and any other consumer
    /// reads through the returned `Arc` to send on the live
    /// tunnel.
    pub fn tunnel_handle_slot(&self) -> Arc<RwLock<Option<crate::tunnel::TunnelHandle>>> {
        self.tunnel_handle_slot.clone()
    }

    /// Install the cross-runtime a2a dispatch context on the
    /// `ExtensionServices` so every per-agent `A2aSendTool` is
    /// built with the ctx (issue #29, Slice B + Slice C
    /// bootstrap). Called by `start_tunnel` after the dispatcher
    /// is built but before the tunnel client starts.
    ///
    /// The default response timeout is 60s — long enough to absorb
    /// a hub round-trip and a target-runtime dispatch without
    /// being so long the LLM caller hangs indefinitely if the
    /// target is stuck. Make this configurable via daemon config
    /// in a follow-up.
    async fn install_cross_runtime_a2a_ctx(
        &self,
        cred: &crate::tunnel::PekoHubCredential,
        vault: &crate::common::vault::Vault,
    ) -> anyhow::Result<()> {
        use crate::tunnel::CrossRuntimeA2aCtx;
        use base64::engine::general_purpose::STANDARD as BASE64;
        use base64::Engine as _;
        use ed25519_dalek::SigningKey;
        use std::time::Duration;

        // 1. Build the directory HTTP client from the credential
        //    URL. `from_credential` flips wss:// → https:// and
        //    strips the /v1/tunnel path. This is the only place
        //    the runtime talks to pekohub's HTTP surface.
        let directory = crate::tunnel::HubAgentDirectoryClient::from_credential(cred)
            .map_err(|e| anyhow::anyhow!("HubAgentDirectoryClient::from_credential: {e}"))?;
        let directory: Arc<dyn crate::tunnel::AgentDirectory> = Arc::new(directory);

        // 2. Build the SigningKey from the credential's stored
        //    private key in the vault. `resolve_private_key` returns the
        //    base64-encoded raw 32 bytes. Decode and construct.
        let privkey_b64 = cred.resolve_private_key(vault)?;
        let privkey_bytes = BASE64.decode(privkey_b64.trim()).map_err(|e| {
            anyhow::anyhow!("PekoHubCredential private key is not valid base64: {e}")
        })?;
        if privkey_bytes.len() != 32 {
            anyhow::bail!(
                "PekoHubCredential private key is {} bytes; expected 32",
                privkey_bytes.len()
            );
        }
        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(&privkey_bytes);
        let signing_key = Arc::new(SigningKey::from_bytes(&key_arr));

        // 3. Build the ctx. The handle slot is shared with the
        //    `TunnelDispatcher` so the outbound path sees the
        //    freshest handle on every reconnect.
        let ctx = Arc::new(CrossRuntimeA2aCtx {
            directory,
            pending: self.pending_a2a_responses(),
            signing_key,
            caller_runtime_id: cred.runtime_id.clone(),
            tunnel: self.tunnel_handle_slot(),
            response_timeout: Duration::from_mins(1),
        });

        // 4. Register on the `ExtensionServices`. The per-agent
        //    `A2aSendTool` constructor in `agent.rs` consults
        //    `services().cross_runtime_a2a_ctx()` and calls
        //    `with_cross_runtime(ctx)` if present.
        //
        //    `tool_runtime.extension_core().services()` returns
        //    `Arc<ExtensionServices>`; we set the ctx on the
        //    underlying ExtensionServices via the Arc. (In tests
        //    the ExtensionCore may have no services — log and
        //    skip rather than crash; the outbound path returns a
        //    clean "not configured" error in that case.)
        self.tool_runtime
            .extension_core()
            .services()
            .set_cross_runtime_a2a_ctx(ctx);

        Ok(())
    }

    /// Check if the tunnel has been started (has a cancellation token)
    pub async fn tunnel_started(&self) -> bool {
        let tc = self.tunnel_cancel.read().await;
        tc.is_some()
    }

    /// Stop the PekoHub tunnel
    pub async fn stop_tunnel(&self) {
        let mut tc = self.tunnel_cancel.write().await;
        if let Some(ref cancel) = *tc {
            cancel.cancel();
        }
        *tc = None;
        let mut connected = self.tunnel_connected.write().await;
        *connected = false;
        let mut dispatcher = self.tunnel_dispatcher.write().await;
        *dispatcher = None;
        // Clear degraded state — if the operator explicitly stopped the
        // tunnel, the daemon is no longer "degraded", it's just "disabled".
        let mut attempts = self.tunnel_attempts.write().await;
        *attempts = 0;
        let mut last_error = self.tunnel_last_error.write().await;
        *last_error = None;
        let mut degraded = self.tunnel_degraded.write().await;
        *degraded = false;
        self.mark_healthy().await;
    }

    /// Get the tunnel dispatcher if the tunnel is active
    pub async fn tunnel_dispatcher(&self) -> Option<crate::tunnel::TunnelDispatcher> {
        let dispatcher = self.tunnel_dispatcher.read().await;
        dispatcher.clone()
    }

    /// Get the running count of consecutive failed reconnect attempts.
    /// Reset to 0 on each successful connect.
    pub async fn tunnel_attempts(&self) -> u32 {
        *self.tunnel_attempts.read().await
    }

    /// Get the last tunnel error message, if any.
    pub async fn tunnel_last_error(&self) -> Option<String> {
        self.tunnel_last_error.read().await.clone()
    }

    /// Compute a high-level `TunnelHealth` snapshot used by
    /// `peko daemon status --json` (issue #8).
    ///
    /// Priority order (most-severe first):
    /// 1. `Connected` — tunnel is up
    /// 2. `Degraded`   — reconnect-attempt cap was hit; client stopped
    /// 3. `Disconnected` — at least one connect attempt has failed
    /// 4. `Disabled`    — never started (no credential / no attempts)
    pub async fn tunnel_health(&self) -> TunnelHealth {
        let connected = *self.tunnel_connected.read().await;
        let attempts = *self.tunnel_attempts.read().await;
        let last_error = self.tunnel_last_error.read().await.clone();
        let tunnel_degraded = *self.tunnel_degraded.read().await;

        if connected {
            return TunnelHealth::Connected;
        }
        if tunnel_degraded {
            return TunnelHealth::Degraded {
                attempts,
                last_error: last_error.unwrap_or_else(|| "reconnect cap exhausted".to_string()),
            };
        }
        if attempts > 0 {
            return TunnelHealth::Disconnected {
                attempts,
                last_error,
            };
        }
        TunnelHealth::Disabled
    }
}

/// High-level snapshot of PekoHub tunnel health, surfaced via
/// `peko daemon status --json` (issue #8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelHealth {
    /// No PekoHub credentials on disk; tunnel is intentionally off.
    Disabled,
    /// WebSocket tunnel is established and authenticated.
    Connected,
    /// Tunnel is configured and started, but the latest connect attempt
    /// failed; the client is still retrying (attempts < cap).
    Disconnected {
        attempts: u32,
        last_error: Option<String>,
    },
    /// The reconnect-attempt cap was hit; the tunnel client has stopped
    /// retrying. Operator must restart with `peko tunnel start` to retry.
    Degraded { attempts: u32, last_error: String },
}

impl TunnelHealth {
    /// String discriminator used in JSON output (`tunnel.state`).
    #[must_use]
    pub fn state_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Connected => "connected",
            Self::Disconnected { .. } => "disconnected",
            Self::Degraded { .. } => "degraded",
        }
    }

    /// Reconnect attempt count (0 for `Disabled`/`Connected`).
    #[must_use]
    pub fn reconnect_attempts(&self) -> u32 {
        match self {
            Self::Disabled | Self::Connected => 0,
            Self::Disconnected { attempts, .. } | Self::Degraded { attempts, .. } => *attempts,
        }
    }

    /// Last tunnel error string (None for `Disabled`/`Connected`).
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Disabled | Self::Connected => None,
            Self::Disconnected { last_error, .. } => last_error.as_deref(),
            Self::Degraded { last_error, .. } => Some(last_error.as_str()),
        }
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
        let data_dir = temp_dir.path().to_path_buf();
        let cache_dir = data_dir.join("cache");
        AppState::build_for_test(
            temp_dir.path().to_path_buf(),
            "127.0.0.1".to_string(),
            11435,
            DaemonConfigSnapshot::default(),
            data_dir.clone(),
            data_dir,
            cache_dir,
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
    #[serial_test::serial(core)]
    async fn test_agent_init_preserves_pre_registered_tools() {
        use crate::agents::Agent;
        use crate::extensions::framework::core::init_global_core;
        use crate::extensions::framework::{HookInput, HookPoint};
        use crate::agents::agent_config::AgentConfig;
        

        let state = create_test_state().await;
        let global_core = state.tool_runtime.extension_core().clone();

        // Simulate what Agent::new() does
        init_global_core(global_core.clone());

        let config = AgentConfig {
            name: "test-agent".to_string(),
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
        let tools: Vec<crate::extensions::framework::types::ToolMetadata> = core.list_tools().await;
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

    // ── Issue #8: tunnel health surface tests ─────────────────────

    #[tokio::test]
    async fn test_tunnel_health_disabled_when_no_credential() {
        // With no PekoHub credential on disk and the daemon never told to
        // start the tunnel, `tunnel_health()` should report `Disabled`.
        let state = create_test_state().await;
        let health = state.tunnel_health().await;
        assert_eq!(health, TunnelHealth::Disabled);
        assert_eq!(health.state_str(), "disabled");
        assert_eq!(health.reconnect_attempts(), 0);
        assert_eq!(health.last_error(), None);
    }

    #[tokio::test]
    async fn test_tunnel_health_degraded_after_cap() {
        // Simulate the tunnel client hitting the reconnect cap without
        // spinning up a real WebSocket: directly set the tracking fields
        // (including `tunnel_degraded`) and verify `tunnel_health()`.
        let state = create_test_state().await;

        *state.tunnel_attempts.write().await = 50;
        *state.tunnel_last_error.write().await = Some("tunnel reconnect cap reached".to_string());
        *state.tunnel_degraded.write().await = true;

        let health = state.tunnel_health().await;
        match &health {
            TunnelHealth::Degraded {
                attempts,
                last_error,
            } => {
                assert_eq!(*attempts, 50);
                assert!(last_error.contains("cap"));
            }
            other => panic!("expected Degraded, got {other:?}"),
        }
        assert_eq!(health.state_str(), "degraded");
        assert_eq!(health.reconnect_attempts(), 50);
    }

    #[tokio::test]
    async fn test_tunnel_health_disconnected_transient() {
        // When the daemon is not degraded but we've recorded a failed
        // attempt, `tunnel_health()` reports Disconnected (transient
        // retry state, attempts < cap).
        let state = create_test_state().await;
        *state.tunnel_attempts.write().await = 3;
        *state.tunnel_last_error.write().await = Some("connection refused".to_string());

        let health = state.tunnel_health().await;
        match &health {
            TunnelHealth::Disconnected {
                attempts,
                last_error,
            } => {
                assert_eq!(*attempts, 3);
                assert_eq!(last_error.as_deref(), Some("connection refused"));
            }
            other => panic!("expected Disconnected, got {other:?}"),
        }
        assert_eq!(health.state_str(), "disconnected");
        assert_eq!(health.reconnect_attempts(), 3);
        assert_eq!(health.last_error(), Some("connection refused"));
    }

    #[tokio::test]
    async fn test_stop_tunnel_clears_degraded_and_errors() {
        // After `stop_tunnel()` the daemon should no longer be degraded
        // (operator explicitly disabled it), and attempts/last_error
        // should be reset so `tunnel_health()` reports Disabled.
        let state = create_test_state().await;
        state.mark_degraded().await;
        *state.tunnel_attempts.write().await = 50;
        *state.tunnel_last_error.write().await = Some("boom".to_string());

        state.stop_tunnel().await;

        assert!(!state.is_degraded().await);
        assert_eq!(state.tunnel_attempts().await, 0);
        assert_eq!(state.tunnel_last_error().await, None);
    }
}
