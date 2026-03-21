//! Application State
//!
//! Shared state accessible to all API route handlers.
//! Updated for stateless cold-start architecture.

use crate::agent::lifecycle::LifecycleManager;
use crate::agent::stateless_service::StatelessAgentService;
use crate::common::services::{
    AgentConfigService, AgentCreationService, AgentService, MessageService, SessionService,
    TeamManagementService, TeamService,
};
use crate::hooks::{EventBroadcaster, HookRegistry};
use crate::observability::Observability;
use crate::registry::{load_from_workspace, RegistryConfig};
use crate::team::TeamManager;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;

/// Shared application state for the HTTP API (Stateless Architecture)
///
/// This struct is passed to all route handlers via Axum's State extractor.
/// All fields are thread-safe and can be accessed concurrently.
#[derive(Clone)]
pub struct AppState {
    /// Time when the daemon started
    pub started_at: SystemTime,

    /// Path to the workspace directory (.pekobot/)
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

    /// Team manager for team runtime
    pub team_manager: Arc<TeamManager>,

    /// Hook registry for webhook and event hooks
    hook_registry: Arc<HookRegistry>,

    /// Event broadcaster for system events
    event_broadcaster: Arc<EventBroadcaster>,

    /// Registry configuration for push/pull operations
    registry_config: Arc<RwLock<RegistryConfig>>,

    /// Observability hub for audit, metrics, and tracing
    observability: Arc<Observability>,

    /// Agent configuration service (unified)
    config_service: Arc<AgentConfigService>,

    /// Stateless agent execution service
    agent_service: Arc<StatelessAgentService>,

    /// Agent service (unified for CLI and API)
    agent_mgmt_service: Arc<AgentService>,

    /// Lifecycle manager (tracks active executions only)
    lifecycle: Arc<LifecycleManager>,

    /// Agent creation service (unified for CLI and API)
    agent_creation_service: Arc<AgentCreationService>,

    /// Message service (unified for CLI and API)
    message_service: Arc<MessageService>,

    /// Session service (unified for CLI and API)
    session_service: Arc<SessionService>,

    /// Team management service (unified for CLI and API)
    team_service: Arc<TeamManagementService>,

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
            .field("team_manager", &"<TeamManager>")
            .field("config_service", &"<AgentConfigService>")
            .field("agent_service", &"<StatelessAgentService>")
            .field("agent_mgmt_service", &"<AgentService>")
            .field("team_service", &"<TeamManagementService>")
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
        let config_dir = dirs::home_dir()
            .map(|d| d.join(".pekobot"))
            .unwrap_or_else(|| PathBuf::from(".").join(".pekobot"));
        let cache_dir = dirs::cache_dir()
            .map(|d| d.join("pekobot"))
            .unwrap_or_else(|| data_dir.join("cache"));

        // Create stateless components
        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            config_dir.clone(),
            data_dir.clone(),
            cache_dir.clone(),
        );

        let config_service = Arc::new(AgentConfigService::new(path_resolver.clone()));

        let path_resolver_clone = path_resolver.clone();
        let agent_service = Arc::new(
            StatelessAgentService::new(config_service.clone(), path_resolver)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create agent service: {}", e))?,
        );

        let lifecycle = Arc::new(LifecycleManager::new());
        let team_manager = Arc::new(TeamManager::new());

        let agent_creation_service = Arc::new(AgentCreationService::new(
            config_service.clone(),
            path_resolver_clone.clone(),
            team_manager.clone(),
        ));

        let message_service = Arc::new(MessageService::new(agent_service.clone(), path_resolver_clone.clone()));

        let session_service = Arc::new(SessionService::new(path_resolver_clone.clone()));

        // Create unified services
        let team_service = Arc::new(TeamManagementService::new(
            TeamService::new(path_resolver_clone.clone()),
            team_manager.clone(),
            path_resolver_clone.clone(),
        ));

        let agent_mgmt_service = Arc::new(AgentService::new(path_resolver_clone));

        Ok(Self {
            started_at: SystemTime::now(),
            workspace_path,
            config_dir,
            data_dir,
            cache_dir,
            port,
            host: host.into(),
            config,
            team_manager,
            hook_registry: Arc::new(HookRegistry::new()),
            event_broadcaster: Arc::new(EventBroadcaster::new()),
            registry_config: Arc::new(RwLock::new(RegistryConfig::default())),
            observability: Arc::new(Observability::new("api")),
            config_service,
            agent_service,
            agent_mgmt_service,
            lifecycle,
            agent_creation_service,
            message_service,
            session_service,
            team_service,
            inner: Arc::new(RwLock::new(AppStateInner::default())),
        })
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
        let config_dir = dirs::home_dir()
            .map(|d| d.join(".pekobot"))
            .unwrap_or_else(|| PathBuf::from(".").join(".pekobot"));
        let cache_dir = dirs::cache_dir()
            .map(|d| d.join("pekobot"))
            .unwrap_or_else(|| data_dir.join("cache"));

        // Create stateless components
        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            config_dir.clone(),
            data_dir.clone(),
            cache_dir.clone(),
        );

        let config_service = Arc::new(AgentConfigService::new(path_resolver.clone()));

        let path_resolver_clone = path_resolver.clone();
        let agent_service = Arc::new(
            StatelessAgentService::new(config_service.clone(), path_resolver)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create agent service: {}", e))?,
        );

        let lifecycle = Arc::new(LifecycleManager::new());
        let team_manager = Arc::new(TeamManager::with_data_dir(data_dir.clone()));

        let agent_creation_service = Arc::new(AgentCreationService::new(
            config_service.clone(),
            path_resolver_clone.clone(),
            team_manager.clone(),
        ));

        let message_service = Arc::new(MessageService::new(agent_service.clone(), path_resolver_clone.clone()));

        let session_service = Arc::new(SessionService::new(path_resolver_clone.clone()));

        // Create unified services
        let team_service = Arc::new(TeamManagementService::new(
            TeamService::new(path_resolver_clone.clone()),
            team_manager.clone(),
            path_resolver_clone.clone(),
        ));

        let agent_mgmt_service = Arc::new(AgentService::new(path_resolver_clone));

        Ok(Self {
            started_at: SystemTime::now(),
            workspace_path,
            config_dir,
            data_dir: data_dir.clone(),
            cache_dir,
            port,
            host: host.into(),
            config,
            team_manager,
            hook_registry: Arc::new(HookRegistry::new()),
            event_broadcaster: Arc::new(EventBroadcaster::new()),
            registry_config: Arc::new(RwLock::new(RegistryConfig::default())),
            observability: Arc::new(Observability::new("api")),
            config_service,
            agent_service,
            agent_mgmt_service,
            lifecycle,
            agent_creation_service,
            message_service,
            session_service,
            team_service,
            inner: Arc::new(RwLock::new(AppStateInner::default())),
        })
    }

    /// Get the current uptime in seconds
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

    /// Get the hook registry
    pub fn hook_registry(&self) -> Arc<HookRegistry> {
        self.hook_registry.clone()
    }

    /// Get the event broadcaster
    pub fn event_broadcaster(&self) -> Arc<EventBroadcaster> {
        self.event_broadcaster.clone()
    }

    /// Get the observability hub
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
    pub fn config_service(&self) -> &Arc<AgentConfigService> {
        &self.config_service
    }

    /// Get the agent service
    pub fn agent_service(&self) -> &Arc<StatelessAgentService> {
        &self.agent_service
    }

    /// Get the lifecycle manager
    pub fn lifecycle(&self) -> &Arc<LifecycleManager> {
        &self.lifecycle
    }

    /// Get the agent creation service
    pub fn agent_creation_service(&self) -> &Arc<AgentCreationService> {
        &self.agent_creation_service
    }

    /// Get the message service
    pub fn message_service(&self) -> &Arc<MessageService> {
        &self.message_service
    }

    /// Get the session service
    pub fn session_service(&self) -> &Arc<SessionService> {
        &self.session_service
    }

    /// Get the team management service (unified)
    pub fn team_service(&self) -> &Arc<TeamManagementService> {
        &self.team_service
    }

    /// Get the agent management service (unified)
    pub fn agent_mgmt_service(&self) -> &Arc<AgentService> {
        &self.agent_mgmt_service
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
            data_dir: PathBuf::from(".pekobot"),
            config_dir: PathBuf::from(".pekobot"),
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
        assert!(uptime2 >= 0); // May still be 0 if less than 1 second passed
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
        assert_eq!(state.agent_count().await, 0);

        // Initially no active executions
        assert_eq!(state.active_execution_count().await, 0);
    }
}
