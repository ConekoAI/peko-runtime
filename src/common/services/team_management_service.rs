//! Team Management Service
//!
//! Provides unified team operations for both CLI and HTTP API.
//!
//! This service wraps filesystem operations (`TeamService`) to provide a
//! single, consistent interface for all team-related configuration operations.
//!
//! ## Architecture
//!
//! - Configuration operations (create, list, delete teams) use `TeamService`
//! - Runtime operations (deploy, stop, scale) have been removed per Phase 2
//!   architecture and will be rebuilt as core primitives later.
//! - Both CLI and API use this unified service

use crate::common::identifiers::{validate_team_name, ValidationError};
use crate::common::paths::PathResolver;
use crate::common::services::TeamService;
use crate::common::types::team::{TeamCreationResult, TeamDeletionResult, TeamExportResult, TeamImportResult, TeamInfo};
use anyhow::Result;

/// Unified team management service
///
/// This is the single entry point for all team operations from both
/// CLI and HTTP API interfaces.
#[derive(Clone)]
pub struct TeamManagementService {
    /// Filesystem-based team operations
    config_service: TeamService,
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
    pub fn new(config_service: TeamService, resolver: PathResolver) -> Self {
        Self {
            config_service,
            resolver,
        }
    }

    // ============================================================================
    // Configuration Operations (used by both CLI and API)
    // ============================================================================

    /// Create a new team
    ///
    /// Creates the team directory structure and metadata file.
    /// Used by both CLI (`peko team create`) and API (`POST /teams`).
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
    /// Used by both CLI (`peko team list`) and API (`GET /teams`).
    pub async fn list_teams(&self) -> Result<Vec<TeamInfo>> {
        self.config_service.list_teams().await
    }

    /// Get team information
    ///
    /// Returns detailed information about a specific team.
    /// Used by both CLI (`peko team show`) and API (`GET /teams/{id}`).
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
    /// Used by both CLI (`peko team delete`) and API (`DELETE /teams/{id}`).
    pub async fn delete_team(&self, name: &str) -> Result<TeamDeletionResult> {
        self.config_service.delete_team(name).await
    }

    /// Export a team to an archive
    pub async fn export_team(
        &self,
        name: &str,
        output: Option<String>,
        exclude_sessions: bool,
        exclude_workspace: bool,
        exclude_mcp: bool,
    ) -> Result<TeamExportResult> {
        self.config_service.export_team(name, output, exclude_sessions, exclude_workspace, exclude_mcp).await
    }

    /// Import a team from an archive
    pub async fn import_team(
        &self,
        file: &str,
        name: Option<String>,
        force: bool,
        rotate_keys: bool,
    ) -> Result<TeamImportResult> {
        self.config_service.import_team(file, name, force, rotate_keys).await
    }

    /// Check if a team exists
    #[must_use]
    pub fn team_exists(&self, name: &str) -> bool {
        self.config_service.team_exists(name)
    }

    // ============================================================================
    // Helper Methods
    // ============================================================================

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
