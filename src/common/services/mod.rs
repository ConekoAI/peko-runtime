//! Common services for CLI and API
//!
//! This module provides business logic services that can be used by both
//! CLI commands and API routes, ensuring consistent behavior across interfaces.

pub mod agent_config_builder;
pub mod agent_service;
pub mod team_service;

pub use agent_service::AgentService;
pub use team_service::TeamService;

use crate::common::paths::PathResolver;

/// Container for all common services
///
/// This provides a convenient way to access all services from a single struct,
/// useful for dependency injection in both CLI and API contexts.
#[derive(Debug, Clone)]
pub struct ServiceRegistry {
    agent: AgentService,
    team: TeamService,
}

impl ServiceRegistry {
    /// Create a new service registry with the given path resolver
    pub fn new(resolver: PathResolver) -> Self {
        Self {
            agent: AgentService::new(resolver.clone()),
            team: TeamService::new(resolver),
        }
    }

    /// Get the agent service
    pub fn agent(&self) -> &AgentService {
        &self.agent
    }

    /// Get the team service
    pub fn team(&self) -> &TeamService {
        &self.team
    }
}
