//! Common services for CLI and API
//!
//! This module provides business logic services that can be used by both
//! CLI commands and API routes, ensuring consistent behavior across interfaces.

pub mod agent_service;
pub mod credentials_service;
pub mod daemon_process_service;
// ADR-016: message_service and session_resolver removed - use StatelessAgentService directly
pub mod extension_management_service;
pub mod session_service;
pub mod team_management_service;
pub mod team_service;

// ConfigAuthority - the new central config system
pub mod config_authority;
pub use config_authority::{AgentConfigEntry, ConfigAuthority, ConfigAuthorityImpl, ConfigSource};

pub use agent_service::AgentService;
pub use credentials_service::CredentialsService;
pub use daemon_process_service::{DaemonProcessService, DaemonStatus};
pub use extension_management_service::ExtensionManagementService;
// ADR-016: message_service and session_resolver removed - use StatelessAgentService directly
pub use session_service::{
    BranchResult, HistoryEvent, HistoryQuery, HistoryResult, HistorySummary, SessionDetails,
    SessionInfo, SessionService,
};
pub use team_management_service::TeamManagementService;
pub use team_service::TeamService;

/// Backward-compatible alias for `ServiceContainer`.
#[deprecated(since = "0.2.0", note = "Use ServiceContainer instead")]
pub type ServiceRegistry = ServiceContainer;

// Note: ConfigSource is now exported from config_authority module above
// For backward compatibility, config_registry::ConfigSource is re-exported via agent::mod.rs

use crate::common::paths::PathResolver;

/// Container for all common services
///
/// This provides a convenient way to access all services from a single struct,
/// useful for dependency injection in both CLI and API contexts.
#[derive(Debug, Clone)]
pub struct ServiceContainer {
    agent: AgentService,
    agent_config: ConfigAuthorityImpl,
    team: TeamService,
    team_management: TeamManagementService,
    extension_management: ExtensionManagementService,
}

impl ServiceContainer {
    /// Create a new service container with the given path resolver
    #[must_use]
    pub fn new(resolver: PathResolver) -> Self {
        let team_service = TeamService::new(resolver.clone());
        Self {
            agent: AgentService::new(resolver.clone()),
            agent_config: ConfigAuthorityImpl::new(resolver.clone()),
            team: team_service.clone(),
            team_management: TeamManagementService::new(team_service.clone(), resolver.clone()),
            extension_management: ExtensionManagementService::new(resolver),
        }
    }

    /// Get the agent service
    pub fn agent(&self) -> &AgentService {
        &self.agent
    }

    /// Get the agent configuration service
    pub fn agent_config(&self) -> &ConfigAuthorityImpl {
        &self.agent_config
    }

    /// Get the team service (filesystem operations)
    pub fn team(&self) -> &TeamService {
        &self.team
    }

    /// Get the team management service (unified operations)
    pub fn team_management(&self) -> &TeamManagementService {
        &self.team_management
    }

    /// Get the extension management service (registry push/pull)
    pub fn extension_management(&self) -> &ExtensionManagementService {
        &self.extension_management
    }
}
