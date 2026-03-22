//! Common services for CLI and API
//!
//! This module provides business logic services that can be used by both
//! CLI commands and API routes, ensuring consistent behavior across interfaces.

pub mod agent_config_builder;
pub mod agent_config_service;
pub mod agent_creation_service;
pub mod agent_service;
pub mod auth_resolver;
pub mod message_service;
pub mod session_resolver;
pub mod session_service;
pub mod team_management_service;
pub mod team_service;

pub use agent_config_builder::{build_config_with_auth, build_default_config, AgentConfigBuilder};
pub use agent_config_service::{AgentConfigEntry, AgentConfigService};
// AgentCreationService is deprecated - use AgentService::create_agent() instead
// pub use agent_creation_service::...;
pub use agent_service::AgentService;
pub use auth_resolver::{AuthResolver, DirectAuthResolver, FilesystemAuthResolver};
pub use message_service::{ChatEvent, MessageRequest, MessageResult, MessageService, ToolCallInfo};
pub use session_resolver::{ResolutionStrategy, SessionResolver};
pub use session_service::{
    BranchResult, HistoryEvent, HistoryQuery, HistoryResult, HistorySummary, SessionDetails,
    SessionInfo, SessionService,
};
pub use team_management_service::TeamManagementService;
pub use team_service::TeamService;

// Re-export config source type from config_registry for backward compatibility
pub use crate::agent::config_registry::ConfigSource;

use crate::common::paths::PathResolver;
use std::sync::Arc;

/// Container for all common services
///
/// This provides a convenient way to access all services from a single struct,
/// useful for dependency injection in both CLI and API contexts.
#[derive(Debug, Clone)]
pub struct ServiceRegistry {
    agent: AgentService,
    team: TeamService,
    team_management: Option<TeamManagementService>,
}

impl ServiceRegistry {
    /// Create a new service registry with the given path resolver
    ///
    /// This is the CLI entry point - it doesn't include runtime services.
    pub fn new(resolver: PathResolver) -> Self {
        Self {
            agent: AgentService::new(resolver.clone()),
            team: TeamService::new(resolver),
            team_management: None,
        }
    }

    /// Create a new service registry with runtime services
    ///
    /// This is the API entry point - it includes runtime services like TeamManager.
    pub fn with_runtime(
        resolver: PathResolver,
        runtime_manager: Arc<crate::team::TeamManager>,
    ) -> Self {
        let config_service = TeamService::new(resolver.clone());
        let team_management =
            TeamManagementService::new(config_service.clone(), runtime_manager, resolver.clone());

        Self {
            agent: AgentService::new(resolver.clone()),
            team: config_service,
            team_management: Some(team_management),
        }
    }

    /// Get the agent service
    pub fn agent(&self) -> &AgentService {
        &self.agent
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
