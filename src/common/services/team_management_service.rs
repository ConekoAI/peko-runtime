//! Team Management Service
//!
//! Provides unified team operations for both CLI and HTTP API.
//!
//! This service combines filesystem operations (`TeamService`) with runtime
//! operations (`TeamRuntimeManager`) to provide a single, consistent interface
//! for all team-related operations.
//!
//! ## Architecture
//!
//! - Configuration operations (create, list, delete teams) use `TeamService`
//! - Runtime operations (deploy, stop, scale) use `TeamRuntimeManager`
//! - Both CLI and API use this unified service

use crate::common::identifiers::{validate_team_name, ValidationError};
use crate::common::paths::PathResolver;
use crate::common::services::TeamService;
use crate::common::types::team::{
    TeamConfigSource, TeamCreationResult, TeamDeletionResult, TeamDeployRequest, TeamDeployResult,
    TeamInfo, TeamRuntimeInfo, TeamRuntimeStatus, TeamScaleRequest, TeamScaleResult,
};
use crate::daemon::state::AppState;
use crate::team::config::TeamConfig;
use crate::team::{TeamManager, TeamStatus};
use anyhow::{Context, Result};
use std::sync::Arc;

/// Unified team management service
///
/// This is the single entry point for all team operations from both
/// CLI and HTTP API interfaces.
#[derive(Clone)]
pub struct TeamManagementService {
    /// Filesystem-based team operations
    config_service: TeamService,
    /// Runtime team manager (for deploy/stop/scale)
    runtime_manager: Arc<TeamManager>,
    /// Path resolver
    resolver: PathResolver,
}

impl std::fmt::Debug for TeamManagementService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamManagementService")
            .field("config_service", &self.config_service)
            .field("resolver", &self.resolver)
            .finish_non_exhaustive()
    }
}

impl TeamManagementService {
    /// Create a new team management service
    #[must_use]
    pub fn new(
        config_service: TeamService,
        runtime_manager: Arc<TeamManager>,
        resolver: PathResolver,
    ) -> Self {
        Self {
            config_service,
            runtime_manager,
            resolver,
        }
    }

    // ============================================================================
    // Configuration Operations (used by both CLI and API)
    // ============================================================================

    /// Create a new team
    ///
    /// Creates the team directory structure and metadata file.
    /// Used by both CLI (`pekobot team create`) and API (`POST /teams`).
    pub async fn create_team(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<TeamCreationResult> {
        self.config_service.create_team(name, description).await
    }

    /// List all teams
    ///
    /// Returns information about all teams from the filesystem.
    /// Used by both CLI (`pekobot team list`) and API (`GET /teams`).
    pub async fn list_teams(&self) -> Result<Vec<TeamInfo>> {
        self.config_service.list_teams().await
    }

    /// Get team information
    ///
    /// Returns detailed information about a specific team.
    /// Used by both CLI (`pekobot team show`) and API (`GET /teams/{id}`).
    pub async fn get_team(&self, name: &str) -> Result<Option<TeamInfo>> {
        self.config_service.get_team(name).await
    }

    /// Get agents in a team
    ///
    /// Returns all agents belonging to a team with their configurations.
    pub async fn get_team_agents(
        &self,
        name: &str,
    ) -> Result<Vec<(String, crate::types::agent::AgentConfig)>> {
        self.config_service.get_team_agents(name).await
    }

    /// Delete a team
    ///
    /// Removes the team directory and all its agents.
    /// Used by both CLI (`pekobot team delete`) and API (`DELETE /teams/{id}`).
    pub async fn delete_team(&self, name: &str) -> Result<TeamDeletionResult> {
        // First, check if there's a running runtime for this team
        // TODO: In the future, we might want to track team name -> runtime ID mapping

        self.config_service.delete_team(name).await
    }

    /// Check if a team exists
    #[must_use]
    pub fn team_exists(&self, name: &str) -> bool {
        self.config_service.team_exists(name)
    }

    // ============================================================================
    // Runtime Operations (primarily used by API)
    // ============================================================================

    /// Deploy a team from configuration
    ///
    /// Creates a runtime team instance from a team configuration.
    /// Used by API (`POST /teams` with inline config or file path).
    pub async fn deploy_runtime(
        &self,
        request: TeamDeployRequest,
        app_state: Arc<AppState>,
    ) -> Result<TeamDeployResult> {
        // Build team config from request
        let config = self.build_team_config(request).await?;

        // Deploy using runtime manager
        let team = self
            .runtime_manager
            .deploy(config, app_state)
            .await
            .context("Failed to deploy team runtime")?;

        let instance_ids = team.all_instance_ids();
        Ok(TeamDeployResult {
            id: team.id,
            name: team.name,
            status: team.status.to_string(),
            agent_count: team.agent_instances.len(),
            instance_ids,
        })
    }

    /// Get runtime team information
    ///
    /// Returns information about a running team instance.
    pub async fn get_runtime_team(&self, team_id: &str) -> Option<TeamRuntimeInfo> {
        self.runtime_manager
            .get_team(&team_id.to_string())
            .await
            .map(|team| {
                let instance_ids = team.all_instance_ids();
                TeamRuntimeInfo {
                    id: team.id,
                    name: team.name.clone(),
                    status: match team.status {
                        TeamStatus::Starting => TeamRuntimeStatus::Starting,
                        TeamStatus::Running => TeamRuntimeStatus::Running,
                        TeamStatus::Stopping => TeamRuntimeStatus::Stopping,
                        TeamStatus::Stopped => TeamRuntimeStatus::Stopped,
                        TeamStatus::Error => TeamRuntimeStatus::Error,
                    },
                    agent_count: team.agent_instances.len(),
                    instance_ids,
                    created_at: team.created_at.to_rfc3339(),
                }
            })
    }

    /// List all runtime teams
    pub async fn list_runtime_teams(&self) -> Vec<TeamRuntimeInfo> {
        let teams = self.runtime_manager.list_teams().await;
        teams
            .into_iter()
            .map(|team| {
                let instance_ids = team.all_instance_ids();
                TeamRuntimeInfo {
                    id: team.id,
                    name: team.name.clone(),
                    status: match team.status {
                        TeamStatus::Starting => TeamRuntimeStatus::Starting,
                        TeamStatus::Running => TeamRuntimeStatus::Running,
                        TeamStatus::Stopping => TeamRuntimeStatus::Stopping,
                        TeamStatus::Stopped => TeamRuntimeStatus::Stopped,
                        TeamStatus::Error => TeamRuntimeStatus::Error,
                    },
                    agent_count: team.agent_instances.len(),
                    instance_ids,
                    created_at: team.created_at.to_rfc3339(),
                }
            })
            .collect()
    }

    /// Stop and remove a runtime team
    ///
    /// Stops all instances and removes the runtime.
    /// Used by API (`DELETE /teams/{id}`).
    pub async fn stop_runtime(&self, team_id: &str) -> Result<()> {
        self.runtime_manager
            .remove_team(&team_id.to_string())
            .await
            .context("Failed to stop team runtime")
    }

    /// Scale an agent within a runtime team
    ///
    /// Adjusts the number of instances for a specific agent.
    /// Used by API (`POST /teams/{id}/scale`).
    pub async fn scale_runtime(
        &self,
        request: TeamScaleRequest,
        app_state: Arc<AppState>,
    ) -> Result<TeamScaleResult> {
        let result = self
            .runtime_manager
            .scale_agent(
                &request.team_id,
                &request.agent_name,
                request.instances,
                app_state,
            )
            .await
            .context("Failed to scale team")?;

        Ok(TeamScaleResult {
            team_id: result.team_id,
            agent_name: result.agent_name,
            previous_count: result.previous_count,
            new_count: result.new_count,
            added_instance_ids: result.added_instance_ids,
            removed_instance_ids: result.removed_instance_ids,
        })
    }

    // ============================================================================
    // Helper Methods
    // ============================================================================

    /// Build team configuration from deploy request
    async fn build_team_config(&self, request: TeamDeployRequest) -> Result<TeamConfig> {
        match request.config_source {
            TeamConfigSource::FilePath(path) => TeamConfig::from_file(&path)
                .with_context(|| format!("Failed to load team config from: {}", path.display())),
            TeamConfigSource::Inline { agents } => {
                // Build TeamConfig from inline definition
                let agent_definitions: Vec<crate::team::config::AgentDefinition> = agents
                    .into_iter()
                    .map(|a| crate::team::config::AgentDefinition {
                        name: a.name,
                        image: a.image,
                        instances: a.instances,
                        role: a.role.and_then(|r| match r.as_str() {
                            "coordinator" => Some(crate::team::config::AgentRole::Coordinator),
                            "worker" => Some(crate::team::config::AgentRole::Worker),
                            _ => None,
                        }),
                        env: None,
                    })
                    .collect();

                Ok(TeamConfig {
                    identity: crate::team::config::TeamIdentity {
                        name: request.name,
                        description: None,
                    },
                    agents: agent_definitions,
                    shared: None,
                })
            }
        }
    }

    /// Validate team name and return appropriate error
    pub fn validate_team_name(&self, name: &str) -> Result<()> {
        validate_team_name(name).map_err(|e| match e {
            ValidationError::Empty => anyhow::anyhow!("Team name cannot be empty"),
            ValidationError::TooLong(max) => {
                anyhow::anyhow!("Team name exceeds maximum length of {max} characters")
            }
            ValidationError::Reserved(reserved) => {
                anyhow::anyhow!("'{reserved}' is a reserved name and cannot be used")
            }
            ValidationError::ContainsPathSeparators => {
                anyhow::anyhow!("Team name cannot contain path separators (/ or \\)")
            }
            ValidationError::InvalidHyphenPlacement => {
                anyhow::anyhow!("Team name cannot start or end with a hyphen")
            }
            ValidationError::InvalidCharacter(ch) => {
                anyhow::anyhow!("Team name contains invalid character: '{ch}'")
            }
        })
    }

    /// Get the path resolver
    #[must_use]
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_team_runtime_status_display() {
        assert_eq!(TeamRuntimeStatus::Running.to_string(), "running");
        assert_eq!(TeamRuntimeStatus::Stopped.to_string(), "stopped");
        assert_eq!(TeamRuntimeStatus::Error.to_string(), "error");
    }
}
