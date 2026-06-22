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
use crate::common::types::membership::{MembershipRole, TeamJoinResult, TeamLeaveResult};
use crate::common::types::team::{
    TeamCreationResult, TeamDeletionResult, TeamExportResult, TeamImportResult, TeamInfo,
    TeamMoveResult,
};
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
        host_runtime_id: Option<&str>,
        owner: Option<&crate::auth::principal::Principal>,
    ) -> Result<TeamCreationResult> {
        self.config_service
            .create_team(name, description, host_runtime_id, owner)
            .await
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
    ) -> Result<Vec<(String, crate::agents::agent_config::AgentConfig)>> {
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
        self.config_service
            .export_team(
                name,
                output,
                exclude_sessions,
                exclude_workspace,
                exclude_mcp,
            )
            .await
    }

    /// Import a team from an archive
    pub async fn import_team(
        &self,
        file: &str,
        name: Option<String>,
        force: bool,
        rotate_keys: bool,
        host_runtime_id: Option<&str>,
    ) -> Result<TeamImportResult> {
        self.config_service
            .import_team(file, name, force, rotate_keys, host_runtime_id)
            .await
    }

    /// Move/rename a team
    pub async fn move_team(&self, old_name: &str, new_name: &str) -> Result<TeamMoveResult> {
        self.config_service.move_team(old_name, new_name).await
    }

    /// Add an agent to a team
    pub async fn join_team(
        &self,
        team: &str,
        agent: &str,
        role: MembershipRole,
    ) -> Result<TeamJoinResult> {
        self.config_service.join_team(team, agent, role).await
    }

    /// Remove an agent from a team
    pub async fn leave_team(&self, team: &str, agent: &str) -> Result<TeamLeaveResult> {
        self.config_service.leave_team(team, agent).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::membership::MembershipRole;

    fn test_service() -> (tempfile::TempDir, TeamManagementService) {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let team_service = TeamService::new(resolver.clone());
        let service = TeamManagementService::new(team_service, resolver);
        (temp_dir, service)
    }

    fn create_test_agent(service: &TeamManagementService, name: &str) {
        let agent_dir = service.resolver().agent_dir(name);
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("config.toml"), format!("name = '{name}'\n")).unwrap();
    }

    #[tokio::test]
    async fn test_create_and_get_team() {
        use crate::auth::principal::Principal;
        let (_temp, service) = test_service();

        let owner = Principal::User("owner-1".into());
        let result = service
            .create_team("engineering", Some("Eng team"), None, Some(&owner))
            .await
            .unwrap();
        assert_eq!(result.metadata.name, "engineering");
        assert_eq!(result.metadata.description.as_deref(), Some("Eng team"));
        assert_eq!(result.metadata.owner, Principal::User("owner-1".into()));

        let team = service.get_team("engineering").await.unwrap();
        assert!(team.is_some());
        let team = team.unwrap();
        assert_eq!(team.name, "engineering");
        assert_eq!(team.agent_count, 0);
    }

    #[tokio::test]
    async fn test_list_teams_sorted() {
        let (_temp, service) = test_service();

        service.create_team("alpha", None, None, None).await.unwrap();
        service.create_team("default", None, None, None).await.unwrap();
        service.create_team("beta", None, None, None).await.unwrap();

        let teams = service.list_teams().await.unwrap();
        assert_eq!(teams.len(), 3);
        assert_eq!(teams[0].name, "default");
        assert_eq!(teams[1].name, "alpha");
        assert_eq!(teams[2].name, "beta");
    }

    #[tokio::test]
    async fn test_join_and_leave_team() {
        let (_temp, service) = test_service();

        service.create_team("engineering", None, None, None).await.unwrap();
        create_test_agent(&service, "alice");

        let join = service
            .join_team("engineering", "alice", MembershipRole::Admin)
            .await
            .unwrap();
        assert_eq!(join.agent, "alice");
        assert_eq!(join.role, MembershipRole::Admin);

        let team = service.get_team("engineering").await.unwrap().unwrap();
        assert_eq!(team.agent_count, 1);
        assert!(team.members.contains(&"alice".to_string()));

        let leave = service.leave_team("engineering", "alice").await.unwrap();
        assert!(leave.was_member);

        let team = service.get_team("engineering").await.unwrap().unwrap();
        assert_eq!(team.agent_count, 0);
    }

    #[tokio::test]
    async fn test_delete_team_removes_members_but_keeps_agents() {
        let (_temp, service) = test_service();

        service.create_team("engineering", None, None, None).await.unwrap();
        create_test_agent(&service, "alice");
        service
            .join_team("engineering", "alice", MembershipRole::Member)
            .await
            .unwrap();

        let result = service.delete_team("engineering").await.unwrap();
        assert_eq!(result.agents_deleted, 1);

        assert!(!service.team_exists("engineering"));
        assert!(service.resolver().agent_dir("alice").exists());
    }

    #[tokio::test]
    async fn test_move_team() {
        let (_temp, service) = test_service();

        service.create_team("engineering", None, None, None).await.unwrap();
        create_test_agent(&service, "alice");
        service
            .join_team("engineering", "alice", MembershipRole::Member)
            .await
            .unwrap();

        let result = service.move_team("engineering", "dev").await.unwrap();
        assert_eq!(result.old_name, "engineering");
        assert_eq!(result.new_name, "dev");
        assert_eq!(result.agents_moved, 1);

        assert!(!service.team_exists("engineering"));
        assert!(service.team_exists("dev"));

        let team = service.get_team("dev").await.unwrap().unwrap();
        assert!(team.members.contains(&"alice".to_string()));
    }

    #[tokio::test]
    async fn test_validate_team_name() {
        let (_temp, service) = test_service();

        assert!(service.validate_team_name("valid-team").is_ok());
        assert!(service.validate_team_name("").is_err());
        assert!(service.validate_team_name("-bad").is_err());
        assert!(service.validate_team_name("bad/team").is_err());
    }

    #[tokio::test]
    async fn test_cannot_delete_default_team() {
        let (_temp, service) = test_service();

        service.create_team("default", None, None, None).await.unwrap();
        let result = service.delete_team("default").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot delete"));
    }
}
