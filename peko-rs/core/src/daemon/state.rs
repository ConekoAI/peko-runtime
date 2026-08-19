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
use crate::engine::tool_runtime::ToolRuntime;
use crate::extensions::framework::async_exec::executor::AsyncExecutor;
use crate::extensions::framework::inbox::SessionInbox;
use crate::extensions::framework::store::ExtensionStore;
use crate::principal::memory::{DefaultPrincipalMemory, PrincipalMemory};
use crate::principal::{
    factory::{DefaultPrincipalRouterFactory, PrincipalMemoryFactory},
    slash::SlashDispatcher,
    PrincipalManager,
};
use crate::registry::{load_from_workspace, RegistryConfig};
use peko_cron::IdleDetector;
use peko_observability::Observability;
use peko_session::InboxRegistry;
use secrecy::SecretString;
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

    /// Typed path resolver (Phase A). Every code path that needs
    /// per-tier roots (`extensions_root`, `mcps_root`, etc.) reaches
    /// them through this resolver instead of hand-rolled
    /// `data_dir.join("…")` joins. The legacy `data_dir` field is
    /// retained for back-compat with callers that haven't migrated.
    pub path_resolver: crate::common::paths::PathResolver,

    /// **Phase B.** Tier-typed authority that hands out
    /// `LocalPath`/`SharedPath`/`RuntimePath` newtypes. Constructed
    /// once at daemon startup with `Subject::Public` (the daemon
    /// itself is the actor); IPC handlers that act on behalf of a
    /// caller layer a second authority with `Subject::Principal` via
    /// the trait-port `authority()` accessor. See
    /// `peko_core::common::authority` for the type-level tier gate.
    pub authority: Arc<crate::common::authority::RuntimeAuthority>,

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
    /// `ModelCatalog::reload` after `peko model {add,remove}` so the
    /// long-running daemon observes CLI mutations without a restart.
    resolver: Arc<peko_providers::LlmResolver>,

    /// Shared credential vault. Re-read in place via `Vault::reload`
    /// after `peko credential {set,delete}` for the same reason as
    /// `resolver` above. Stored as a concrete `Vault` (not the
    /// `SecretStore` trait object) so `reload` can mutate the inner
    /// state without going through trait dispatch.
    vault: Arc<crate::common::vault::Vault>,

    /// Principal manager (AI Principal container lifecycle)
    principal_manager: Arc<PrincipalManager>,

    /// PR-2c: file-backed channel port (`peko-channel` runtime-tier
    /// store). One per daemon process; shared across IPC handler
    /// invocations and the per-channel subscriber loops. Constructed
    /// lazily at `AppState::build_internal` time using the typed
    /// path resolver's `runtime_dir()`.
    channel_port: Arc<dyn peko_channel::ChannelPort>,

    /// Phase 4 (agent-session paradigm sprint): owns the
    /// per-(principal, channel) `ChannelSubscriber` lifespan — boot
    /// enumeration plus the post-boot create/invite hooks — and the
    /// per-principal bound-session turn drivers. See
    /// `daemon/channel_binding.rs`.
    channel_binding_supervisor: Arc<crate::daemon::channel_binding::ChannelBindingSupervisor>,

    /// peko-channel cross-runtime PR-B commit 2: the concrete
    /// `TunnelChannelPort` that wraps `channel_port`'s local store.
    /// The tunnel dispatcher reaches this through
    /// [`crate::tunnel::TunnelHost::tunnel_channel_port`] so it can
    /// append inbound `TunnelChannelEvent`s to the local mirror
    /// after verifying the envelope signature. `Arc<dyn ChannelPort>`
    /// above stays the trait surface for IPC handlers / tool
    /// runtime; this field is the cross-runtime-typed accessor.
    tunnel_channel_port: Arc<crate::tunnel::TunnelChannelPort>,

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
    pub runtime_identity: peko_identity::runtime::RuntimeIdentity,

    /// Runtime signing key derived from the vault. Shared by the tunnel
    /// client, direct connection manager, and direct server.
    pub runtime_signing_key: Arc<ed25519_dalek::SigningKey>,

    /// In-memory revocation set for invite tokens (PR #11). The
    /// dispatcher's `check_request_allowed` consults this set before
    /// falling back to the Exposure-based ACL. The verifying key for
    /// minting/verifying tokens is derived from `runtime_signing_key`
    /// on demand; only the revocation set needs to live on
    /// `AppState` so multiple dispatchers (and the IPC mint/revoke
    /// handler) share the same view.
    pub invite_revocation_set: Arc<crate::tunnel::InviteRevocationSet>,

    /// Loaded peko configuration (`network.direct.advertise_endpoint`
    /// is still announced to the hub directory; the direct transport
    /// itself retired in sprint 3 Phase 12b).
    pub peko_config: PekoConfig,

    /// Shared idle detector used by the cron engine and IPC server to
    /// track Principal activity for idle-triggered jobs.
    idle_detector: Option<Arc<IdleDetector>>,

    /// Cron engine used by the `CronRun` IPC handler to dispatch
    /// manual triggers (`peko cron run <id>`). Cloned into the
    /// daemon's own `CronEngine` so the IPC handler can spawn
    /// executions without borrowing `Daemon`. Set once at startup
    /// via [`AppState::set_cron_engine`].
    cron_engine: Option<Arc<crate::daemon::cron_engine::CronEngine>>,

    /// Runtime metadata (ADR-032)
    pub runtime_metadata: peko_identity::runtime_metadata::RuntimeMetadata,

    /// Known runtimes registry (ADR-032)
    pub known_runtimes:
        std::sync::Arc<tokio::sync::RwLock<crate::tunnel::known_runtimes::KnownRuntimes>>,

    /// Trust store for principal package publisher pinning (issue #91).
    pub trust_store: std::sync::Arc<tokio::sync::RwLock<crate::registry::packaging::TrustStore>>,

    /// Auth configuration (ADR-034)
    auth_config: peko_auth::config::AuthConfig,

    /// API key store (ADR-034)
    api_key_store: Option<peko_auth::api_key::ApiKeyStore>,

    /// API key verifier (ADR-034)
    api_key_verifier: Option<peko_auth::api_key::ApiKeyVerifier>,

    /// JWT validator (ADR-034)
    jwt_validator: Option<peko_auth::jwt::JwtValidator>,

    /// Rate limiter (ADR-034)
    rate_limiter: Option<peko_auth::rate_limit::RateLimiter>,

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
    /// `std::sync::Mutex` (not the tokio one): every operation is
    /// hash-map-only, no `.await` is held across the lock.
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
#[allow(dead_code)] // field-by-field — kept as public-ish surface for tests.
pub(crate) struct StreamingRunHandle {
    /// Principal name — diagnostic only, included in control responses.
    pub principal_name: String,
    /// Peer subject — needed to derive `session_id` for steer pushes.
    /// Cloned into the IPC handler's scope (cheap, `Subject` is small).
    pub peer: peko_auth::Subject,
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

        // PR-2c: capture the runtime-tier channel directory BEFORE the
        // `path_resolver` is consumed by `RuntimeAuthority::for_runtime`.
        // The `Arc<ChannelStore>` constructor needs a concrete
        // `PathBuf`, not a borrow.
        let channel_runtime_dir = path_resolver.runtime_dir();
        // PR-3d: capture the Shared-tier channel parent. Per-principal
        // SharedLayout::channels_dir lives under each principal's
        // shared root; we use the principals root as the parent
        // because `pin_to_shared` will resolve per-channel inside.
        // Concrete-per-principal resolution happens at the
        // `RuntimeAuthority` seam (deferred — PR-3d wires the trait
        // surface; production auth gate lives in PR-4).
        let channel_shared_root = path_resolver.principals_root_dir();

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
        let runtime_identity = peko_identity::runtime::RuntimeIdentity::generate_or_load(
            crate::identity_compat::runtime_paths_arc(&path_resolver).as_ref(),
            crate::identity_compat::identity_vault_arc(vault.clone()).as_ref(),
        )?;
        let runtime_metadata = peko_identity::runtime_metadata::RuntimeMetadata::load_or_create(
            crate::identity_compat::runtime_paths_arc(&path_resolver).as_ref(),
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
        let invite_revocation_set = Arc::new(crate::tunnel::InviteRevocationSet::new());
        // peko-channel cross-runtime PR-B commit 2: the concrete
        // `TunnelChannelPort` field on `AppState` is populated during
        // channel-port construction (line ~706). The variable holds
        // `None` until then; the init arm at ~706 swaps it for
        // `Some(...)` once the local store has been built. `commit
        // 3` will also assign the `CrossRuntimeChannelCtx` into the
        // wrapper.
        #[allow(unused_assignments)]
        let mut tunnel_channel_port: Option<Arc<crate::tunnel::TunnelChannelPort>> = None;
        let streaming_runs: Arc<std::sync::Mutex<HashMap<u64, StreamingRunHandle>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let trust_store = crate::registry::packaging::TrustStore::load_or_create(&path_resolver)?;
        let trust_store = std::sync::Arc::new(tokio::sync::RwLock::new(trust_store));

        // v3-cleanup: ADR-032 / ADR-033 / provider-catalog migration
        // runners were deleted; the runtime now expects every agent
        // on disk to already have `host_runtime_id` set (which the
        // principal creation path does at v3).

        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver.clone()));

        // v3: Build the `LlmResolver` here so every agent cold-start
        // goes through `LlmResolver::build` instead of the deprecated
        // inline-[provider] path. Catalog is `~/.peko/models.toml`,
        // secrets are the OS keychain. Test harnesses that need a
        // env-var fallback (no keychain on CI) flip
        // `PEKO_TEST_RESOLVER_BOOTSTRAP=1`; the daemon picks that up
        // via `LlmResolver::with_env_bootstrap()` below.
        let catalog_path = path_resolver.config_dir().join("models.toml");
        let catalog = peko_providers::ModelCatalog::load_or_init(&catalog_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load model catalog: {e}"))?;
        let secrets: Arc<dyn peko_providers::secret_store::SecretStore> = Arc::new(
            crate::common::vault_secret_store::VaultSecretStore::new(Arc::clone(&vault)),
        );
        let credential_provider: Arc<dyn peko_provider_api::credentials::CredentialProvider> =
            Arc::new(
                crate::common::vault_credential_provider::VaultCredentialProvider::new(Arc::clone(
                    &vault,
                )),
            );
        // F40b / PR #3 Phase 2B: thread the daemon's `[provider.retry]`
        // config block into the resolver so every provider the daemon
        // builds inherits the same per-retry knobs. The config has
        // already been validated by `load_peko_config`; if validation
        // had failed, we would have fallen back to defaults here too,
        // so re-validation at the resolver layer is just defensive
        // and surfaces any future regression in load-time sanitization.
        let retry_config = peko_config.provider.retry.clone();
        let mut resolver_builder = peko_providers::LlmResolver::new(catalog, secrets)
            .with_credential_provider(credential_provider)
            .with_retry_config(retry_config)
            .map_err(|e| anyhow::anyhow!("resolver retry-config wiring failed: {e}"))?;
        if std::env::var_os("PEKO_TEST_RESOLVER_BOOTSTRAP").is_some() {
            resolver_builder = resolver_builder.with_env_bootstrap();
        }
        let resolver = Arc::new(resolver_builder);

        let path_resolver_clone = path_resolver.clone();
        let principal_service = Arc::new(
            StatelessAgentService::new_with_resolver(
                config_service.clone(),
                Arc::new(peko_session::DefaultPathResolver::with_data_dir(
                    path_resolver.data_dir().to_path_buf(),
                )),
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
        let principal_service_dyn: Arc<
            dyn crate::extensions::framework::principal_message::PrincipalMessageService,
        > = principal_service.clone();

        // For tests, always create a fresh core to avoid shared mutable state
        // between concurrent tests.
        //
        // WS3 (implicit session management, 2026-08-11): hoist the
        // daemon-shared inbox registry ABOVE this block so the
        // AsyncExecutionRouter can be wired to it. The router owns an
        // executor whose completion pushes must land in the same
        // registry the agentic loop drains at iteration start —
        // otherwise subagent results are silently dropped on the
        // floor and `persist_subagent_completions` never fires.
        let inbox_registry = Arc::new(InboxRegistry::new(Arc::new(
            || -> Arc<dyn peko_extension_api::AsyncInboxLike> { Arc::new(SessionInbox::new()) },
        )));
        let global_core = if for_test {
            use crate::extensions::framework::core::{ExtensionCore, ExtensionServices};
            use crate::extensions::framework::transport::async_router::AsyncExecutionRouter;
            use crate::extensions::framework::transport::async_transport::create_local_transport_with_inbox;
            let router = AsyncExecutionRouter::with_transport(
                create_local_transport_with_inbox(Arc::clone(&inbox_registry)),
            );
            let services = ExtensionServices::with_async_router_and_principal_message_service(
                Arc::new(router),
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
            use crate::extensions::framework::transport::async_router::AsyncExecutionRouter;
            use crate::extensions::framework::transport::async_transport::create_local_transport_with_inbox;
            let router = AsyncExecutionRouter::with_transport(
                create_local_transport_with_inbox(Arc::clone(&inbox_registry)),
            );
            let services = ExtensionServices::with_async_router_and_principal_message_service(
                Arc::new(router),
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
        // PR-4a: wire the daemon's real `channel_port` so `ChannelRead`
        // resolves to the file-backed `ChannelStore` (PR-5b) and
        // can be invoked from any principal's agentic loop. The port
        // is built first so we can both register it with the tool
        // runtime and store it on `AppState` (line ~939) without
        // doubling up the adapter construction.
        let channel_port: Arc<dyn peko_channel::ChannelPort> = {
            // peko-channel cross-runtime PR-B commit 2: wrap the
            // local `ChannelStore` in a `TunnelChannelPort` so the
            // tunnel dispatcher's `handle_inbound_tunnel_channel_event`
            // can reach the cross-runtime append path. The wrapper
            // implements `ChannelPort` via pure delegation, so the
            // `dyn ChannelPort` surface for the tool runtime / IPC
            // handlers is unchanged.
            //
            // `ctx` is `None` for commit 2 — commit 3 wires the
            // `CrossRuntimeChannelCtx` so `post` / `invite` can fan
            // out to remote members.
            let store = Arc::new(peko_channel::ChannelStore::new(
                peko_channel::ChannelConfig {
                    runtime_dir: channel_runtime_dir.clone(),
                    // PR-3d: Shared-tier root for `pin_to_shared`.
                    shared_dir: Some(channel_shared_root.clone()),
                },
            ));
            // peko-channel cross-runtime PR-B commit 3: the cross-runtime
            // ctx slot starts as `None`. `install_cross_runtime_channel_ctx`
            // (called once the tunnel has been provisioned) fills it in
            // via `TunnelChannelPort::set_ctx` so `post` / `invite` can
            // fan out to remote members.
            let tcp = crate::tunnel::TunnelChannelPort::new(store);
            tunnel_channel_port = Some(Arc::new(tcp.clone()));
            Arc::new(tcp) as Arc<dyn peko_channel::ChannelPort>
        };
        // Install the real port process-wide so a later
        // `PrincipalContext::core()` tool-bag re-registration resolves
        // the same adapter via `peko_channel::global_channel_port()`
        // instead of clobbering the global-core `ChannelRead` /
        // `ChannelSend` instances with a `NoopChannelPort`
        // (2026-08-18 reviewer finding). Set-once; silently ignored
        // if a previous daemon init in this process already set it.
        peko_channel::set_global_channel_port(channel_port.clone());
        // Sprint 4: install the channel port on the ExtensionServices
        // so the per-agent `ChannelSendTool` constructor in `agent.rs`
        // can find it (mirrors `set_cross_runtime_a2a_ctx` /
        // `set_llm_resolver` above). Without this the per-agent
        // registration falls through to the `No ChannelPort` branch
        // and ChannelSend is skipped entirely.
        global_core
            .services()
            .set_channel_port(channel_port.clone());
        let tool_runtime = Arc::new(
            ToolRuntime::with_workspace_and_core_and_channel_port(
                path_resolver_clone.clone(),
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                Arc::clone(&global_core),
                channel_port.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create tool runtime: {e}"))?,
        );
        // Per-session inbox registry: shared by the IPC server (which
        // pushes steering messages from external clients), the
        // `AsyncExecutor` (which pushes completion events from
        // background tasks), and the in-flight `AgenticLoop` (which
        // drains at iteration start). Lazy-initializes entries on
        // first access; no explicit cleanup. NOTE: the actual
        // `inbox_registry` Arc is constructed ABOVE the global_core
        // block (WS3, 2026-08-11) so the AsyncExecutionRouter can be
        // wired to it; we just `Arc::clone` it here.
        let async_task_executor =
            Arc::new(AsyncExecutor::new(Arc::clone(&inbox_registry)));

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
                .with_storage_dir(path_resolver.extensions_root()),
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
        // ADR-046 trust + audit: write to JSONL sink rooted at
        // `<data_dir>/runtime/audit` so audit events survive daemon
        // restarts and are queryable via `peko audit tail`. Construction
        // is fail-fast: if the audit dir can't be created, the daemon
        // refuses to start — an un-audited daemon is worse than no daemon.
        let observability = Arc::new(Observability::with_audit_dir(
            "api",
            path_resolver.audit_dir(),
        )?);

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
                path_resolver.clone(),
                Arc::new(DaemonPrincipalMemoryFactory {
                    resolver: path_resolver.clone(),
                }),
                Arc::new(DefaultPrincipalRouterFactory),
                Arc::clone(&inbox_registry),
            )
            .with_resolver(resolver.clone())
            .with_slash_dispatcher(slash_dispatcher)
            .with_extension_store(Arc::clone(&extension_store))
            .with_observability(Arc::clone(&observability))
            // Sprint 3 Phase 10: peer ingress auto-provisions the
            // peer's DM channel through the daemon-global port.
            .with_channel_port(channel_port.clone());

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
                    tracing::warn!("Failed to load peer registry from {}: {e}", root.display());
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
        let auth_config = peko_auth::config::AuthConfig::load(&path_resolver)?;
        let api_key_store = if auth_config.enable_api_key() {
            Some(peko_auth::api_key::ApiKeyStore::load(&path_resolver)?)
        } else {
            None
        };
        let api_key_verifier = api_key_store
            .as_ref()
            .map(|s| peko_auth::api_key::ApiKeyVerifier::new(s.clone()));
        let jwt_validator = if auth_config.enable_pekohub_jwt() {
            Some(peko_auth::jwt::JwtValidator::new(
                auth_config.trusted_issuers().to_vec(),
                runtime_identity.runtime_did.clone(),
                None,
            ))
        } else {
            None
        };
        let rate_limiter = if auth_config.has_any_remote_auth_method() {
            Some(peko_auth::rate_limit::RateLimiter::new(
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

        // Phase 4 (agent-session paradigm sprint): the channel
        // passive-binding supervisor. Owns subscriber spawning (boot +
        // post-boot hooks) and per-principal bound-session turn
        // drivers. Built here (not lazily) so the `ChannelHost` hooks
        // on `AppState` can reach it from the first IPC call. The meter
        // is one `AuditChannelMeter` over the observability hub
        // (`peko audit list --type channel.` surfaces channel
        // observation history); the supervisor clones the `Arc` into
        // each subscriber — the meter is a stateless wrapper, so one
        // shared instance is equivalent to the pre-Phase-4 per-call
        // construction.
        let channel_binding_supervisor = Arc::new(
            crate::daemon::channel_binding::ChannelBindingSupervisor::new(
                channel_port.clone(),
                Arc::new(peko_channel::AuditChannelMeter::new(observability.clone())),
                path_resolver.runtime_dir(),
                Arc::clone(&principal_manager),
                Arc::clone(&resolver),
                Arc::clone(&observability),
            ),
        );

        // Sprint 3 Phase 10: peer ingress (`PeerChildTurns::ensure_child`
        // via `PrincipalManager`) fires this hook when it freshly
        // creates a peer's DM channel, so the channel gets its
        // subscriber — including the `PassiveBindingResponder` — without
        // waiting for the next boot sweep. Installed post-construction
        // because the supervisor itself needs the manager's `Arc`.
        {
            let supervisor = Arc::clone(&channel_binding_supervisor);
            principal_manager.set_dm_subscriber_hook(Arc::new(move |principal, channel| {
                supervisor.ensure_subscriber(principal, channel);
            }));
        }

        Ok(Self {
            started_at: SystemTime::now(),
            workspace_path,
            config_dir,
            data_dir,
            cache_dir,
            // Phase A: carry the typed resolver forward so starters
            // and IPC handlers can reach `extensions_root()`,
            // `principal_layout(name).local.root`, etc. without
            // re-deriving them from `data_dir`.
            path_resolver: path_resolver.clone(),
            // **Phase B.** Authority gated by `Subject::Public` — the
            // daemon itself is the actor. IPC handlers that act on
            // behalf of a caller wrap this with `Subject::Principal`
            // for tier-specific reads.
            authority: Arc::new(crate::common::authority::RuntimeAuthority::for_runtime(
                path_resolver,
            )),
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
            // PR-2c: instantiate the file-backed channel port against
            // the typed path resolver's runtime dir. PR-1 only ships
            // Runtime-tier; PR-3 may add a Shared-tier sibling adapter.
            channel_port: channel_port.clone(),
            channel_binding_supervisor,
            // peko-channel cross-runtime PR-B commit 2: the concrete
            // `TunnelChannelPort` accessor. Built by the
            // channel_port construction block above (line ~706); the
            // `unwrap_or_else` here turns the local `None` default
            // into a panic if someone reorders the construction
            // without populating `tunnel_channel_port` — fail loud,
            // not silent.
            tunnel_channel_port: tunnel_channel_port
                .clone()
                .unwrap_or_else(|| panic!("tunnel_channel_port must be initialized before AppState build")),
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
            invite_revocation_set,
            peko_config,
            idle_detector: None,
            cron_engine: None,
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
            // Issue #29: outbound tunnel handle slot. Starts as
            // `None` and is filled by the dispatcher's
            // handle-publisher on every reconnect.
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

    /// Attach the daemon-owned cron engine used by the `CronRun` IPC
    /// handler. Called once at startup after the engine is wired to
    /// the real `PrincipalManager` (the placeholder made before
    /// `AppState` exists is replaced by the daemon and re-attached
    /// here so the IPC handler can dispatch manual triggers).
    pub fn set_cron_engine(&mut self, engine: Arc<crate::daemon::cron_engine::CronEngine>) {
        self.cron_engine = Some(engine);
    }

    /// Record activity for a Principal so idle-triggered cron jobs do not
    /// fire while the Principal is actively being used.
    pub async fn record_principal_activity(&self, principal_name: &str) {
        if let Some(detector) = self.idle_detector.as_ref() {
            detector.record_activity(principal_name).await;
        }
    }

    /// Re-read the model catalog and the credential vault from
    /// disk. Called by the IPC `ModelReload` handler so CLI
    /// mutations (`peko model {add,remove}`, `peko credential
    /// {set,delete}`) are visible to the long-running daemon without
    /// a restart.
    ///
    /// Returns `(models_count, keys_count)` for the IPC response so
    /// the caller can confirm what was reloaded. A reload that
    /// partially fails (e.g. corrupt vault) keeps the prior in-memory
    /// state and surfaces the error rather than blanking the daemon.
    pub async fn reload_models(&self) -> anyhow::Result<(usize, usize)> {
        let models_count = self
            .resolver
            .catalog()
            .reload()
            .await
            .map_err(|e| anyhow::anyhow!("model catalog reload failed: {e}"))?;
        let keys_count = self
            .vault
            .reload()
            .map_err(|e| anyhow::anyhow!("vault reload failed: {e}"))?;
        tracing::info!("Model reload: {models_count} models, {keys_count} vault entries");
        Ok((models_count, keys_count))
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
            if let Err(e) =
                async move { m.read().await.start_server(&name_owned, None).await }.await
            {
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

    /// Phase 4 (agent-session paradigm sprint): the channel
    /// passive-binding supervisor (subscriber lifespan + bound-session
    /// turn drivers). Cloned `Arc` — callers (`daemon::run`'s boot
    /// spawn, the `ChannelHost` hooks) share the one instance.
    pub(crate) fn channel_binding_supervisor(
        &self,
    ) -> Arc<crate::daemon::channel_binding::ChannelBindingSupervisor> {
        Arc::clone(&self.channel_binding_supervisor)
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
    pub fn auth_config(&self) -> peko_auth::config::AuthConfig {
        self.auth_config.clone()
    }

    /// Get the API key store (ADR-034)
    #[must_use]
    pub fn api_key_store(&self) -> Option<peko_auth::api_key::ApiKeyStore> {
        self.api_key_store.clone()
    }

    /// Get the API key verifier (ADR-034)
    #[must_use]
    pub fn api_key_verifier(&self) -> Option<peko_auth::api_key::ApiKeyVerifier> {
        self.api_key_verifier.clone()
    }

    /// Get the JWT validator (ADR-034)
    #[must_use]
    pub fn jwt_validator(&self) -> Option<peko_auth::jwt::JwtValidator> {
        self.jwt_validator.clone()
    }

    /// Get the rate limiter (ADR-034)
    #[must_use]
    pub fn rate_limiter(&self) -> Option<peko_auth::rate_limit::RateLimiter> {
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
            // Phase A: hand the typed resolver through so starters
            // can reach `extensions_root()`, `mcps_root()`, etc.
            path_resolver: self.path_resolver.clone(),
            vault: Some(Arc::clone(&self.vault)),
            resolver: Some(Arc::clone(&self.resolver)),
        }
    }

    /// Get the runtime identity (ADR-032)
    #[must_use]
    pub fn runtime_identity(&self) -> &peko_identity::runtime::RuntimeIdentity {
        &self.runtime_identity
    }

    /// Get the runtime metadata (ADR-032)
    #[must_use]
    pub fn runtime_metadata(&self) -> &peko_identity::runtime_metadata::RuntimeMetadata {
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

        // Build the cross-runtime dispatch ctx for `ChannelSend`'s principal
        // branch (sprint 4: this used to live in `send_peer`; sprint 4
        // unifies both surfaces — directory + caller runtime id +
        // principal manager + the concrete `TunnelChannelPort`) and
        // register it on the `ExtensionServices` so every per-agent
        // `ChannelSendTool` gets the ctx injected (via `agent.rs`).
        //
        // If the directory client build fails, log
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
        // peko-channel cross-runtime PR-B commit 3: install the
        // channel ctx (same signing key + directory as the a2a
        // install). Local-only channels still work without this;
        // this just unlocks outbound fan-out to remote members.
        if let Err(e) = self.install_cross_runtime_channel_ctx(&cred, &vault).await {
            warn!(
                "Could not install cross-runtime channel ctx (peko-channel PR-B); \
                 cross-runtime channel fan-out will be unavailable until this is fixed. \
                 The local channel path is unaffected. error: {e:#}"
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

    /// Install the cross-runtime dispatch context for the `ChannelSend`
    /// tool's principal branch on the `ExtensionServices` so every
    /// per-agent `ChannelSendTool` is built with it. Called by
    /// `start_tunnel` after the dispatcher is built but before the
    /// tunnel client starts.
    ///
    /// Sprint 3 Phase 12b: the ctx slimmed down to what the
    /// channel-based principal DM path needs — the directory (target
    /// runtime resolution), the caller runtime id, the principal
    /// manager (peer-child + DM-channel provisioning), and the
    /// concrete `TunnelChannelPort` (posts, reply subscription, and
    /// the DM invite fan-out). The retired RPC stack's signing key,
    /// pending-response registry, tunnel handle slot, direct
    /// manager, known-runtimes registry, and chat-log store are
    /// gone from this ctx (the channel ctx carries the first four
    /// for its own envelope fan-out).
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
        use std::time::Duration;

        // Shared directory + runtime_id. Same components the channel
        // ctx consumes; factored out so the two install paths cannot
        // drift on credential decoding (`peko-channel` cross-runtime
        // PR-B commit 3). The signing key is the channel ctx's
        // concern now — the `ChannelSend` principal branch signs
        // nothing (the channel envelopes it triggers are signed
        // inside `TunnelChannelPort`).
        let (directory, _signing_key, caller_runtime_id) =
            self.build_cross_runtime_ctx_parts(cred, vault)?;

        let ctx = Arc::new(CrossRuntimeA2aCtx {
            directory,
            caller_runtime_id,
            principal_manager: self.principal_manager().clone(),
            channel_port: Arc::clone(&self.tunnel_channel_port),
            response_timeout: Duration::from_mins(1),
        });
        // The framework stores the ctx as `Arc<dyn Any + Send + Sync>`
        // to avoid a framework → tunnel dependency.
        let ctx: Arc<dyn std::any::Any + Send + Sync + 'static> = ctx;

        // Register on the `ExtensionServices`. The per-agent
        //    `ChannelSendTool` constructor in `agent.rs` consults
        //    `services().cross_runtime_a2a_ctx()` and builds with the
        //    ctx if present.
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
        // router so `send_peer` is registered on their agents.
        // Routers that don't need a runtime id (the default for
        // anything other than `RootRouter`) ignore the call.
        let runtime_id = cred.runtime_id.clone();
        for principal in self.principal_manager().list_all().await {
            Arc::clone(&principal.router).set_caller_runtime_id(runtime_id.clone());
        }

        Ok(())
    }

    /// Build the shared parts of every cross-runtime ctx: the
    /// directory (local-first wrap), the runtime signing key, and the
    /// `runtime_id` echoed into outbound envelopes. Factored out of
    /// `install_cross_runtime_a2a_ctx` and
    /// `install_cross_runtime_channel_ctx` so the two paths cannot
    /// drift on credential decoding or directory wrap.
    ///
    /// `peko-channel` cross-runtime PR-B commit 3 — the channel ctx
    /// consumes the same components (PR-A already wires the
    /// `TunnelChannelEvent` envelope signature against this signing
    /// key, so it must be the same key the a2a path mints invite
    /// tokens with).
    fn build_cross_runtime_ctx_parts(
        &self,
        cred: &crate::tunnel::PekoHubCredential,
        vault: &crate::common::vault::Vault,
    ) -> anyhow::Result<(Arc<dyn crate::tunnel::AgentDirectory>, Arc<ed25519_dalek::SigningKey>, String)>
    {
        use base64::engine::general_purpose::STANDARD as BASE64;
        use base64::Engine as _;
        use ed25519_dalek::SigningKey;

        // Directory: hub HTTP client + local-first wrap so
        // same-runtime principals resolve without the hub.
        let hub_directory = crate::tunnel::HubAgentDirectoryClient::from_credential(cred)
            .map_err(|e| anyhow::anyhow!("HubAgentDirectoryClient::from_credential: {e}"))?;
        let directory: Arc<dyn crate::tunnel::AgentDirectory> =
            Arc::new(crate::tunnel::LocalFirstAgentDirectory::new(
                cred.runtime_id.clone(),
                self.principal_manager().clone(),
                Arc::new(hub_directory),
            ));

        // Signing key: 32 raw bytes decoded from the credential's
        // base64-encoded private key in the vault.
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

        Ok((directory, signing_key, cred.runtime_id.clone()))
    }

    /// Install the cross-runtime channel dispatch context on the
    /// `TunnelChannelPort` so `post` / `invite` can fan out to remote
    /// members. `peko-channel` cross-runtime PR-B commit 3.
    ///
    /// Built with the same `directory` + `signing_key` as the a2a
    /// path so the runtime identity is consistent across both
    /// outbound envelopes (and so the channel signature matches the
    /// one the hub recognizes from invite-token mints).
    ///
    /// Like the a2a install, this is best-effort: if the credential
    /// is missing or malformed, log a warning and skip — local-only
    /// channel use stays available.
    async fn install_cross_runtime_channel_ctx(
        &self,
        cred: &crate::tunnel::PekoHubCredential,
        vault: &crate::common::vault::Vault,
    ) -> anyhow::Result<()> {
        use crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx;

        let (directory, signing_key, caller_runtime_id) =
            self.build_cross_runtime_ctx_parts(cred, vault)?;

        let ctx = Arc::new(CrossRuntimeChannelCtx {
            directory,
            signing_key,
            caller_runtime_id,
            tunnel: self.tunnel_handle_slot(),
            known_runtimes: self.known_runtimes.clone(),
        });

        // Push into the existing `TunnelChannelPort`'s ctx slot. The
        // `Arc::clone` keeps the `TunnelChannelPort` reachable
        // (idempotent if called twice — the slot holds the latest
        // ctx, used by the tunnel-reconnect path so a fresh
        // `runtime_id` propagates without rebuilding every
        // `TunnelChannelPort`).
        self.tunnel_channel_port.set_ctx(ctx).await;
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

/// Load the runtime's Ed25519 signing key from the encrypted vault.
fn load_runtime_signing_key(
    identity: &peko_identity::runtime::RuntimeIdentity,
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
///
/// F40b: also validates the `[provider.retry]` block — invalid jitter
/// fractions, zero delay, or `max_retries > max_attempts` surface as
/// a config error so the daemon fails loudly at boot rather than
/// silently running with bad values for the lifetime of the process.
/// The validation message is logged at `warn` and the retry block
/// is replaced with `ProviderRetryConfig::default()` so the daemon
/// still boots in a degraded-but-functional state (matches the
/// pre-F40b behavior of swallowing unknown file errors).
fn load_peko_config(config_dir: &Path) -> PekoConfig {
    let path = config_dir.join("peko.toml");
    if path.exists() {
        match PekoConfig::from_file(&path) {
            Ok(cfg) => {
                if let Err(e) = cfg.provider.retry.validate() {
                    tracing::warn!(
                        "Invalid [provider.retry] in {}: {e}; falling back to defaults",
                        path.display()
                    );
                    let mut sanitized = cfg;
                    sanitized.provider.retry = Default::default();
                    return sanitized;
                }
                return cfg;
            }
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

/// Memory factory that places Principal memory under the Local tier root.
///
/// **Phase A.** Memory now lives at `{data_dir}/principals/{name}/local/`
/// (Local tier), not `{data_dir}/principals/{name}/memory/` (the old
/// on-disk layout). The factory takes a `PathResolver` so the runtime
/// writer and the IPC resolver agree on the same path — the previous
/// hand-rolled `data_dir.join("principals").join(name).join("memory")`
/// join was the root cause of the silent session-export loss.
struct DaemonPrincipalMemoryFactory {
    resolver: crate::common::paths::PathResolver,
}

#[async_trait::async_trait]
impl PrincipalMemoryFactory for DaemonPrincipalMemoryFactory {
    async fn create(
        &self,
        _principal_id: &peko_subject::PrincipalId,
        workspace_path: &Path,
    ) -> Arc<dyn PrincipalMemory> {
        let name = workspace_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let local_root = self.resolver.principal_layout(&name).local.root;
        let _ = tokio::fs::create_dir_all(&local_root).await;
        let memory = DefaultPrincipalMemory::new(local_root);
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

// F5: AppState is the only type that knows both `daemon` and `tunnel`, so it
// implements the tunnel's narrow host port here. The dispatcher holds an
// `Arc<dyn TunnelHost>` and never names `AppState` (boundary rule 9).
#[async_trait::async_trait]
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

    fn jwt_validator(&self) -> Option<peko_auth::jwt::JwtValidator> {
        self.jwt_validator.clone()
    }

    fn observability(&self) -> Arc<Observability> {
        self.observability.clone()
    }

    fn tunnel_handle_slot(&self) -> Arc<RwLock<Option<crate::tunnel::TunnelHandle>>> {
        self.tunnel_handle_slot.clone()
    }

    fn runtime_verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.runtime_signing_key.verifying_key()
    }

    fn invite_revocation_set(&self) -> Arc<crate::tunnel::InviteRevocationSet> {
        Arc::clone(&self.invite_revocation_set)
    }

    /// peko-channel cross-runtime PR-B commit 2: typed accessor for
    /// the concrete `TunnelChannelPort`. The dispatcher's
    /// `handle_inbound_tunnel_channel_event` calls
    /// `tunnel_channel_port().append_remote_event(...)` after
    /// verifying the envelope signature.
    fn tunnel_channel_port(&self) -> Arc<crate::tunnel::TunnelChannelPort> {
        Arc::clone(&self.tunnel_channel_port)
    }

    /// Sprint 3 Phase 12a: cross-runtime DM mirror bootstrap. The
    /// dispatcher calls this after the invite signature verifies (see
    /// the trait docs for the contract).
    async fn dm_channel_mirror_bootstrap(
        &self,
        invite: crate::tunnel::DmChannelInviteBootstrap,
    ) -> anyhow::Result<()> {
        // 1. WHICH local principal was invited: the invitee row (the
        //    one with `runtime_id: None` — "addressed to you")
        //    carries the invited principal's DID.
        let Some(invitee_did) = invite
            .initial_members
            .iter()
            .find(|m| m.runtime_id.is_none())
            .map(|m| m.principal_did.clone())
        else {
            tracing::warn!(
                channel = %invite.channel_id,
                "DM mirror bootstrap: invite carries no invitee row; skipping"
            );
            return Ok(());
        };
        let Some(principal) = self.principal_manager.find_by_did(&invitee_did).await else {
            tracing::warn!(
                channel = %invite.channel_id,
                invitee_did = %invitee_did,
                "DM mirror bootstrap: invitee DID matches no loaded principal; skipping"
            );
            return Ok(());
        };

        // 2. DM invites (binding marker present): CHILD-ONLY ensure
        //    of the receiver's peer child for the creator and derive
        //    the receiver-local binding from the child's real slug.
        //    Deliberately NOT `ensure_child_ingress` — that would also
        //    provision a local-only DM channel (a second, unmirrored
        //    channel for the same peer). The wire binding VALUE is
        //    ignored: each side's binding names its own child, and
        //    `-N` slug suffixes are runtime-local.
        let binding = match invite.passive_binding.as_ref() {
            Some(_) => {
                let peer = peko_auth::Subject::Principal(peko_subject::PrincipalDID(
                    invite.creator_did.clone(),
                ));
                let (owner, agent_name) = {
                    let config = principal.config.read().await;
                    (
                        config.owner.clone(),
                        crate::principal::child_turns::peer_child_agent_config(
                            &config,
                            &principal.workspace_path,
                        )
                        .name,
                    )
                };
                let session_manager = crate::principal::child_turns::peer_child_session_manager(
                    &principal,
                    &agent_name,
                    &owner,
                );
                let child_id = crate::principal::peer_children::ensure_peer_child(
                    &agent_name,
                    &owner,
                    &peer,
                    &session_manager,
                )
                .await?;
                let slug = crate::principal::peer_dm::peer_child_slug_readback(
                    &session_manager,
                    &child_id,
                    &peer,
                )
                .await?;
                Some(format!("/{slug}"))
            }
            None => None,
        };

        // 3. Bootstrap the mirror. `join_remote` is idempotent on the
        //    meta.json-existence check, so a duplicate envelope is a
        //    no-op (the subscriber ensure below is deduped too).
        let channel = peko_channel::ChannelId(invite.channel_id.clone());
        self.tunnel_channel_port
            .join_remote(
                &channel,
                &invite.creator,
                &invite.name,
                &invite.initial_members,
                &principal.id,
                &invite.source_runtime_id,
                binding,
            )
            .await?;

        // 4. Live subscriber — closes the Phase 10 live-hook gap:
        //    bound mirrors get their `PassiveBindingResponder`
        //    immediately; unbound mirrors get the meter-only `Noop`
        //    responder, matching the boot sweep's treatment of
        //    unbound channels (`select_responder`).
        self.channel_binding_supervisor()
            .ensure_subscriber(principal.id.clone(), channel);
        Ok(())
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
    fn auth_config(&self) -> peko_auth::config::AuthConfig {
        AppState::auth_config(self)
    }

    fn api_key_store(&self) -> Option<peko_auth::api_key::ApiKeyStore> {
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

    /// ADR-046: grant audit events are emitted from the
    /// capability handler. Returns the Observability hub by
    /// cheap Arc clone — same shape as the existing `observability`
    /// accessor at line 1985, just lifted onto the trait surface
    /// the handler imports.
    fn observability(&self) -> Arc<Observability> {
        AppState::observability(self)
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
    ) -> &Arc<crate::daemon::background_runtime::ExtensionRuntimeStarterRegistry> {
        AppState::runtime_starter_registry(self)
    }

    fn starter_context(&self) -> crate::daemon::background_runtime::StarterContext {
        AppState::starter_context(self)
    }

    fn background_runtime_manager(&self) -> &Arc<BackgroundRuntimeManager> {
        AppState::background_runtime_manager(self)
    }

    /// Phase B: tier-typed authority mirror. The ext_runtime
    /// handler doesn't currently use tier-typed paths but the
    /// accessor is here for parity with the rest of the trait
    /// ports.
    fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        &self.authority
    }
}

/// F7 eighth narrow handle: the port the `cron` IPC domain handler uses
/// to read the typed path resolver (cron files now live at
/// `{data_dir}/principals/{name}/local/cron/schedule.toml`) and the
/// principal manager (used to validate `job.principal_name` resolves
/// before adding a job, and to enumerate loaded principals for
/// cross-principal operations). Trait lives in `ipc::handlers::cron`.
/// Both methods are sync (cheap reference / `PathResolver` clone), so
/// the trait is object-safe without `async_trait`.
impl crate::ipc::handlers::cron::CronHost for AppState {
    fn path_resolver(&self) -> crate::common::paths::PathResolver {
        // Phase A: hand the typed resolver through to the cron
        // handler so it can derive each principal's
        // `cron_schedule(name)` path without re-walking
        // `data_dir.join("principals").join(name)`.
        self.path_resolver.clone()
    }

    fn principal_manager(&self) -> &Arc<PrincipalManager> {
        AppState::principal_manager(self)
    }

    /// Phase B: hand the tier-typed authority through to the cron
    /// handler. The cron engine resolves Local-tier paths (the
    /// per-principal `schedule.toml` / `history.log`) through this
    /// authority; the actor is the daemon (`Subject::Public`), which
    /// is allowed to grant `LocalPath` (see
    /// `common::authority::RuntimeAuthority::local`).
    fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        &self.authority
    }

    /// Cron engine for manual fire dispatch (`peko cron run <id>`).
    /// The daemon attaches the engine at startup (after `AppState`
    /// exists); the `expect` arms a programmer-error panic if the
    /// IPC `CronRun` packet arrives before the engine is wired up.
    /// `CronEngine` is cheaply cloneable (all internal state is
    /// `Arc`), so callers get an owned handle without a borrow.
    fn cron_engine(&self) -> Arc<crate::daemon::cron_engine::CronEngine> {
        self.cron_engine
            .clone()
            .expect("CronHost::cron_engine called before AppState::set_cron_engine")
    }
}

impl crate::ipc::handlers::channel::ChannelHost for AppState {
    fn path_resolver(&self) -> crate::common::paths::PathResolver {
        self.path_resolver.clone()
    }

    fn channel_port(&self) -> Arc<dyn peko_channel::ChannelPort> {
        self.channel_port.clone()
    }

    fn principal_manager(
        &self,
    ) -> Option<&Arc<crate::principal::manager::PrincipalManager>> {
        Some(&self.principal_manager)
    }

    /// PR-4c: post-invite kickoff hook. Records the join trigger in
    /// the audit ring buffer (`peko audit list --type channel.`) so
    /// operators can distinguish "joined" from "kickoff observed" at
    /// join time, not just at read time.
    ///
    /// Phase 4 (agent-session paradigm sprint): additionally ensures a
    /// subscriber exists for the (invitee, channel) pair — channels
    /// joined after daemon boot otherwise get none until the next
    /// restart. This is a *subscriber* spawn (meter +, for bound
    /// channels, the passive responder), still NOT an `AsyncSpawn` of
    /// `ChannelRead`: waking a session on join remains the passive
    /// binding's job, driven by inbound events rather than the join
    /// itself.
    fn kickoff_channel_read(
        &self,
        invitee: &peko_subject::PrincipalId,
        channel: &peko_channel::ChannelId,
    ) {
        tracing::info!(
            invitee = %invitee.0,
            channel = %channel.as_str(),
            "channel invite kickoff observed (PR-4c); ensuring subscriber (Phase 4)"
        );
        self.channel_binding_supervisor()
            .ensure_subscriber(invitee.clone(), channel.clone());
    }

    /// Phase 4 (agent-session paradigm sprint): post-create hook.
    /// Ensures a subscriber exists for the (creator, channel) pair so
    /// a channel created after boot gets its responder — including the
    /// `PassiveBindingResponder` when `create --bind` set
    /// `meta.json`'s `passive_binding` — without waiting for a daemon
    /// restart.
    fn channel_created(
        &self,
        creator: &peko_subject::PrincipalId,
        channel: &peko_channel::ChannelId,
    ) {
        tracing::info!(
            creator = %creator.0,
            channel = %channel.as_str(),
            "channel created (Phase 4); ensuring subscriber"
        );
        self.channel_binding_supervisor()
            .ensure_subscriber(creator.clone(), channel.clone());
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
    fn runtime_identity(&self) -> &peko_identity::runtime::RuntimeIdentity {
        AppState::runtime_identity(self)
    }

    fn runtime_metadata(&self) -> &peko_identity::runtime_metadata::RuntimeMetadata {
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

    /// Phase B: tier-typed authority mirror. The runtime handler
    /// doesn't currently use tier-typed paths but the accessor is
    /// here for parity with the rest of the trait ports.
    fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        &self.authority
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

    /// Phase B: hand the tier-typed authority through to the
    /// extension handler. Runtime-tier reads (`extensions_root`)
    /// pass through this accessor; the actor is the daemon.
    fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        &self.authority
    }
}

/// Project a catalog [`ModelConfig`](peko_providers::catalog::ModelConfig)
/// into the `ModelSummary` IPC wire shape. Shared by the `ModelList`
/// snapshot and the add/update hosts so every surface emits identical
/// rows for the same catalog entry.
fn model_summary_from_config(
    entry: &peko_providers::catalog::ModelConfig,
) -> crate::ipc::packet::ModelSummary {
    crate::ipc::packet::ModelSummary {
        id: entry.id.clone(),
        display_name: entry.display_name.clone(),
        template_id: entry.template_id.clone(),
        api_type: match entry.api_format {
            peko_providers::catalog::ApiFormat::OpenaiCompletions => "openai".to_string(),
            peko_providers::catalog::ApiFormat::AnthropicMessages => "anthropic".to_string(),
            peko_providers::catalog::ApiFormat::OpenAiResponses => "responses".to_string(),
        },
        base_url: entry.base_url.clone(),
        model_id: entry.model_id.clone(),
        context_window: entry.context_window,
        max_output_tokens: entry.max_output_tokens,
        headers: entry.headers.clone(),
        credential_id: entry.credential_id.clone(),
        requires_key: entry.requires_key,
        is_local: !entry.requires_key,
        enabled: entry.enabled,
        // PR 1 / `feature/model-first-config`: forward the
        // declarative spec so the desktop can gate the image
        // attachment picker, tool toggle, thinking toggle, JSON
        // toggle, etc. without hard-coding per-model branches.
        spec: entry.spec.map(model_spec_to_wire),
        // Phase 2 of `feature/multi-model-subagents`: forward
        // the user note so the desktop can render it on each
        // catalog card. Parent agents reading `model_list` see
        // the same field.
        note: entry.note.clone(),
    }
}

/// PR 1 / `feature/model-first-config`: project the
/// `peko_providers::spec::ModelSpec` into the IPC mirror. Lives
/// at module scope so the future gallery rework (PR 4) can reuse it
/// when shaping "Recommended" / "By facet" lists.
fn model_spec_to_wire(spec: peko_providers::spec::ModelSpec) -> crate::ipc::packet::ModelSpec {
    use crate::ipc::packet::{ModelPricingHint, ModelThinkingMode, ModelToolSupport};
    crate::ipc::packet::ModelSpec {
        image_input: spec.image_input,
        audio_input: spec.audio_input,
        tool_support: match spec.tool_support {
            peko_providers::spec::ToolSupport::None => ModelToolSupport::None,
            peko_providers::spec::ToolSupport::FunctionCalling => ModelToolSupport::FunctionCalling,
            peko_providers::spec::ToolSupport::Full => ModelToolSupport::Full,
        },
        streaming: spec.streaming,
        thinking: match spec.thinking {
            peko_providers::spec::ThinkingMode::Disabled => ModelThinkingMode::Disabled,
            peko_providers::spec::ThinkingMode::Optional => ModelThinkingMode::Optional,
            peko_providers::spec::ThinkingMode::Required => ModelThinkingMode::Required,
            peko_providers::spec::ThinkingMode::CustomBudget => ModelThinkingMode::CustomBudget,
        },
        json_mode: spec.json_mode,
        pricing: spec.pricing.map(|p| ModelPricingHint {
            input_per_million: p.input_per_million,
            output_per_million: p.output_per_million,
        }),
    }
}

/// F7 eleventh narrow handle: the port the `provider_mcp` IPC domain
/// handler uses to live-reload the model catalog and MCP config
/// from disk. Trait lives in `ipc::handlers::provider_mcp`. Both
/// methods are async because they drive live config-file reloads.
#[async_trait::async_trait]
impl crate::ipc::handlers::provider_mcp::ModelMcpHost for AppState {
    async fn reload_models(&self) -> anyhow::Result<(usize, usize)> {
        AppState::reload_models(self).await
    }

    async fn reload_mcp_config(&self) -> anyhow::Result<usize> {
        AppState::reload_mcp_config(self).await
    }

    /// Snapshot every catalog entry (enabled + disabled) as the
    /// `ModelSummary` wire shape. Reads go through
    /// `self.resolver.catalog()` so the response matches what the
    /// resolver and every other daemon-side consumer see — including
    /// user-added entries that don't appear in the static
    /// `BUILT_IN_TEMPLATES`.
    async fn list_catalog_models(&self) -> Vec<crate::ipc::packet::ModelSummary> {
        let catalog = self.resolver.catalog();
        catalog
            .list_all()
            .await
            .iter()
            .map(model_summary_from_config)
            .collect()
    }
}

/// F7 fourteenth narrow handle: the port the `provider_templates`
/// IPC domain handler uses (T-109b). Trait lives in
/// `ipc::handlers::provider_templates`. Sync because the templates
/// are a `&'static [ProviderTemplate]` — no I/O, no locking, no
/// async work. Mirrors the credential-host shape (also a pure-read
/// surface).
impl crate::ipc::handlers::provider_templates::ModelTemplatesHost for AppState {
    fn list_templates(&self) -> Vec<crate::ipc::packet::ModelPresetInfo> {
        use peko_providers::catalog::ApiFormat;
        peko_providers::templates::iter_templates()
            .map(|t| crate::ipc::packet::ModelPresetInfo {
                id: t.id.to_string(),
                display_name: t.display_name.to_string(),
                api_type: match t.api_format {
                    ApiFormat::OpenaiCompletions => "openai",
                    ApiFormat::AnthropicMessages => "anthropic",
                    ApiFormat::OpenAiResponses => "responses",
                }
                .to_string(),
                base_url: t.base_url.to_string(),
                requires_key: t.requires_key,
                default_model: t.default_model.to_string(),
                models: t
                    .models
                    .iter()
                    .map(|m| crate::ipc::packet::ModelTemplateInfo {
                        id: m.id.to_string(),
                        display_name: m.display_name.map(str::to_string),
                        context_length: m.context_length,
                        max_output_tokens: m.max_output_tokens,
                    })
                    .collect(),
            })
            .collect()
    }
}

/// F7 fifteenth narrow handle: the port the `provider_add` IPC
/// domain handler uses (T-109b). Trait lives in
/// `ipc::handlers::provider_add`. Async because catalog mutations
/// (`ModelCatalog::upsert`) and the vault write are I/O. The body is
/// intentionally a 1:1 mirror of the CLI's `peko model add`
/// (`commands/model.rs:add_cmd`) — same template vs. custom branch,
/// same `--key` folding into an `llm`-namespace credential — so the
/// IPC and CLI surfaces never disagree on the resulting catalog state
/// (F6/F7 symmetry rule).
#[async_trait::async_trait]
impl crate::ipc::handlers::provider_add::ModelAddHost for AppState {
    async fn add_model(
        &self,
        args: crate::ipc::packet::ModelAddArgs,
    ) -> anyhow::Result<crate::ipc::packet::ModelSummary> {
        use crate::common::vault::{Credential, CredentialKind};
        use peko_providers::catalog::{ApiFormat, ModelConfig};
        use peko_providers::templates;
        use secrecy::SecretString;

        // The wire id the API expects. Required in both modes, same
        // as the CLI (`--model <wire-id>`).
        let model_id = args
            .model
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("--model <wire-id> is required"))?;
        if model_id.is_empty() {
            anyhow::bail!("--model must not be empty");
        }

        let mut entry: ModelConfig = if let Some(template_id) = args.template.as_deref() {
            // Template mode — mirror the CLI's `{template}-{model}`
            // default id and display-name override.
            let tmpl = templates::find_template(template_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown template '{template_id}'. Run `peko model templates` to list available ones."
                )
            })?;
            let id = args
                .name
                .clone()
                .unwrap_or_else(|| format!("{}-{model_id}", tmpl.id));
            let mut entry = ModelConfig::from_template(tmpl, id, model_id);
            if let Some(dn) = args.display_name.clone() {
                entry.display_name = dn;
            }
            entry
        } else if args.custom {
            // Custom mode — mirror the CLI's required-flag checks.
            let api_format_str = args
                .api_format
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!(
                    "--api-format is required with --custom (openai_completions | anthropic_messages)"
                ))?;
            let api_format = ApiFormat::from_wire(api_format_str)
                .ok_or_else(|| anyhow::anyhow!("unknown --api-format '{api_format_str}'"))?;
            let base_url = args
                .base_url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--base-url is required with --custom"))?;
            let id = args
                .name
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--name is required with --custom"))?;
            if id.is_empty() {
                anyhow::bail!("--name must not be empty");
            }
            ModelConfig {
                id: id.clone(),
                display_name: args.display_name.clone().unwrap_or_else(|| id.clone()),
                template_id: None,
                api_format,
                base_url,
                model_id,
                context_window: None,
                max_output_tokens: None,
                headers: Default::default(),
                credential_id: None,
                requires_key: args.requires_key.unwrap_or(true),
                enabled: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                compat: None,
                spec: None,
                // Phase 2 of `feature/multi-model-subagents`: the
                // IPC `models.upsert` accepts `note` via the CLI
                // `--note` flag; this branch is the `--custom`
                // non-template path where the IPC handler hasn't
                // surfaced a note yet (only the CLI's `model
                // add --custom --note ...` path does). `None`
                // preserves the pre-Phase-2 default; users edit
                // notes via `peko model edit --note ...`.
                note: None,
            }
        } else {
            // Bare-invocation guard. The handler also short-circuits
            // before calling the host, so reaching this branch means
            // a future caller (e.g. a direct host consumer) passed
            // through without going through the IPC handler. Keep
            // the same hint string so the surfaces stay symmetric.
            anyhow::bail!(
                "either --template <id> or --custom is required.\n\
                 \n\
                 Quick start:\n\
                   peko model add --template anthropic --model claude-sonnet-4-5 --key \"$ANTHROPIC_API_KEY\"\n\
                 \n\
                 List templates:\n\
                   peko model templates"
            );
        };

        // Fold in the optional key, same as the CLI: store the
        // material as a generic `llm`-namespace credential named
        // after the model and point the entry's `credential_id` at
        // it. Silently refused for key-less models (e.g. Ollama
        // presets) so a misconfigured request fails loudly instead
        // of storing an orphaned key.
        if let Some(cid) = args.credential_id.as_deref() {
            if cid.is_empty() {
                anyhow::bail!("--credential-id must not be empty");
            }
            if args.key.is_some() {
                anyhow::bail!("--key and --credential-id are mutually exclusive");
            }
            if self.vault.get_credential(cid).is_none() {
                anyhow::bail!("credential not found in vault: {cid}");
            }
            entry.credential_id = Some(cid.to_string());
        }
        if let Some(key) = args.key.as_deref() {
            if key.is_empty() {
                anyhow::bail!("--key must not be empty");
            }
            if !entry.requires_key {
                anyhow::bail!(
                    "--key supplied but model '{}' does not require a key",
                    entry.id
                );
            }
            let credential = Credential::now(
                "llm".to_string(),
                entry.id.clone(),
                CredentialKind::ApiKey,
                SecretString::from(key.to_string()),
            );
            let cid = credential.id.clone();
            self.vault.set_credential(&credential).map_err(|e| {
                anyhow::anyhow!("failed to store key for '{}' in vault: {e}", entry.id)
            })?;
            entry.credential_id = Some(cid);
        }

        // `self.resolver.catalog()` returns the daemon's in-memory
        // `Arc<ModelCatalog>` — same instance the resolver and every
        // other consumer read. Refuse to silently overwrite an
        // existing entry: catalog ids must be unique (they're how
        // principals and `peko send --model` reference the model),
        // so the user must use the Edit Model modal to mutate an
        // existing entry rather than discover the loss in a 401
        // later. `upsert` stamps `updated_at` and persists to
        // `models.toml` atomically, so the mutation is visible to
        // the next `ModelList` IPC call without a reload hop (the
        // CLI's `notify_daemon_reload` pattern doesn't apply — we
        // ARE the daemon).
        if self.resolver.catalog().get(&entry.id).await.is_some() {
            anyhow::bail!(
                "model id '{}' already exists. Open Edit Model to change it, or remove it first.",
                entry.id
            );
        }
        self.resolver.catalog().upsert(entry.clone()).await?;

        // Build the catalog-summary view the handler wraps in
        // `ResponsePacket::ModelAdded`. Uses the same projection as
        // the `ModelList` rows so the two surfaces stay in sync.
        Ok(model_summary_from_config(&entry))
    }
}

/// F7 fifteenth narrow handle (RP6): the port the `provider_edit`
/// IPC domain handler uses for catalog mutations after the initial
/// add. Trait lives in `ipc::handlers::provider_edit`. Async because
/// catalog reads/writes are I/O. Mirrors `peko model {remove, test}`
/// and adds the `model_update` path the desktop's "Edit Model"
/// modal needs. There is no default-model concept anymore — every
/// principal pins its own configured model.
#[async_trait::async_trait]
impl crate::ipc::handlers::provider_edit::ModelEditHost for AppState {
    async fn update_model(
        &self,
        args: crate::ipc::packet::ModelUpdateArgs,
    ) -> anyhow::Result<crate::ipc::packet::ModelSummary> {
        use peko_providers::catalog::ApiFormat;

        let catalog = self.resolver.catalog();
        let mut entry = catalog
            .get(&args.id)
            .await
            .ok_or_else(|| anyhow::anyhow!("model '{}' not found in catalog", args.id))?;

        if let Some(display_name) = args.display_name {
            entry.display_name = display_name;
        }
        if let Some(base_url) = args.base_url {
            if base_url.is_empty() {
                anyhow::bail!("base_url must not be empty");
            }
            entry.base_url = base_url;
        }
        if let Some(api_format_str) = args.api_format {
            entry.api_format = ApiFormat::from_wire(&api_format_str)
                .ok_or_else(|| anyhow::anyhow!("unknown api_format '{api_format_str}'"))?;
        }
        if let Some(model_id) = args.model_id {
            if model_id.is_empty() {
                anyhow::bail!("model_id must not be empty");
            }
            entry.model_id = model_id;
        }
        if let Some(context_window) = args.context_window {
            entry.context_window = Some(context_window);
        }
        if let Some(max_output_tokens) = args.max_output_tokens {
            entry.max_output_tokens = Some(max_output_tokens);
        }
        if let Some(headers) = args.headers {
            entry.headers = headers;
        }
        if let Some(credential_id) = args.credential_id {
            // Empty string clears the reference (e.g. the user
            // detached a key); a non-empty value must point at an
            // existing vault credential.
            if credential_id.is_empty() {
                entry.credential_id = None;
            } else {
                if self.vault.get_credential(&credential_id).is_none() {
                    anyhow::bail!("credential not found in vault: {credential_id}");
                }
                entry.credential_id = Some(credential_id);
            }
        }
        if let Some(requires_key) = args.requires_key {
            entry.requires_key = requires_key;
        }
        if let Some(enabled) = args.enabled {
            entry.enabled = enabled;
        }

        // `upsert` stamps `updated_at` and persists atomically.
        catalog.upsert(entry.clone()).await?;

        Ok(model_summary_from_config(&entry))
    }

    async fn remove_model(&self, id: &str) -> anyhow::Result<bool> {
        self.resolver.catalog().remove(id).await
    }

    async fn model_test(
        &self,
        id: &str,
    ) -> anyhow::Result<peko_providers::validator::CredentialTestOutcome> {
        let entry = self
            .resolver
            .catalog()
            .get(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("model not found in catalog: {id}"))?;

        // Resolve the credential material from the vault, if the
        // entry references one — same flow as `peko model test`.
        let api_key = match &entry.credential_id {
            Some(cid) => {
                let credential = self
                    .vault
                    .get_credential(cid)
                    .ok_or_else(|| anyhow::anyhow!("credential not found in vault: {cid}"))?;
                Some(credential.material)
            }
            None => None,
        };

        let outcome = peko_providers::validator::Validator::test(&entry, api_key.as_ref()).await;

        // Record the outcome on the credential so `credential list`
        // shows the last-tested marker.
        if let Some(cid) = &entry.credential_id {
            if let Err(e) = self.vault.record_test(cid, outcome.ok) {
                tracing::warn!(credential_id = %cid, error = %e, "failed to record credential test outcome");
            }
        }
        Ok(outcome)
    }
}

/// Fix D: the port the `persona` IPC domain handler uses. Trait
/// lives in `ipc::handlers::persona`. The handler is a thin wrapper
/// around `Provider::chat_with_system`; the daemon-side impl
/// resolves the model via the shared `LlmResolver`, builds a
/// one-shot Provider, and returns the raw LLM text.
#[async_trait::async_trait]
impl crate::ipc::handlers::persona::PersonaHost for AppState {
    async fn draft_persona(
        &self,
        model_id: String,
        system: String,
        from: String,
    ) -> anyhow::Result<String> {
        let entry = self
            .resolver
            .catalog()
            .get(&model_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("model not found in catalog: {model_id}"))?;
        if !entry.enabled {
            anyhow::bail!("model '{model_id}' is disabled");
        }
        let provider = self.resolver.build_provider(&entry).await?;
        provider
            .chat_with_system(Some(&system), &from, &model_id, 0.7)
            .await
    }
}

/// ADR-046 trust+audit: the port the `audit` IPC domain handler
/// uses. Sync because reading the in-memory audit ring buffer is
/// just an Arc clone over the bounded VecDeque — the daemon's
/// `get_audit_log` is internally cheap. (The handler itself is
/// async because `RequestHandler::handle` is async, but the host
/// port here is sync to match the read-only `get_audit_log` API.)
impl crate::ipc::handlers::audit::AuditHost for AppState {
    fn observability(&self) -> Arc<Observability> {
        Arc::clone(&self.observability)
    }
}

/// F7 thirteenth narrow handle: the port the `credential` IPC domain
/// handler uses. Trait lives in `ipc::handlers::credential`. Sync
/// because reading the in-memory vault (`Vault::list_providers` +
/// `Vault::get_provider_key`) is a pure in-memory operation — no
/// CredentialHost implementation for the daemon. Translates between
/// the generic vault API (`Vault::list_credentials`,
/// `Vault::set_credential`, `Vault::delete_credential`) and the IPC
/// wire types in `crate::ipc::packet`.
///
/// PR 3 / `feature/model-first-config` adds the catalog join used to
/// populate `CredentialRow::is_referenced` / `referenced_by`, the
/// `force` flag on `delete_credential`, and the `replace_on` flag on
/// `set_credential`. The catalog is read through
/// `peko_providers::catalog::ModelCatalog` — the same instance the
/// resolver uses, so any mutation is reflected here without a reload.
#[async_trait::async_trait]
impl crate::ipc::handlers::credential::CredentialHost for AppState {
    fn list_credentials(
        &self,
        namespace: Option<&str>,
        kind: Option<crate::common::vault::CredentialKind>,
        include_system: bool,
    ) -> Vec<crate::ipc::packet::CredentialRow> {
        let filter = crate::common::vault::CredentialFilter {
            namespace: namespace.map(String::from),
            kind,
            include_system,
        };
        self.vault
            .list_credentials(&filter)
            .into_iter()
            .map(|summary| crate::ipc::packet::CredentialRow {
                id: summary.id,
                namespace: summary.namespace,
                name: summary.name,
                kind: summary.kind.as_str().to_string(),
                has_key: summary.has_key,
                last_tested_at: summary.last_tested_at.map(|dt| dt.to_rfc3339()),
                last_tested_ok: summary.last_tested_ok,
                system_owned: summary.system_owned,
                // `is_referenced` / `referenced_by` populated by the
                // handler from `credential_references()` after this
                // returns, so the row stays "flat" here.
                is_referenced: false,
                referenced_by: Vec::new(),
            })
            .collect()
    }

    fn get_credential(&self, id: &str) -> Option<crate::ipc::packet::Credential> {
        self.vault
            .get_credential(id)
            .map(|c| crate::ipc::packet::Credential {
                system_owned: c.is_system_owned(),
                id: c.id,
                namespace: c.namespace,
                name: c.name,
                kind: c.kind.as_str().to_string(),
                metadata: c.metadata,
                created_at: c.created_at.to_rfc3339(),
                updated_at: c.updated_at.to_rfc3339(),
                last_tested_at: c.last_tested_at.map(|dt| dt.to_rfc3339()),
                last_tested_ok: c.last_tested_ok,
            })
    }

    fn get_credential_material(&self, id: &str) -> Option<SecretString> {
        self.vault.get_credential(id).map(|c| c.material)
    }

    async fn set_credential(
        &self,
        namespace: &str,
        name: &str,
        kind: crate::common::vault::CredentialKind,
        material: &secrecy::SecretString,
        metadata: Option<serde_json::Value>,
        replace_on: Option<&str>,
    ) -> anyhow::Result<(String, u32)> {
        // Reuse the existing credential id when overwriting the same
        // (namespace, name) slot so rotation bindings remain valid.
        let mut credential = crate::common::vault::Credential::now(
            namespace.to_string(),
            name.to_string(),
            kind,
            material.clone(),
        );
        credential.metadata = metadata.unwrap_or_default();
        if let Some(existing) = self
            .vault
            .list_credentials(&crate::common::vault::CredentialFilter {
                namespace: Some(namespace.to_string()),
                kind: None,
                include_system: true,
            })
            .into_iter()
            .find(|s| s.name == name)
        {
            credential.id = existing.id;
        }
        let new_id = credential.id.clone();
        self.vault.set_credential(&credential).map_err(|e| {
            anyhow::anyhow!("vault refused to store credential '{namespace}/{name}': {e}")
        })?;

        // PR 3: bulk-rewire dependents when `--replace-on` was
        // supplied. Only count rewires that actually happened;
        // unknown `old_id` is a no-op (rewired_models = 0).
        let rewired = if let Some(old_id) = replace_on {
            if old_id == new_id {
                0
            } else {
                self.resolver
                    .catalog()
                    .rewire_credential(old_id, &new_id)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "vault stored credential but catalog rewire failed: {e}"
                        )
                    })?
            }
        } else {
            0
        };

        Ok((new_id, rewired as u32))
    }

    async fn delete_credential(
        &self,
        id: &str,
        force: bool,
    ) -> anyhow::Result<crate::ipc::handlers::credential::CredentialDeleteOutcome> {
        // PR 3: refuse if dependents exist and force is false.
        let dependents = self
            .resolver
            .catalog()
            .models_referencing(id)
            .await
            .into_iter()
            .map(|e| model_summary_from_config(&e))
            .collect::<Vec<_>>();
        if !dependents.is_empty() && !force {
            return Ok(
                crate::ipc::handlers::credential::CredentialDeleteOutcome::InUse { dependents },
            );
        }

        // Detach dependents (force path) before deleting the vault
        // record so a partially-failed delete can't strand a model
        // pointing at a missing credential.
        let mut detached = 0u32;
        if !dependents.is_empty() {
            let entries = self.resolver.catalog().list_all().await;
            for mut entry in entries {
                if entry.credential_id.as_deref() == Some(id) {
                    entry.credential_id = None;
                    entry.updated_at = chrono::Utc::now();
                    if let Err(e) = self.resolver.catalog().upsert(entry).await {
                        // Surface the catalog failure so the caller
                        // can retry — never silently leak orphans.
                        return Err(anyhow::anyhow!(
                            "catalog detach failed during credential delete: {e}"
                        ));
                    }
                    detached += 1;
                }
            }
        }

        self.vault
            .delete_credential(id)
            .map_err(|e| anyhow::anyhow!("vault refused to delete credential '{id}': {e}"))?;

        Ok(
            crate::ipc::handlers::credential::CredentialDeleteOutcome::Removed {
                broken_references: detached,
            },
        )
    }

    async fn credential_references(&self) -> HashMap<String, Vec<crate::ipc::packet::ModelSummary>> {
        let mut out: HashMap<String, Vec<crate::ipc::packet::ModelSummary>> = HashMap::new();
        // Best-effort join: if the catalog is corrupt or unloaded we
        // just don't decorate rows. The handler will still paint a
        // list, just without the dependents badge.
        for entry in self.resolver.catalog().list_all().await {
            if let Some(cid) = entry.credential_id.clone() {
                out.entry(cid)
                    .or_default()
                    .push(model_summary_from_config(&entry));
            }
        }
        out
    }
}

impl crate::ipc::handlers::credential::BindingHost for AppState {
    fn list_bindings(&self) -> Vec<crate::ipc::packet::RotationBindingWire> {
        self.vault
            .list_bindings()
            .into_iter()
            .map(|(key, binding)| crate::ipc::packet::RotationBindingWire {
                key,
                strategy: binding.strategy.as_str().to_string(),
                order: binding.ordered_credential_ids,
            })
            .collect()
    }

    fn get_binding(&self, key: &str) -> Option<crate::ipc::packet::RotationBindingWire> {
        self.vault
            .list_bindings()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(key, binding)| crate::ipc::packet::RotationBindingWire {
                key,
                strategy: binding.strategy.as_str().to_string(),
                order: binding.ordered_credential_ids,
            })
    }

    fn set_binding(
        &self,
        key: &str,
        strategy: crate::common::vault::RotationStrategy,
        order: Vec<String>,
    ) -> anyhow::Result<()> {
        let binding = crate::common::vault::RotationBinding {
            strategy,
            ordered_credential_ids: order,
        };
        self.vault
            .set_binding(key, &binding)
            .map_err(|e| anyhow::anyhow!("vault refused to store binding '{key}': {e}"))
    }

    fn delete_binding(&self, key: &str) -> anyhow::Result<bool> {
        self.vault
            .delete_binding(key)
            .map_err(|e| anyhow::anyhow!("vault refused to delete binding '{key}': {e}"))
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
    ) -> Arc<std::sync::Mutex<std::collections::HashMap<u64, StreamingRunHandle>>> {
        AppState::streaming_runs(self)
    }

    fn inbox_registry(&self) -> &Arc<peko_session::InboxRegistry> {
        &self.inbox_registry
    }

    fn extension_store(&self) -> &Arc<ExtensionStore> {
        AppState::extension_store(self)
    }

    fn trust_store(&self) -> &Arc<tokio::sync::RwLock<crate::registry::packaging::TrustStore>> {
        AppState::trust_store(self)
    }

    fn config_dir(&self) -> std::path::PathBuf {
        self.config_dir.clone()
    }

    fn path_resolver(&self) -> crate::common::paths::PathResolver {
        // Phase A: hand the typed resolver through so the IPC
        // handlers can reach `principal_layout(name).shared.agents_dir`
        // and friends without re-deriving them from
        // `config_dir()` / `data_dir()`.
        self.path_resolver.clone()
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

    fn runtime_signing_key(&self) -> Arc<ed25519_dalek::SigningKey> {
        Arc::clone(&self.runtime_signing_key)
    }

    fn invite_revocation_set(&self) -> Arc<crate::tunnel::InviteRevocationSet> {
        Arc::clone(&self.invite_revocation_set)
    }

    fn pekohub_base_url(&self) -> String {
        // PekoConfig has no `tunnel` field today; the public hub URL
        // is operator-supplied via the `PEKOHUB_BASE_URL` env var.
        std::env::var("PEKOHUB_BASE_URL")
            .ok()
            .map(|s| s.trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://pekohub.org".to_string())
    }

    fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        // Production `AppState` was constructed with
        // `RuntimeAuthority::for_runtime(...)` so its actor is
        // `Subject::Public`. The IPC admission layer is responsible
        // for projecting a caller subject into the per-tier read
        // paths the handler actually invokes.
        &self.authority
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
        let tools = core.list_tools(peko_subject::PrincipalId::system()).await;
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
        let tools: Vec<crate::extensions::framework::types::ToolMetadata> =
            core.list_tools(peko_subject::PrincipalId::system()).await;
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        assert!(
            tool_names.contains(&"Bash".to_string()),
            "Bash missing after agent init"
        );
        assert!(
            tool_names.contains(&"Grep".to_string()),
            "Grep missing after agent init"
        );

        // Wire-format catalog should expose Bash and Grep under the
        // `tool:*` grant. F36 removed the `## Available Tools` prose
        // section; tool catalogs now travel on the wire as the `tools[]`
        // JSON-schema array. Use the principal-aware allowlist helper so
        // the capability gate sees the wildcard grant for built-ins.
        let caps = peko_extension_api::Capabilities::with_grants(["tool:*"]);
        let defs = core
            .list_tool_definitions_with_allowlist(&caps, None, peko_subject::PrincipalId::system())
            .await;
        let def_names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
        assert!(
            def_names.contains(&"Bash".to_string()),
            "Bash missing from wire catalog: {def_names:?}"
        );
        assert!(
            def_names.contains(&"Grep".to_string()),
            "Grep missing from wire catalog: {def_names:?}"
        );
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
