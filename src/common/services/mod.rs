//! Common services for CLI and API
//!
//! This module provides business logic services that can be used by both
//! CLI commands and API routes, ensuring consistent behavior across interfaces.

pub mod agent_service;
pub mod agent_validator;
// ADR-016: message_service and session_resolver removed - use StatelessAgentService directly
pub mod session_service;
pub mod team_management_service;
pub mod team_service;

// ConfigAuthority - the new central config system
pub mod config_authority;
pub use config_authority::{AgentConfigEntry, ConfigAuthority, ConfigAuthorityImpl, ConfigSource};

pub use agent_service::AgentService;
pub use agent_validator::AgentValidator;
// ADR-016: message_service and session_resolver removed - use StatelessAgentService directly
pub use session_service::{
    BranchResult, HistoryEvent, HistoryQuery, HistoryResult, HistorySummary, SessionDetails,
    SessionInfo, SessionService,
};
pub use team_management_service::TeamManagementService;
pub use team_service::TeamService;

// Note: ConfigSource is now exported from config_authority module above
// For backward compatibility, config_registry::ConfigSource is re-exported via agent::mod.rs

use crate::common::paths::PathResolver;
use std::sync::Arc;

/// Container for all common services
///
/// This provides a convenient way to access all services from a single struct,
/// useful for dependency injection in both CLI and API contexts.
#[derive(Debug, Clone)]
pub struct ServiceRegistry {
    agent: AgentService,
    agent_config: ConfigAuthorityImpl,
    team: TeamService,
    team_management: Option<TeamManagementService>,
}

impl ServiceRegistry {
    /// Create a new service registry with the given path resolver
    ///
    /// This is the CLI entry point - it doesn't include runtime services.
    #[must_use]
    pub fn new(resolver: PathResolver) -> Self {
        Self {
            agent: AgentService::new(resolver.clone()),
            agent_config: ConfigAuthorityImpl::new(resolver.clone()),
            team: TeamService::new(resolver),
            team_management: None,
        }
    }

    /// Create a new service registry with runtime services
    ///
    /// This is the API entry point - it includes runtime services like `TeamManager`.
    #[must_use]
    pub fn with_runtime(
        resolver: PathResolver,
        runtime_manager: Arc<crate::team::TeamManager>,
    ) -> Self {
        let config_service = TeamService::new(resolver.clone());
        let team_management =
            TeamManagementService::new(config_service.clone(), runtime_manager, resolver.clone());

        Self {
            agent: AgentService::new(resolver.clone()),
            agent_config: ConfigAuthorityImpl::new(resolver.clone()),
            team: config_service,
            team_management: Some(team_management),
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
    ///
    /// Returns None if the registry was created without runtime support.
    pub fn team_management(&self) -> Option<&TeamManagementService> {
        self.team_management.as_ref()
    }
}
