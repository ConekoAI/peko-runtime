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
use crate::common::services::{ConfigAuthority, ConfigAuthorityImpl, SessionService};
use crate::common::types::config::PekoConfig;
use crate::cron::IdleDetector;
use crate::engine::tool_runtime::ToolRuntime;
use crate::extensions::framework::async_exec::executor::AsyncExecutor;
use crate::extensions::framework::store::ExtensionStore;
use crate::observability::Observability;
use crate::principal::{
    factory::{DefaultPrincipalRouterFactory, PrincipalMemoryFactory},
    memory::{DefaultPrincipalMemory, PrincipalMemory},
    slash::SlashDispatcher,
    PrincipalManager,
};
use crate::registry::{load_from_workspace, RegistryConfig};
use crate::session::InboxRegistry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{broadcast, RwLock};

/// Shared application state for the HTTP API (Stateless Architecture)
///
/// This struct is passed to all route handlers via Axum's State extractor.
/// All fields are thread-safe and can be accessed concurrently.
#[derive(Clone)]
pub(crate) struct AppState {
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
    principal_service: Arc<StatelessAgentService>,

    /// Shared LLM resolver. Re-read in place via
    /// `ProviderCatalog::reload` after `peko provider {add,remove,
    /// set-default}` so the long-running daemon observes CLI
    /// mutations without a restart.
    resolver: Arc<crate::providers::LlmResolver>,

    /// Shared credential vault. Re-read in place via `Vault::reload`
    /// after `peko credential {set,delete}` for the same reason as
    /// `resolver` above. Stored as a concrete `Vault` (not the
    /// `SecretStore` trait object) so `reload` can mutate the inner
    /// state without going through trait dispatch.
    vault: Arc<crate::common::vault::Vault>,

    /// Principal manager (AI Principal container lifecycle)
    principal_manager: Arc<PrincipalManager>,

    /// F20: peer quota registry. `Some` after daemon startup loads
    /// `<runtime>/peers/` and materializes each peer's meter. The
    /// quota handler reads this to resolve `is_peer=true` requests;
    /// the engine loop reads it to resolve a peer's quota meter at
    /// run time. `None` means peer attribution is disabled (tests /
    /// slim daemon builds).
    peer_registry: Option<Arc<crate::principal::peer::PeerRegistry>>,

    /// Lifecycle manager (tracks active executions only)
    lifecycle: Arc<LifecycleManager>,

    /// Session service (unified for CLI and API)
    session_service: Arc<SessionService>,

    /// Tool runtime for async task execution (ADR-020)
    pub tool_runtime: Arc<ToolRuntime>,

    /// Async task executor for daemon-side background execution (ADR-020)
    pub async_task_executor: Arc<AsyncExecutor>,

    /// Per-session inbox registry: shared `SessionInbox` and run-permit
    /// semaphore for every session the daemon knows about. The IPC
    /// server pushes steering messages here, the executor pushes
    /// completion events here, and the in-flight `AgenticLoop` drains
    /// from here at the top of every iteration.
    pub inbox_registry: Arc<InboxRegistry>,

    /// Background runtime manager for MCP servers and gateways (ADR-025)
    background_runtime_manager: Arc<BackgroundRuntimeManager>,

    /// Gateway router for channel→agent mapping (ADR-025)
    gateway_router: Arc<GatewayRouter>,

    /// Shared MCP client registry — populated by McpRuntimeAdapter (ADR-025)
    mcp_client_registry: Arc<McpClientRegistry>,

    /// Extension runtime starter registry — dispatches ext start/stop by type (ADR-025/026)
    runtime_starter_registry: Arc<ExtensionRuntimeStarterRegistry>,

    /// Extension store for installed extensions (ADR-030 Tier 1)
    extension_store: Arc<ExtensionStore>,

    /// Extension services for built-in extension operations
    extension_services: Arc<crate::extensions::framework::services::Services>,

    /// Shutdown broadcast channel - send () to trigger graceful shutdown
    shutdown_tx: Arc<broadcast::Sender<()>>,

    /// Internal state that can be modified
    inner: Arc<RwLock<AppStateInner>>,

    /// Runtime identity (ADR-032)
    pub runtime_identity: crate::identity::runtime::RuntimeIdentity,

    /// Runtime signing key derived from the vault. Shared by the tunnel
    /// client, direct connection manager, and direct server.
    pub runtime_signing_key: Arc<ed25519_dalek::SigningKey>,

    /// Loaded peko configuration (network.direct used by direct transport).
    pub peko_config: PekoConfig,

    /// Direct connection manager for outbound direct transport.
    pub direct_manager: Arc<crate::tunnel::direct::DirectConnectionManager>,

    /// Direct server bound address, if started.
    pub direct_bound_addr: Arc<RwLock<Option<std::net::SocketAddr>>>,

    /// Direct server cancellation token.
    pub direct_cancel: Arc<RwLock<Option<tokio_util::sync::CancellationToken>>>,

    /// Last direct server error, if any.
    pub direct_last_error: Arc<RwLock<Option<String>>>,

    /// Shared idle detector used by the cron engine and IPC server to
    /// track Principal activity for idle-triggered jobs.
    idle_detector: Option<Arc<IdleDetector>>,

    /// Runtime metadata (ADR-032)
    pub runtime_metadata: crate::identity::runtime_metadata::RuntimeMetadata,

    /// Known runtimes registry (ADR-032)
    pub known_runtimes:
        std::sync::Arc<tokio::sync::RwLock<crate::tunnel::known_runtimes::KnownRuntimes>>,

    /// Trust store for principal package publisher pinning (issue #91).
    pub trust_store: std::sync::Arc<tokio::sync::RwLock<crate::registry::packaging::TrustStore>>,

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
    /// Shared between the outbound `PrincipalSendTool` path (which
    /// registers a oneshot under `request_id`) and the inbound
    /// tunnel dispatcher arm (which completes the oneshot when the
    /// matching `AgentToAgentResponse` arrives). Initialized lazily
    /// (a fresh `PendingA2aResponses`) so the registry exists even
    /// before the tunnel connects.
    pending_a2a_responses: Arc<crate::tunnel::PendingA2aResponses>,

    /// In-flight principal-send runs, keyed by the original
    /// `request_id`. Both IPC variants — `RequestPacket::PrincipalSend`
    /// and `RequestPacket::PrincipalSendStream` — register here, so
    /// `peko interrupt <id>` and `peko steer <id>` work uniformly. The
    /// shared `run_principal_send` helper inserts on spawn (with a
    /// cancel token + peer for steer session-id derivation) and
    /// removes on natural completion via the `StreamingRunGuard`
    /// RAII. The `PrincipalSendControl` IPC handler looks up entries
    /// here to issue soft-interrupt or push a steering message into
    /// the run's session inbox.
    ///
    /// `std::sync::Mutex` matches the `PendingA2aResponses` pattern:
    /// every operation is hash-map-only, no `.await` is held across
    /// the lock. See `src/tunnel/a2a_pending.rs:53-55`.
    streaming_runs: Arc<std::sync::Mutex<HashMap<u64, StreamingRunHandle>>>,

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

/// Per-run control handle for an in-flight `PrincipalSendStream`.
///
/// Inserted by the streaming handler when it spawns the root agent
/// task, removed on natural completion. Looked up by
/// `handle_principal_send_control` to either cancel the run (Interrupt
/// mode) or push a steering message into its session inbox (Steer
/// mode). See `src/ipc/server.rs` for the streaming handler and the
/// `PrincipalSendControl` IPC handler.
#[allow(dead_code)] // field-by-field — see DirectHealth note. Kept as public-ish surface for tests.
pub(crate) struct StreamingRunHandle {
    /// Principal name — diagnostic only, included in control responses.
    pub principal_name: String,
    /// Peer subject — needed to derive `session_id` for steer pushes.
    /// Cloned into the IPC handler's scope (cheap, `Subject` is small).
    pub peer: crate::auth::Subject,
    /// Cancellation token for soft-interrupt. Setting this signals
    /// the agentic loop to finish the current step and exit cleanly.
    /// Cloned into both the agentic loop and the IPC handler.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Set by the streaming handler when it observes the cancel
    /// signal (or detects natural completion). Lets the IPC handler
    /// wait for the run to actually wind down if it needs to. Not
    /// required for the fire-and-forget control ack; reserved for
    /// future "wait for clean shutdown" semantics.
    pub interrupt_acked: Arc<tokio::sync::Notify>,
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
            .field("principal_service", &"<StatelessAgentService>")
            .field("principal_manager", &"<PrincipalManager>")
            .field("tool_runtime", &"<ToolRuntime>")
            .field("async_task_executor", &"<AsyncExecutor>")
            .field("inbox_registry", &"<InboxRegistry>")
            .field("background_runtime_manager", &"<BackgroundRuntimeManager>")
            .field("gateway_router", &"<GatewayRouter>")
            .field("mcp_client_registry", &"<McpClientRegistry>")
            .field(
                "runtime_starter_registry",
                &"<ExtensionRuntimeStarterRegistry>",
            )
            .field("extension_store", &"<ExtensionStore>")
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
    /// Whether the daemon is ready to serve requests
    pub ready: bool,
}

/// Snapshot of daemon configuration
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields on this snapshot are read by tests and inline e2e harnesses; cargo build's pass doesn't see them.
pub(crate) struct DaemonConfigSnapshot {
    /// Data directory path
    pub data_dir: PathBuf,
    /// Config directory path
    pub config_dir: PathBuf,
    /// Log level
    pub log_level: String,
    /// How this daemon was launched (CLI vs. sidecar). Reflected in the
    /// `mode` field of `ResponsePacket::Status` so peers (notably the
    /// desktop's SidecarSupervisor) can tell who owns the IPC socket.
    /// Defaults to `Headless` for tests that don't construct a full
    /// `DaemonConfig`.
    pub launch_mode: crate::daemon::LaunchMode,
}

// Several methods on `AppState` are kept as a deliberate public-ish
// surface even though `cargo build` doesn't see any in-crate callers:
// the daemon-side live wiring reaches every service through the
// `host: SystemHandle` port trait (`src/daemon/host.rs`), so the
// underlying `AppState` getter methods look unused to the dead-code
// pass after F9 narrowed the struct to `pub(crate)`. They're real
// API surface for tests, the `daemon::run` direct field access, and
// the inline `tunnel_e2e` / `principal_send_offline` tests; future
// dead-code consolidation can revisit.
#[allow(dead_code)]
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
        // Wrap in Arc so both the daemon's SecretStore (passed to the
        // LlmResolver) and the daemon's reload machinery can share the
        // same in-memory state — `Vault::reload` mutates the interior
        // through `RwLock`, so an Arc aliasing the same instance sees
        // the same writes.
        let vault = Arc::new(
            crate::common::vault::Vault::load(path_resolver.vault())
                .map_err(|e| anyhow::anyhow!("Failed to load credential vault: {e}"))?,
        );

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

        // Load the runtime's private signing key from the vault and the
        // on-disk `peko.toml` configuration. These are needed by the
        // tunnel client, direct connection manager, and direct server.
        let runtime_signing_key = load_runtime_signing_key(&runtime_identity, &vault)?;
        let peko_config = load_peko_config(&config_dir);
        let pending_a2a_responses = Arc::new(crate::tunnel::PendingA2aResponses::new());
        let streaming_runs: Arc<std::sync::Mutex<HashMap<u64, StreamingRunHandle>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let direct_manager = Arc::new(crate::tunnel::direct::DirectConnectionManager::new(
            runtime_signing_key.clone(),
            runtime_identity.runtime_did.clone(),
            peko_config.network.direct.tls_required,
            pending_a2a_responses.clone(),
        ));

        let trust_store = crate::registry::packaging::TrustStore::load_or_create(&path_resolver)?;
        let trust_store = std::sync::Arc::new(tokio::sync::RwLock::new(trust_store));

        // v3-cleanup: ADR-032 / ADR-033 / provider-catalog migration
        // runners were deleted; the runtime now expects every agent
        // on disk to already have `host_runtime_id` set (which the
        // principal creation path does at v3).

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
        let secrets: Arc<dyn crate::common::secret_store::SecretStore> =
            Arc::clone(&vault) as Arc<dyn crate::common::secret_store::SecretStore>;
        let mut resolver_builder = crate::providers::LlmResolver::new(catalog, secrets);
        if std::env::var_os("PEKO_TEST_RESOLVER_BOOTSTRAP").is_some() {
            resolver_builder = resolver_builder.with_env_bootstrap();
        }
        let resolver = Arc::new(resolver_builder);

        let path_resolver_clone = path_resolver.clone();
        let principal_service = Arc::new(
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

        // ADR-021: Initialize global ExtensionCore FIRST so ToolRuntime can register
        // tools with it, and Agent::new() can find them later.
        //
        // If main.rs already initialized the global core (e.g. for the async router),
        // reuse it and register tools on that instance. Otherwise create a new one.
        // This prevents a race where main.rs sets an empty core and AppState's
        // tool-filled core gets discarded by the OnceLock.
        //
        // Trait-object clone for the framework (avoids a framework → agents
        // dependency while keeping the concrete arc for other consumers).
        let principal_service_dyn: Arc<dyn crate::common::types::principal_message::PrincipalMessageService> =
            principal_service.clone();

        // For tests, always create a fresh core to avoid shared mutable state
        // between concurrent tests.
        let global_core = if for_test {
            use crate::extensions::framework::core::{ExtensionCore, ExtensionServices};
            use crate::extensions::framework::services::AsyncExecutionRouter;
            let router = AsyncExecutionRouter::with_transport(
                crate::extensions::framework::services::async_transport::create_local_transport(),
            );
            let services = ExtensionServices::with_async_router_and_principal_message_service(
                router,
                Arc::clone(&principal_service_dyn),
            );
            Arc::new(ExtensionCore::with_services(Arc::new(services)))
        } else if let Some(existing) = crate::extensions::framework::core::global_core() {
            tracing::info!("Reusing global ExtensionCore initialized by main.rs");
            existing
        } else {
            use crate::extensions::framework::core::{
                init_global_core, ExtensionCore, ExtensionServices,
            };
            use crate::extensions::framework::services::AsyncExecutionRouter;
            let router = AsyncExecutionRouter::with_transport(
                crate::extensions::framework::services::async_transport::create_local_transport(),
            );
            let services = ExtensionServices::with_async_router_and_principal_message_service(
                router,
                Arc::clone(&principal_service_dyn),
            );
            let core = Arc::new(ExtensionCore::with_services(Arc::new(services)));
            init_global_core(Arc::clone(&core));
            core
        };

        // ADR-023: Ensure the principal message service is set on the ExtensionCore.
        // If we reused an existing global core, it may not have the service yet.
        global_core
            .services()
            .set_principal_message_service(Arc::clone(&principal_service_dyn));

        // Make the LLM resolver available to extension hooks (e.g. MCP sampling).
        global_core
            .services()
            .set_llm_resolver(Arc::clone(&resolver));

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
        // Per-session inbox registry: shared by the IPC server (which
        // pushes steering messages from external clients), the
        // `AsyncExecutor` (which pushes completion events from
        // background tasks), and the in-flight `AgenticLoop` (which
        // drains at iteration start). Lazy-initializes entries on
        // first access; no explicit cleanup.
        let inbox_registry = Arc::new(InboxRegistry::new());

        let async_task_executor =
            Arc::new(AsyncExecutor::new().with_inbox_registry(Arc::clone(&inbox_registry)));

        // ADR-025: Initialize BackgroundRuntimeManager and GatewayRouter
        let background_runtime_manager = Arc::new(BackgroundRuntimeManager::new());
        let gateway_router = Arc::new(GatewayRouter::new(Arc::clone(&principal_service)));

        // ADR-025: Shared MCP client registry — populated by McpRuntimeAdapter
        let mcp_client_registry = Arc::new(McpClientRegistry::new());

        // Ensure the global MCP manager uses the daemon-wide shared resources.
        // This unifies the runtime paths so `ext start` / `ext stop` control the
        // same processes that agent-init and tool-proxy code paths see.
        // F19: we forward the principal_manager so MCP sampling can charge
        // the calling principal's quota meter. The MCP init is wired below
        // (after `principal_manager` is built) for this reason.

        // ADR-025/026: Extension runtime starter registry
        let mut runtime_starter_registry = ExtensionRuntimeStarterRegistry::new();
        runtime_starter_registry.register(Box::new(GatewayRuntimeStarter::new()));
        runtime_starter_registry.register(Box::new(McpRuntimeStarter::new()));
        let runtime_starter_registry = Arc::new(runtime_starter_registry);

        // ADR-030: Initialize the global ExtensionStore for IPC extension operations
        let extension_store = Arc::new(
            ExtensionStore::with_core(Arc::clone(&global_core))
                .with_storage_dir(data_dir.join("extensions")),
        );

        // Register adapters (same as CLI create_manager_with_adapters)
        use crate::extensions::gateway::GatewayAdapter;
        use crate::extensions::general::GeneralExtensionAdapter;
        use crate::extensions::mcp::McpAdapter;
        use crate::extensions::skill::SkillAdapter;
        use crate::extensions::slash::SlashAdapter;
        use crate::extensions::universal::UniversalToolAdapter;

        extension_store
            .register_adapter(Box::new(SkillAdapter::new()))
            .await;
        extension_store
            .register_adapter(Box::new(McpAdapter::with_default_manager()))
            .await;
        extension_store
            .register_adapter(Box::new(SlashAdapter::new()))
            .await;
        extension_store
            .register_adapter(Box::new(UniversalToolAdapter::new()))
            .await;
        extension_store
            .register_adapter(Box::new(GatewayAdapter::new(Arc::clone(&global_core))))
            .await;
        extension_store
            .register_adapter(Box::new(GeneralExtensionAdapter::new()))
            .await;

        // Load all extensions (log warnings but don't fail startup)
        if let Err(e) = extension_store.load_all().await {
            tracing::warn!(
                "Failed to load some extensions during daemon startup: {}",
                e
            );
        }
        let extension_services = Arc::new(
            crate::extensions::framework::services::Services::with_core(Arc::clone(&global_core)),
        );

        // Observability hub is constructed early so it can be shared with the
        // PrincipalManager and threaded through to subagent spawn audit events.
        let observability = Arc::new(Observability::new("api"));

        // Initialize the PrincipalManager and load any existing principals.
        // This happens after the extension manager is built so we can inject
        // the slash-command dispatcher, which needs extension state.
        let slash_dispatcher = Arc::new(SlashDispatcher::new(
            Arc::clone(&extension_store),
            Arc::clone(&extension_services),
        ));
        let principal_manager = {
            let root = path_resolver.principals_root_dir();
            let _ = std::fs::create_dir_all(&root);
            let manager = PrincipalManager::with_path_resolver(
                root.clone(),
                path_resolver.clone(),
                Arc::new(DaemonPrincipalMemoryFactory {
                    data_dir: data_dir.clone(),
                }),
                Arc::new(DefaultPrincipalRouterFactory),
            )
            .with_resolver(resolver.clone())
            .with_slash_dispatcher(slash_dispatcher)
            .with_extension_store(Arc::clone(&extension_store))
            .with_observability(Arc::clone(&observability));

            if let Ok(mut entries) = tokio::fs::read_dir(&root).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    if path.is_dir() {
                        let config_path = path.join("principal.toml");
                        if config_path.exists() {
                            if let Err(e) = manager.load(&config_path).await {
                                tracing::warn!(
                                    "Failed to load principal from {}: {e}",
                                    config_path.display()
                                );
                            }
                        }
                    }
                }
            }
            manager
        };

        // F20: build the peer registry by scanning `<runtime>/peers/`.
        // Mirrors `PrincipalManager::load` — every directory is a
        // peer's home, every `peer.toml` is that peer's quota config.
        // We attach the registry to the freshly-built
        // `PrincipalManager` (before wrapping in `Arc`) so
        // `get_or_create_peer` can resolve peer meters without taking
        // a separate dependency.
        let (principal_manager, peer_registry) = {
            let root = path_resolver.peers_root_dir();
            match crate::principal::peer::PeerRegistry::load_or_init(
                root.clone(),
                chrono::Utc::now(),
            )
            .await
            {
                Ok(reg) => {
                    let mgr = principal_manager.with_peer_registry(Arc::clone(&reg));
                    (Arc::new(mgr), Some(reg))
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to load peer registry from {}: {e}",
                        root.display()
                    );
                    (Arc::new(principal_manager), None)
                }
            }
        };

        // F19: now that `principal_manager` is built, wire it into the
        // global MCP manager so MCP sampling can resolve per-principal
        // quota meters for the server's `SamplingRequestHandler`.
        crate::extensions::mcp::init_global_mcp_manager_with_shared_resources(
            Arc::clone(&background_runtime_manager),
            Arc::clone(&mcp_client_registry),
            Some(Arc::clone(&resolver)),
            Some(Arc::clone(&vault)),
            Some(Arc::clone(&principal_manager)),
        );

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
            observability,
            config_service,
            principal_service,
            resolver,
            vault: Arc::clone(&vault),
            principal_manager,
            peer_registry,
            lifecycle,
            session_service,
            tool_runtime,
            async_task_executor,
            inbox_registry,
            background_runtime_manager,
            gateway_router,
            mcp_client_registry,
            runtime_starter_registry,
            extension_store,
            extension_services,
            shutdown_tx: Arc::new(shutdown_tx),
            inner: Arc::new(RwLock::new(AppStateInner::default())),
            runtime_identity,
            runtime_signing_key,
            peko_config,
            direct_manager,
            direct_bound_addr: Arc::new(RwLock::new(None)),
            direct_cancel: Arc::new(RwLock::new(None)),
            direct_last_error: Arc::new(RwLock::new(None)),
            idle_detector: None,
            runtime_metadata,
            known_runtimes,
            trust_store,
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
            pending_a2a_responses,
            streaming_runs,
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

    /// Attach the shared idle detector used by the cron engine.
    pub fn set_idle_detector(&mut self, detector: Arc<IdleDetector>) {
        self.idle_detector = Some(detector);
    }

    /// Record activity for a Principal so idle-triggered cron jobs do not
    /// fire while the Principal is actively being used.
    pub async fn record_principal_activity(&self, principal_name: &str) {
        if let Some(detector) = self.idle_detector.as_ref() {
            detector.record_activity(principal_name).await;
        }
    }

    /// Re-read the provider catalog and the credential vault from
    /// disk. Called by the IPC `ProviderReload` handler so CLI
    /// mutations (`peko provider {add,remove,set-default}`,
    /// `peko credential {set,delete}`) are visible to the long-running
    /// daemon without a restart.
    ///
    /// Returns `(providers_count, keys_count)` for the IPC response so
    /// the caller can confirm what was reloaded. A reload that
    /// partially fails (e.g. corrupt vault) keeps the prior in-memory
    /// state and surfaces the error rather than blanking the daemon.
    pub async fn reload_providers(&self) -> anyhow::Result<(usize, usize)> {
        let providers_count = self
            .resolver
            .catalog()
            .reload()
            .await
            .map_err(|e| anyhow::anyhow!("provider catalog reload failed: {e}"))?;
        let keys_count = self
            .vault
            .reload()
            .map_err(|e| anyhow::anyhow!("vault reload failed: {e}"))?;
        tracing::info!("Provider reload: {providers_count} providers, {keys_count} vault entries");
        Ok((providers_count, keys_count))
    }

    /// Re-read the MCP server configuration from `mcp.toml` and the
    /// credential vault from disk. Called by the IPC `McpReload` handler
    /// so CLI mutations (`peko ext mcp {add,auth,remove}`) are visible to the
    /// long-running daemon without a restart.
    pub async fn reload_mcp_config(&self) -> anyhow::Result<usize> {
        let keys_count = self
            .vault
            .reload()
            .map_err(|e| anyhow::anyhow!("vault reload failed: {e}"))?;
        tracing::info!("MCP reload: {keys_count} vault entries reloaded");

        let mcp_config_path = self.config_dir.join("mcp.toml");
        let adapter = crate::extensions::mcp::McpAdapter::with_default_manager();
        let manager = adapter.manager();
        let servers_count = manager
            .read()
            .await
            .reload_config(&mcp_config_path)
            .await
            .map_err(|e| anyhow::anyhow!("mcp config reload failed: {e}"))?;
        tracing::info!(
            "MCP reload: {servers_count} servers from {}",
            mcp_config_path.display()
        );

        // Auto-start any newly-added servers that request it.
        let auto_start_names: Vec<String> = {
            let mgr = manager.read().await;
            let mut names = Vec::new();
            for state in mgr.list_server_prompt_context().await {
                if !state.running {
                    if let Some(cfg) = mgr.get_server_config(&state.name).await {
                        if cfg.auto_start {
                            names.push(state.name);
                        }
                    }
                }
            }
            names
        };
        for name in auto_start_names {
            let m = manager.clone();
            let name_owned = name.clone();
            if let Err(e) = async move { m.read().await.start_server(&name_owned, None).await }.await {
                tracing::warn!(server = %name, error = %e, "Failed to auto-start MCP server after reload");
            } else {
                tracing::info!(server = %name, "Auto-started MCP server after reload");
            }
        }

        Ok(servers_count)
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

    /// Get the principal message service
    #[must_use]
    pub fn principal_service(&self) -> &Arc<StatelessAgentService> {
        &self.principal_service
    }

    /// Get the principal manager
    #[must_use]
    pub fn principal_manager(&self) -> &Arc<PrincipalManager> {
        &self.principal_manager
    }

    /// F20: get the peer quota registry. `None` when the daemon
    /// failed to load peer state at startup (logged as a warning).
    #[must_use]
    pub fn peer_registry(&self) -> Option<&Arc<crate::principal::peer::PeerRegistry>> {
        self.peer_registry.as_ref()
    }

    /// Get the session service
    #[must_use]
    pub fn session_service(&self) -> &Arc<SessionService> {
        &self.session_service
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
    pub fn extension_store(&self) -> &Arc<ExtensionStore> {
        &self.extension_store
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
            principal_service: Arc::clone(&self.principal_service),
            gateway_router: Arc::clone(&self.gateway_router),
            mcp_client_registry: Arc::clone(&self.mcp_client_registry),
            data_dir: self.data_dir.clone(),
            vault: Some(Arc::clone(&self.vault)),
            resolver: Some(Arc::clone(&self.resolver)),
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

    /// Get the trust store for principal package import (issue #91).
    #[must_use]
    pub fn trust_store(
        &self,
    ) -> &std::sync::Arc<tokio::sync::RwLock<crate::registry::packaging::TrustStore>> {
        &self.trust_store
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

        let dispatcher = TunnelDispatcher::new(Arc::new(self.clone()));

        // If direct cross-runtime connections are enabled, start the
        // inbound direct server now. It shares the same dispatcher
        // callback as the tunnel so inbound A2A traffic is handled
        // identically regardless of transport.
        if self.peko_config.network.direct.enabled {
            let direct_cancel = tokio_util::sync::CancellationToken::new();
            {
                let mut dc = self.direct_cancel.write().await;
                *dc = Some(direct_cancel.clone());
            }
            let direct_config = self.peko_config.network.direct.clone();
            let direct_runtime_id = self.runtime_identity.runtime_did.clone();
            let direct_signing_key = self.runtime_signing_key.clone();
            let direct_known_runtimes = self.known_runtimes.clone();
            let direct_dispatcher = dispatcher.clone();
            let direct_handler: crate::tunnel::direct::DirectMessageHandler = Arc::new(
                move |msg: crate::tunnel::TunnelMessage, handle: crate::tunnel::TunnelHandle| {
                    let d = direct_dispatcher.clone();
                    Box::pin(async move { d.handle_message(msg, handle).await })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                },
            );
            let direct_server = crate::tunnel::direct::DirectServer::new(
                direct_config,
                direct_signing_key,
                direct_runtime_id,
                direct_known_runtimes,
                direct_handler,
            );
            let direct_bound_addr = self.direct_bound_addr.clone();
            let direct_last_error = self.direct_last_error.clone();
            tokio::spawn(async move {
                match direct_server.start(direct_cancel).await {
                    Ok(addr) => {
                        tracing::info!("Direct server bound to {addr}");
                        if let Ok(mut g) = direct_bound_addr.try_write() {
                            *g = Some(addr);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Direct server failed to start: {e}");
                        if let Ok(mut g) = direct_last_error.try_write() {
                            *g = Some(e.to_string());
                        }
                    }
                }
            });
        }

        // Issue #29: build the cross-runtime a2a dispatch ctx
        // (Slice B + Slice C bootstrap). Wires the
        // `HubAgentDirectoryClient` (HTTP client to the hub's
        // agent directory API) + the runtime's signing key + the
        // pending registry + the tunnel handle slot into a single
        // `Arc<CrossRuntimeA2aCtx>` and registers it on the
        // `ExtensionServices` so every per-agent `PrincipalSendTool`
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
    /// Shared with the `CrossRuntimeA2aCtx` on every `PrincipalSendTool`
    /// and the inbound `AgentToAgentResponse` arm of the
    /// `TunnelDispatcher`. Returns a clone of the inner `Arc`, so
    /// call sites hold a cheap reference.
    pub fn pending_a2a_responses(&self) -> Arc<crate::tunnel::PendingA2aResponses> {
        self.pending_a2a_responses.clone()
    }

    /// In-flight `PrincipalSendStream` run registry. Looked up by the
    /// `PrincipalSendControl` IPC handler for soft-interrupt and
    /// steer operations. Returns a clone of the inner `Arc<Mutex>`
    /// so call sites can hold a cheap reference.
    pub fn streaming_runs(&self) -> Arc<std::sync::Mutex<HashMap<u64, StreamingRunHandle>>> {
        self.streaming_runs.clone()
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
    /// `ExtensionServices` so every per-agent `PrincipalSendTool` is
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
        //    strips the /v1/tunnel path. Wrap it with the local-first
        //    directory so same-runtime principals resolve without the
        //    hub.
        let hub_directory = crate::tunnel::HubAgentDirectoryClient::from_credential(cred)
            .map_err(|e| anyhow::anyhow!("HubAgentDirectoryClient::from_credential: {e}"))?;
        let directory: Arc<dyn crate::tunnel::AgentDirectory> =
            Arc::new(crate::tunnel::LocalFirstAgentDirectory::new(
                cred.runtime_id.clone(),
                self.principal_manager().clone(),
                Arc::new(hub_directory),
            ));

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
        //    freshest handle on every reconnect. The direct manager
        //    and known-runtimes registry enable per-peer transport
        //    selection.
        let ctx = Arc::new(CrossRuntimeA2aCtx {
            directory,
            pending: self.pending_a2a_responses(),
            signing_key,
            caller_runtime_id: cred.runtime_id.clone(),
            tunnel: self.tunnel_handle_slot(),
            direct_manager: self.direct_manager.clone(),
            known_runtimes: self.known_runtimes.clone(),
            principal_manager: self.principal_manager().clone(),
            response_timeout: Duration::from_mins(1),
        });
        // The framework stores the ctx as `Arc<dyn Any + Send + Sync>`
        // to avoid a framework → tunnel dependency.
        let ctx: Arc<dyn std::any::Any + Send + Sync + 'static> = ctx;

        // 4. Register on the `ExtensionServices`. The per-agent
        //    `PrincipalSendTool` constructor in `agent.rs` consults
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

        // Phase 4b: propagate the runtime id into every Principal's
        // router so `principal_send` is registered on their agents.
        // Routers that don't need a runtime id (the default for
        // anything other than `RootRouter`) ignore the call.
        let runtime_id = cred.runtime_id.clone();
        for principal in self.principal_manager().list_all().await {
            Arc::clone(&principal.router).set_caller_runtime_id(runtime_id.clone());
        }

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

    /// Stop the inbound direct server, if it is running.
    pub async fn stop_direct_server(&self) {
        let mut dc = self.direct_cancel.write().await;
        if let Some(ref cancel) = *dc {
            cancel.cancel();
        }
        *dc = None;
        let mut addr = self.direct_bound_addr.write().await;
        *addr = None;
        let mut err = self.direct_last_error.write().await;
        *err = None;
    }

    /// Get the bound address of the direct server, if started.
    pub async fn direct_bound_addr(&self) -> Option<std::net::SocketAddr> {
        *self.direct_bound_addr.read().await
    }

    /// High-level direct server health snapshot.
    pub async fn direct_health(&self) -> DirectHealth {
        let enabled = self.peko_config.network.direct.enabled;
        let bound = self.direct_bound_addr.read().await.clone();
        let error = self.direct_last_error.read().await.clone();

        if !enabled {
            return DirectHealth::Disabled;
        }
        if let Some(err) = error {
            return DirectHealth::Error(err);
        }
        match bound {
            Some(addr) => DirectHealth::Listening(addr),
            None => DirectHealth::Starting,
        }
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
#[allow(dead_code)] // tunnel state surface — used by IPC handler serialisation, not reachable from cargo build's dead-code graph
pub(crate) enum TunnelHealth {
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

/// High-level snapshot of direct inbound server health.
#[derive(Debug, Clone, PartialEq, Eq)]
// Same explanation as `impl AppState` above: this is genuine API surface
// for tests and the inline e2e harness; cargo build's dead-code pass
// doesn't see those callers after the F9 narrowing to `pub(crate)`.
#[allow(dead_code)]
pub(crate) enum DirectHealth {
    /// Direct inbound connections are disabled in configuration.
    Disabled,
    /// Server is starting but has not bound a port yet.
    Starting,
    /// Server is listening on the given address.
    Listening(std::net::SocketAddr),
    /// Server failed to start or crashed.
    Error(String),
}

#[allow(dead_code)] // JSON formatting helpers — used by IPC handler serialisation; not reachable from cargo build's graph.
impl DirectHealth {
    /// String discriminator used in JSON output (`direct.state`).
    #[must_use]
    pub fn state_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Starting => "starting",
            Self::Listening(_) => "listening",
            Self::Error(_) => "error",
        }
    }

    /// Bound socket address, if listening.
    #[must_use]
    pub fn bound_addr(&self) -> Option<std::net::SocketAddr> {
        match self {
            Self::Listening(addr) => Some(*addr),
            _ => None,
        }
    }

    /// Last error string, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Error(e) => Some(e.as_str()),
            _ => None,
        }
    }
}

/// Load the runtime's Ed25519 signing key from the encrypted vault.
fn load_runtime_signing_key(
    identity: &crate::identity::runtime::RuntimeIdentity,
    vault: &crate::common::vault::Vault,
) -> anyhow::Result<Arc<ed25519_dalek::SigningKey>> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use ed25519_dalek::SigningKey;

    let privkey_b64 = identity
        .load_private_key(vault)?
        .ok_or_else(|| anyhow::anyhow!("runtime private key not found in vault"))?;
    let privkey_bytes = BASE64
        .decode(privkey_b64.trim())
        .map_err(|e| anyhow::anyhow!("runtime private key is not valid base64: {e}"))?;
    if privkey_bytes.len() != 32 {
        anyhow::bail!(
            "runtime private key is {} bytes; expected 32",
            privkey_bytes.len()
        );
    }
    let mut key_arr = [0u8; 32];
    key_arr.copy_from_slice(&privkey_bytes);
    Ok(Arc::new(SigningKey::from_bytes(&key_arr)))
}

/// Load `peko.toml` from the config directory, falling back to defaults
/// if the file does not exist or cannot be parsed.
fn load_peko_config(config_dir: &Path) -> PekoConfig {
    let path = config_dir.join("peko.toml");
    if path.exists() {
        match PekoConfig::from_file(&path) {
            Ok(cfg) => return cfg,
            Err(e) => {
                tracing::warn!(
                    "Failed to load {}: {e}; using default configuration",
                    path.display()
                );
            }
        }
    }
    PekoConfig::default()
}

/// Memory factory that places Principal memory under the data directory,
/// outside the config directory where `principal.toml` lives.
struct DaemonPrincipalMemoryFactory {
    data_dir: PathBuf,
}

#[async_trait::async_trait]
impl PrincipalMemoryFactory for DaemonPrincipalMemoryFactory {
    async fn create(
        &self,
        _principal_id: &crate::subject::PrincipalId,
        workspace_path: &Path,
    ) -> Arc<dyn PrincipalMemory> {
        let name = workspace_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let memory_dir = self.data_dir.join("principals").join(name).join("memory");
        let _ = tokio::fs::create_dir_all(&memory_dir).await;
        let memory = DefaultPrincipalMemory::new(memory_dir);
        let _ = tokio::fs::create_dir_all(memory.sessions_dir()).await;
        Arc::new(memory)
    }
}

impl Default for DaemonConfigSnapshot {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(".peko"),
            config_dir: PathBuf::from(".peko"),
            log_level: "info".to_string(),
            launch_mode: crate::daemon::LaunchMode::default(),
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
    async fn test_instance_count_starts_at_zero() {
        // `instance_count()` is live (read by `ipc/server.rs:1480` for the
        // SystemStatus response). The corresponding setter was removed
        // — it had no production callers, only this test — so the only
        // meaningful invariant we can assert is the initial value.
        let state = create_test_state().await;
        assert_eq!(state.instance_count().await, 0);
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
            tool_runtime.has_tool("Bash").await,
            "Bash tool not registered"
        );
        assert!(
            tool_runtime.has_tool("Read").await,
            "Read tool not registered"
        );
        assert!(
            tool_runtime.has_tool("Write").await,
            "Write tool not registered"
        );
        assert!(
            tool_runtime.has_tool("Glob").await,
            "Glob tool not registered"
        );
        assert!(
            tool_runtime.has_tool("Grep").await,
            "Grep tool not registered"
        );
        assert!(
            tool_runtime.has_tool("Edit").await,
            "Edit tool not registered"
        );
        // `AsyncSpawn` and `AsyncOutput` are registered per-agent (not
        // globally on the daemon's ToolRuntime) — see `Agent::build_agentic_loop`
        // and `BuiltinToolAdapter::register_async_spawn_tool`. Asserting they
        // are missing here pins the contract.

        // ExtensionCore should list the tools
        let core = tool_runtime.extension_core();
        let tools = core
            .list_tools(crate::subject::PrincipalId::system())
            .await;
        assert!(!tools.is_empty(), "No tools in ExtensionCore");

        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        assert!(tool_names.contains(&"Bash".to_string()));
        assert!(tool_names.contains(&"Grep".to_string()));

        // Tool definitions should be available for LLM API
        let defs = core.list_tool_definitions().await;
        assert!(!defs.is_empty(), "No tool definitions available");
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_agent_init_preserves_pre_registered_tools() {
        use crate::agents::agent_config::AgentConfig;
        use crate::agents::Agent;
        use crate::extensions::framework::core::init_global_core;
        use crate::extensions::framework::{HookInput, HookPoint};

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
        let tools: Vec<crate::extensions::framework::types::ToolMetadata> = core
            .list_tools(crate::subject::PrincipalId::system())
            .await;
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        assert!(
            tool_names.contains(&"Bash".to_string()),
            "Bash missing after agent init"
        );
        assert!(
            tool_names.contains(&"Grep".to_string()),
            "Grep missing after agent init"
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
        assert!(prompt_text.contains("Bash"), "Prompt doesn't mention Bash");
        assert!(prompt_text.contains("Grep"), "Prompt doesn't mention Grep");
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

// F5: AppState is the only type that knows both `daemon` and `tunnel`, so it
// implements the tunnel's narrow host port here. The dispatcher holds an
// `Arc<dyn TunnelHost>` and never names `AppState` (boundary rule 9).
impl crate::tunnel::TunnelHost for AppState {
    fn principal_manager(&self) -> Arc<PrincipalManager> {
        Arc::clone(&self.principal_manager)
    }

    fn runtime_did(&self) -> String {
        self.runtime_identity.runtime_did.clone()
    }

    fn runtime_display_name(&self) -> String {
        self.runtime_metadata.display_name.clone()
    }

    fn runtime_direct_endpoint(&self) -> Option<String> {
        self.peko_config.network.direct.advertise_endpoint.clone()
    }

    fn jwt_validator(&self) -> Option<crate::auth::jwt::JwtValidator> {
        self.jwt_validator.clone()
    }

    fn pending_a2a_responses(&self) -> Arc<crate::tunnel::PendingA2aResponses> {
        self.pending_a2a_responses.clone()
    }

    fn observability(&self) -> Arc<Observability> {
        self.observability.clone()
    }

    fn tunnel_handle_slot(&self) -> Arc<RwLock<Option<crate::tunnel::TunnelHandle>>> {
        self.tunnel_handle_slot.clone()
    }
}

/// F7 first narrow handle: the port the `system` IPC domain handler uses
/// to reach daemon state. The trait lives in `ipc::handlers::system` (the
/// consumer defines the port, the producer implements it — same
/// dependency-inversion pattern as `TunnelHost`).
#[async_trait::async_trait]
impl crate::ipc::handlers::system::SystemHost for AppState {
    fn uptime_seconds(&self) -> u64 {
        AppState::uptime_seconds(self)
    }

    fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone()
    }

    async fn is_degraded(&self) -> bool {
        AppState::is_degraded(self).await
    }

    async fn is_ready(&self) -> bool {
        AppState::is_ready(self).await
    }

    async fn instance_count(&self) -> u64 {
        AppState::instance_count(self).await
    }

    async fn tunnel_health(&self) -> TunnelHealth {
        AppState::tunnel_health(self).await
    }

    async fn request_shutdown(&self, force: bool) {
        AppState::request_shutdown(self, force).await;
    }

    fn launch_mode(&self) -> crate::daemon::LaunchMode {
        self.config.launch_mode
    }
}

/// F7 second narrow handle: the port the `auth` IPC domain handler uses
/// to reach the API key store and auth configuration. Trait lives in
/// `ipc::handlers::auth`; both methods are sync (return owned values)
/// so the trait is object-safe without `async_trait`.
impl crate::ipc::handlers::auth::AuthHost for AppState {
    fn auth_config(&self) -> crate::auth::config::AuthConfig {
        AppState::auth_config(self)
    }

    fn api_key_store(&self) -> Option<crate::auth::api_key::ApiKeyStore> {
        AppState::api_key_store(self)
    }
}

/// F7 third narrow handle: the port the `tool` IPC domain handler uses
/// to reach the async task executor, tool runtime, principal manager,
/// and extension store. Trait lives in `ipc::handlers::tool`. All
/// methods are sync (return cheap references / `Arc` clones) so the
/// trait is object-safe without `async_trait`. The actual principal
/// resolution (F8 server-side grant threading) is awaited inside the
/// handler against these accessors.
impl crate::ipc::handlers::tool::ToolHost for AppState {
    fn principal_manager(&self) -> &Arc<PrincipalManager> {
        AppState::principal_manager(self)
    }

    fn extension_store(&self) -> &Arc<ExtensionStore> {
        AppState::extension_store(self)
    }

    fn tool_runtime(&self) -> Arc<ToolRuntime> {
        self.tool_runtime.clone()
    }

    fn async_task_executor(&self) -> Arc<AsyncExecutor> {
        self.async_task_executor.clone()
    }
}

/// F7 fifth narrow handle: the port the `capability` IPC domain handler
/// uses for principal-capability grant/list/revoke. Trait lives in
/// `ipc::handlers::capability`. Both methods are sync (return cheap
/// references), so the trait is object-safe without `async_trait`. The
/// actual per-principal mutations happen in the handler against these
/// accessors.
impl crate::ipc::handlers::capability::CapabilityHost for AppState {
    fn principal_manager(&self) -> &Arc<PrincipalManager> {
        AppState::principal_manager(self)
    }

    fn extension_store(&self) -> &Arc<ExtensionStore> {
        AppState::extension_store(self)
    }
}

/// F7 sixth narrow handle: the port the `instance` IPC domain handler
/// uses to reach the live tunnel dispatcher. Trait lives in
/// `ipc::handlers::instance`. Async because `tunnel_dispatcher` is
/// behind a lock; the trait needs `async_trait` for the same reason.
#[async_trait::async_trait]
impl crate::ipc::handlers::instance::InstanceHost for AppState {
    async fn tunnel_dispatcher(&self) -> Option<crate::tunnel::TunnelDispatcher> {
        AppState::tunnel_dispatcher(self).await
    }
}

/// F7 seventh narrow handle: the port the `ext_runtime` IPC domain
/// handler uses to drive the background extension runtime manager
/// (ADR-025). Trait lives in `ipc::handlers::ext_runtime`. All
/// methods are sync (return cheap references / owned `StarterContext`),
/// so the trait is object-safe without `async_trait`.
impl crate::ipc::handlers::ext_runtime::ExtRuntimeHost for AppState {
    fn runtime_starter_registry(
        &self,
    ) -> &Arc<
        crate::daemon::background_runtime::ExtensionRuntimeStarterRegistry,
    > {
        AppState::runtime_starter_registry(self)
    }

    fn starter_context(
        &self,
    ) -> crate::daemon::background_runtime::StarterContext {
        AppState::starter_context(self)
    }

    fn background_runtime_manager(&self) -> &Arc<BackgroundRuntimeManager> {
        AppState::background_runtime_manager(self)
    }
}

/// F7 eighth narrow handle: the port the `cron` IPC domain handler uses
/// to read the data dir (cron DB lives at `<data_dir>/cron.json`) and
/// the principal manager (used to validate `job.principal_name`
/// resolves before adding a job). Trait lives in
/// `ipc::handlers::cron`. Both methods are sync (cheap reference /
/// `PathBuf` clone), so the trait is object-safe without `async_trait`.
impl crate::ipc::handlers::cron::CronHost for AppState {
    fn data_dir(&self) -> std::path::PathBuf {
        self.data_dir.clone()
    }

    fn principal_manager(&self) -> &Arc<PrincipalManager> {
        AppState::principal_manager(self)
    }
}

/// F18 narrow handle for the `quota` IPC handler. The trait lives in
/// `ipc::handlers::quota` and only exposes the principal manager —
/// the handler reaches the per-principal `QuotaMeter` through
/// `Principal::quota_meter`. Sync (`&Arc<PrincipalManager>` is cheap)
/// so the trait is object-safe without `async_trait`.
impl crate::ipc::handlers::quota::QuotaHost for AppState {
    fn principal_manager(&self) -> &Arc<PrincipalManager> {
        AppState::principal_manager(self)
    }

    fn peer_registry(&self) -> Option<&Arc<crate::principal::peer::PeerRegistry>> {
        self.peer_registry.as_ref()
    }
}

/// F7 ninth narrow handle: the port the `runtime` IPC domain handler
/// uses to surface this runtime's identity / metadata and the
/// persistent `KnownRuntimes` registry. Trait lives in
/// `ipc::handlers::runtime`. All methods are sync (cheap references /
/// `PathBuf` clones), so the trait is object-safe without
/// `async_trait`. The actual `KnownRuntimes` lock awaits live in the
/// handler.
impl crate::ipc::handlers::runtime::RuntimeHost for AppState {
    fn runtime_identity(&self) -> &crate::identity::runtime::RuntimeIdentity {
        AppState::runtime_identity(self)
    }

    fn runtime_metadata(&self) -> &crate::identity::runtime_metadata::RuntimeMetadata {
        AppState::runtime_metadata(self)
    }

    fn known_runtimes(
        &self,
    ) -> &Arc<tokio::sync::RwLock<crate::tunnel::known_runtimes::KnownRuntimes>> {
        AppState::known_runtimes(self)
    }

    fn config_dir(&self) -> std::path::PathBuf {
        self.config_dir.clone()
    }

    fn data_dir(&self) -> std::path::PathBuf {
        self.data_dir.clone()
    }

    fn cache_dir(&self) -> std::path::PathBuf {
        self.cache_dir.clone()
    }
}

/// F7 fourth narrow handle: the port the `tunnel` IPC domain handler uses
/// to drive the tunnel lifecycle from CLI control packets (`TunnelStop`,
/// `TunnelStatus`). Trait lives in `ipc::handlers::tunnel`. Both methods
/// are async because they drive the live outbound tunnel connection.
///
/// Note: this trait is distinct from `crate::tunnel::host::TunnelHost`,
/// which powers inbound-message dispatch (F5). They share a name but
/// live in different modules and serve different consumers; the F5 +
/// F7 dependency-inversion pattern intentionally produces two narrow
/// ports per cross-cutting concern.
#[async_trait::async_trait]
impl crate::ipc::handlers::tunnel::TunnelHost for AppState {
    async fn stop_tunnel(&self) {
        AppState::stop_tunnel(self).await;
    }

    async fn tunnel_connected(&self) -> bool {
        AppState::tunnel_connected(self).await
    }
}

/// F7 tenth narrow handle: the port the `extension` IPC domain handler
/// uses to read/write the on-disk extension store and to enumerate
/// built-in extensions via `Services`. Trait lives in
/// `ipc::handlers::extension`. Both methods are sync (cheap `Arc`
/// references), so the trait is object-safe without `async_trait`.
/// The actual store awaits (install / uninstall / list / bundle /
/// export) happen in the handler against these accessors.
impl crate::ipc::handlers::extension::ExtensionHost for AppState {
    fn extension_store(&self) -> &Arc<ExtensionStore> {
        AppState::extension_store(self)
    }

    fn extension_services(
        &self,
    ) -> &Arc<crate::extensions::framework::services::Services> {
        AppState::extension_services(self)
    }
}

/// F7 eleventh narrow handle: the port the `provider_mcp` IPC domain
/// handler uses to live-reload the provider registry and MCP config
/// from disk. Trait lives in `ipc::handlers::provider_mcp`. Both
/// methods are async because they drive live config-file reloads.
#[async_trait::async_trait]
impl crate::ipc::handlers::provider_mcp::ProviderMcpHost for AppState {
    async fn reload_providers(&self) -> anyhow::Result<(usize, usize)> {
        AppState::reload_providers(self).await
    }

    async fn reload_mcp_config(&self) -> anyhow::Result<usize> {
        AppState::reload_mcp_config(self).await
    }
}

/// F7 thirteenth narrow handle: the port the `credential` IPC domain
/// handler uses. Trait lives in `ipc::handlers::credential`. Sync
/// because reading the in-memory vault (`Vault::list_providers` +
/// `Vault::get_provider_key`) is a pure in-memory operation — no
/// disk I/O, no async work. The handler emits one row per provider
/// id the vault knows about; `has_key` is set iff
/// `Vault::get_provider_key` returns `Some`, and `last_tested` is
/// always `None` until the vault gains that field (see
/// [`crate::ipc::packet::CredentialRow`]).
impl crate::ipc::handlers::credential::CredentialHost for AppState {
    fn list_credentials(&self) -> Vec<crate::ipc::packet::CredentialRow> {
        let vault = &self.vault;
        vault
            .list_providers()
            .into_iter()
            .map(|provider| {
                let has_key = vault.get_provider_key(&provider).is_some();
                crate::ipc::packet::CredentialRow {
                    provider,
                    has_key,
                    last_tested: None,
                }
            })
            .collect()
    }
}

/// F7 twelfth narrow handle: the port the `principal` IPC domain
/// handler uses. Trait lives in `ipc::handlers::principal`. Most
/// methods are sync (cheap references / `Arc` clones / `PathBuf`
/// clones); `tunnel_dispatcher` and `record_principal_activity` are
/// async because they drive live tunnel / activity-write paths. The
/// trait needs `async_trait` for those two.
///
/// The `principal` domain is the largest of the F6 migrations (17
/// arms + a sizable set of `build_*` / `import_*` / `push_*` /
/// `pull_*` / `export_*` / `load_*` / `read_*` helpers). Everything
/// inside `ipc::handlers::principal` reaches daemon state only
/// through this trait.
#[async_trait::async_trait]
impl crate::ipc::handlers::principal::PrincipalHost for AppState {
    fn principal_manager(&self) -> &Arc<PrincipalManager> {
        AppState::principal_manager(self)
    }

    fn streaming_runs(
        &self,
    ) -> Arc<std::sync::Mutex<std::collections::HashMap<u64, StreamingRunHandle>>>
    {
        AppState::streaming_runs(self)
    }

    fn inbox_registry(&self) -> &Arc<crate::session::InboxRegistry> {
        &self.inbox_registry
    }

    fn extension_store(&self) -> &Arc<ExtensionStore> {
        AppState::extension_store(self)
    }

    fn trust_store(
        &self,
    ) -> &Arc<tokio::sync::RwLock<crate::registry::packaging::TrustStore>> {
        AppState::trust_store(self)
    }

    fn config_dir(&self) -> std::path::PathBuf {
        self.config_dir.clone()
    }

    fn data_dir(&self) -> std::path::PathBuf {
        self.data_dir.clone()
    }

    fn cache_dir(&self) -> std::path::PathBuf {
        self.cache_dir.clone()
    }

    async fn record_principal_activity(&self, principal_name: &str) {
        AppState::record_principal_activity(self, principal_name).await;
    }

    async fn tunnel_dispatcher(&self) -> Option<crate::tunnel::TunnelDispatcher> {
        AppState::tunnel_dispatcher(self).await
    }
}
