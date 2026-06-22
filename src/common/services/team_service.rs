//! Team management service
//!
//! Provides filesystem-based team operations used by both CLI and API.
//! All business logic for team management lives here.
//!
//! # Membership Model (ADR-031)
//!
//! Teams no longer own agents. Instead, agents exist independently and
//! join teams via explicit membership. Membership is stored bidirectionally:
//!
//! - Agent-side: `agents/{agent}/memberships.toml`
//! - Team-side: `teams/{team}/members.toml`

use crate::common::identifiers::{validate_team_name, ValidationError};
use crate::common::paths::PathResolver;
use crate::common::types::membership::{
    AgentMembership, AgentMemberships, MembershipRole, TeamJoinResult, TeamLeaveResult, TeamMember,
    TeamMembers,
};
use crate::common::types::team::{
    TeamCreationResult, TeamDeletionResult, TeamExportResult, TeamImportResult, TeamInfo,
    TeamMetadata, TeamMoveResult,
};
use crate::identity::Identity;
use crate::registry::packaging::{self, TeamExportOptions, TeamImportOptions};
use crate::agents::agent_config::AgentConfig;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Service for managing teams on the filesystem
#[derive(Debug, Clone)]
pub struct TeamService {
    resolver: PathResolver,
}

impl TeamService {
    /// Create a new team service with the given path resolver
    #[must_use]
    pub fn new(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    /// Create a new team with the given name and optional description
    ///
    /// # Errors
    /// Returns an error if:
    /// - The team name is invalid
    /// - The team already exists
    /// - The filesystem operation fails
    pub async fn create_team(
        &self,
        name: &str,
        description: Option<&str>,
        host_runtime_id: Option<&str>,
        owner: Option<&crate::auth::principal::Principal>,
    ) -> Result<TeamCreationResult> {
        // Validate team name
        if let Err(e) = validate_team_name(name) {
            return Err(map_validation_error(name, e));
        }

        let team_dir = self.resolver.team_dir(name);

        // Check if team already exists
        if team_dir.exists() {
            anyhow::bail!("Team '{name}' already exists");
        }

        // Create team directory structure
        tokio::fs::create_dir_all(&team_dir).await?;

        // Create team metadata file
        let metadata = TeamMetadata {
            name: name.to_string(),
            description: description.map(String::from),
            created_at: chrono::Utc::now().to_rfc3339(),
            host_runtime_id: host_runtime_id.unwrap_or("").to_string(),
            owner: owner
                .cloned()
                .unwrap_or_else(|| crate::auth::principal::Principal::User(String::new())),
            owner_id: None,
            permissions: Vec::new(),
        };

        let metadata_path = team_dir.join("team.toml");
        let metadata_content = toml::to_string_pretty(&metadata)?;
        tokio::fs::write(&metadata_path, metadata_content).await?;

        // Initialize empty members file
        let members = TeamMembers::new();
        members.save(&self.resolver.team_members(name))?;

        Ok(TeamCreationResult {
            metadata,
            path: team_dir,
        })
    }

    /// List all teams with their information
    pub async fn list_teams(&self) -> Result<Vec<TeamInfo>> {
        let teams_dir = self.resolver.teams_dir();

        if !teams_dir.exists() {
            return Ok(Vec::new());
        }

        let mut teams: Vec<TeamInfo> = Vec::new();
        let mut entries = tokio::fs::read_dir(&teams_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let team_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            // Skip invalid team names (could be temp files, etc.)
            if validate_team_name(&team_name).is_err() {
                continue;
            }

            let metadata = load_team_metadata(&path, &team_name).await;
            let members = self.load_team_member_names(&team_name).await;
            let agent_count = members.len();

            teams.push(TeamInfo {
                name: team_name,
                metadata,
                agent_count,
                members,
                path,
            });
        }

        // Sort teams: default first, then alphabetically
        teams.sort_by(|a, b| {
            if a.name == "default" {
                return std::cmp::Ordering::Less;
            }
            if b.name == "default" {
                return std::cmp::Ordering::Greater;
            }
            a.name.cmp(&b.name)
        });

        Ok(teams)
    }

    /// Get information about a specific team
    pub async fn get_team(&self, name: &str) -> Result<Option<TeamInfo>> {
        // Validate team name
        if let Err(e) = validate_team_name(name) {
            return Err(map_validation_error(name, e));
        }

        let team_dir = self.resolver.team_dir(name);

        if !team_dir.exists() {
            return Ok(None);
        }

        let metadata = load_team_metadata(&team_dir, name).await;
        let members = self.load_team_member_names(name).await;
        let agent_count = members.len();

        Ok(Some(TeamInfo {
            name: name.to_string(),
            metadata,
            agent_count,
            members,
            path: team_dir,
        }))
    }

    /// Get agents in a team with their configs.
    ///
    /// Returns membership-based agents from the new layout.
    pub async fn get_team_agents(&self, name: &str) -> Result<Vec<(String, AgentConfig)>> {
        let team_dir = self.resolver.team_dir(name);

        if !team_dir.exists() {
            anyhow::bail!("Team '{name}' not found");
        }

        let mut agents = Vec::new();

        // Get members from the membership model
        let members_path = self.resolver.team_members(name);
        if members_path.exists() {
            let members = TeamMembers::load(&members_path)?;
            for member in &members.members {
                let agent_name = &member.agent;
                let config_path = self.resolver.agent_config(agent_name);
                if config_path.exists() {
                    if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
                        if let Ok(config) = toml::from_str::<AgentConfig>(&content) {
                            agents.push((agent_name.clone(), config));
                        }
                    }
                }
            }
        }

        // Sort alphabetically
        agents.sort_by(|a, b| a.0.cmp(&b.0));

        Ok(agents)
    }

    /// Delete a team.
    ///
    /// In the new model, deleting a team removes the team directory and
    /// membership references, but does NOT delete the member agents.
    pub async fn delete_team(&self, name: &str) -> Result<TeamDeletionResult> {
        // Validate team name
        if let Err(e) = validate_team_name(name) {
            return Err(map_validation_error(name, e));
        }

        // Prevent deletion of default team
        if name == "default" {
            anyhow::bail!("Cannot delete the 'default' team");
        }

        let team_dir = self.resolver.team_dir(name);

        if !team_dir.exists() {
            anyhow::bail!("Team '{name}' not found");
        }

        let member_count = self.count_team_members(name).await;

        // Remove memberships from all member agents
        let members_path = self.resolver.team_members(name);
        if members_path.exists() {
            let members = TeamMembers::load(&members_path)?;
            for member in &members.members {
                let agent_memberships_path = self.resolver.agent_memberships(&member.agent);
                if agent_memberships_path.exists() {
                    if let Ok(mut agent_memberships) =
                        AgentMemberships::load(&agent_memberships_path)
                    {
                        agent_memberships.remove(name);
                        let _ = agent_memberships.save(&agent_memberships_path);
                    }
                }
            }
        }

        // Delete team directory (this removes members.toml, team.toml, etc.)
        tokio::fs::remove_dir_all(&team_dir).await?;

        Ok(TeamDeletionResult {
            name: name.to_string(),
            agents_deleted: member_count,
        })
    }

    /// Move/rename a team
    pub async fn move_team(&self, old_name: &str, new_name: &str) -> Result<TeamMoveResult> {
        // Validate team names
        if let Err(e) = validate_team_name(old_name) {
            return Err(map_validation_error(old_name, e));
        }
        if let Err(e) = validate_team_name(new_name) {
            return Err(map_validation_error(new_name, e));
        }

        // Prevent renaming the default team
        if old_name == "default" {
            anyhow::bail!("Cannot rename the 'default' team");
        }

        let old_team_dir = self.resolver.team_dir(old_name);
        let new_team_dir = self.resolver.team_dir(new_name);

        // Check source exists
        if !old_team_dir.exists() {
            anyhow::bail!("Team '{old_name}' not found");
        }

        // Check target doesn't exist (default team always exists conceptually)
        if new_name == "default" || new_team_dir.exists() {
            anyhow::bail!("Team '{new_name}' already exists");
        }

        // Count agents before move
        let agents_moved = self.count_team_members(old_name).await;

        // Update agent memberships to point to the new team name
        let members_path = self.resolver.team_members(old_name);
        if members_path.exists() {
            let members = TeamMembers::load(&members_path)?;
            for member in &members.members {
                let agent_memberships_path = self.resolver.agent_memberships(&member.agent);
                if agent_memberships_path.exists() {
                    if let Ok(mut agent_memberships) =
                        AgentMemberships::load(&agent_memberships_path)
                    {
                        if let Some(m) = agent_memberships.get(old_name) {
                            let updated = AgentMembership {
                                team: new_name.to_string(),
                                joined_at: m.joined_at.clone(),
                                role: m.role,
                            };
                            agent_memberships.remove(old_name);
                            agent_memberships.add(updated);
                            let _ = agent_memberships.save(&agent_memberships_path);
                        }
                    }
                }
            }
        }

        // Update metadata file if it exists
        let metadata_path = old_team_dir.join("team.toml");
        if metadata_path.exists() {
            let content = tokio::fs::read_to_string(&metadata_path).await?;
            if let Ok(mut metadata) = toml::from_str::<TeamMetadata>(&content) {
                metadata.name = new_name.to_string();
                let updated_content = toml::to_string_pretty(&metadata)?;
                tokio::fs::write(&metadata_path, updated_content).await?;
            }
        }

        // Rename the directory
        tokio::fs::rename(&old_team_dir, &new_team_dir).await?;

        Ok(TeamMoveResult {
            old_name: old_name.to_string(),
            new_name: new_name.to_string(),
            old_path: old_team_dir,
            new_path: new_team_dir,
            agents_moved,
        })
    }

    // ========================================================================
    // Membership Operations (NEW - ADR-031)
    // ========================================================================

    /// Add an agent to a team (join).
    ///
    /// Updates both the team's members.toml and the agent's memberships.toml.
    pub async fn join_team(
        &self,
        team: &str,
        agent: &str,
        role: MembershipRole,
    ) -> Result<TeamJoinResult> {
        // Validate team exists
        if !self.team_exists(team) {
            anyhow::bail!("Team '{team}' not found");
        }

        // Validate agent exists
        if !self.resolver.agent_exists(agent) {
            anyhow::bail!("Agent '{agent}' not found");
        }

        let joined_at = chrono::Utc::now().to_rfc3339();

        // Update team-side members.toml
        let members_path = self.resolver.team_members(team);
        let mut members = TeamMembers::load(&members_path)?;
        members.add(TeamMember {
            agent: agent.to_string(),
            joined_at: joined_at.clone(),
            role,
        });
        members.save(&members_path)?;

        // Update agent-side memberships.toml
        let memberships_path = self.resolver.agent_memberships(agent);
        let mut memberships = AgentMemberships::load(&memberships_path)?;
        memberships.add(AgentMembership {
            team: team.to_string(),
            joined_at: joined_at.clone(),
            role,
        });
        memberships.save(&memberships_path)?;

        Ok(TeamJoinResult {
            agent: agent.to_string(),
            team: team.to_string(),
            role,
        })
    }

    /// Remove an agent from a team (leave).
    ///
    /// Updates both the team's members.toml and the agent's memberships.toml.
    pub async fn leave_team(&self, team: &str, agent: &str) -> Result<TeamLeaveResult> {
        // Validate team exists
        if !self.team_exists(team) {
            anyhow::bail!("Team '{team}' not found");
        }

        let members_path = self.resolver.team_members(team);
        let mut members = TeamMembers::load(&members_path)?;
        let was_member = members.has_member(agent);
        members.remove(agent);
        members.save(&members_path)?;

        // Update agent-side memberships.toml
        let memberships_path = self.resolver.agent_memberships(agent);
        if memberships_path.exists() {
            if let Ok(mut memberships) = AgentMemberships::load(&memberships_path) {
                memberships.remove(team);
                let _ = memberships.save(&memberships_path);
            }
        }

        Ok(TeamLeaveResult {
            agent: agent.to_string(),
            team: team.to_string(),
            was_member,
        })
    }

    /// Get the members of a team
    pub async fn get_members(&self, team: &str) -> Result<TeamMembers> {
        if !self.team_exists(team) {
            anyhow::bail!("Team '{team}' not found");
        }

        let members_path = self.resolver.team_members(team);
        Ok(TeamMembers::load(&members_path)?)
    }

    /// Get the teams an agent belongs to
    pub async fn get_agent_memberships(&self, agent: &str) -> Result<AgentMemberships> {
        let memberships_path = self.resolver.agent_memberships(agent);
        Ok(AgentMemberships::load(&memberships_path)?)
    }

    /// Check if an agent is a member of a team
    pub async fn is_member(&self, team: &str, agent: &str) -> Result<bool> {
        let members = self.get_members(team).await?;
        Ok(members.has_member(agent))
    }

    /// Load member agent names for a team
    async fn load_team_member_names(&self, team_name: &str) -> Vec<String> {
        let members_path = self.resolver.team_members(team_name);
        if members_path.exists() {
            if let Ok(members) = TeamMembers::load(&members_path) {
                return members.members.iter().map(|m| m.agent.clone()).collect();
            }
        }
        Vec::new()
    }

    /// Count the number of members in a team
    async fn count_team_members(&self, team_name: &str) -> usize {
        self.load_team_member_names(team_name).await.len()
    }

    /// Check if a team exists
    #[must_use]
    pub fn team_exists(&self, name: &str) -> bool {
        self.resolver.team_dir(name).exists()
    }

    /// Get the path resolver
    #[must_use]
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    // ============================================================================
    // Ownership and Permission (ADR-033)
    // ============================================================================

    /// Transfer ownership of a team.
    pub async fn transfer_team_owner(
        &self,
        name: &str,
        new_owner_id: &str,
        caller: &crate::auth::principal::Principal,
    ) -> Result<()> {
        let team_dir = self.resolver.team_dir(name);
        if !team_dir.exists() {
            anyhow::bail!("Team '{name}' not found");
        }

        let meta_path = team_dir.join("team.toml");
        let content = tokio::fs::read_to_string(&meta_path).await?;
        let mut meta: crate::common::types::team::TeamMetadata = toml::from_str(&content)?;

        if &meta.owner != caller {
            anyhow::bail!("Permission denied: only the owner can transfer ownership");
        }

        meta.owner = crate::auth::principal::principal_from_string_with_default_user(new_owner_id);
        let updated = toml::to_string_pretty(&meta)?;
        tokio::fs::write(&meta_path, updated).await?;
        Ok(())
    }

    /// Grant a permission on a team.
    pub async fn grant_team_permission(
        &self,
        name: &str,
        grant: crate::auth::ownership::PermissionGrant,
        caller: &crate::auth::principal::Principal,
    ) -> Result<()> {
        let team_dir = self.resolver.team_dir(name);
        if !team_dir.exists() {
            anyhow::bail!("Team '{name}' not found");
        }

        let meta_path = team_dir.join("team.toml");
        let content = tokio::fs::read_to_string(&meta_path).await?;
        let mut meta: crate::common::types::team::TeamMetadata = toml::from_str(&content)?;

        if &meta.owner != caller {
            anyhow::bail!("Permission denied: only the owner can grant permissions");
        }

        let grant_disc = std::mem::discriminant(&grant.permission);
        meta.permissions.retain(|g| {
            !(g.subject == grant.subject
                && std::mem::discriminant(&g.permission) == grant_disc)
        });
        meta.permissions.push(grant);

        let updated = toml::to_string_pretty(&meta)?;
        tokio::fs::write(&meta_path, updated).await?;
        Ok(())
    }

    /// Revoke a permission from a team.
    pub async fn revoke_team_permission(
        &self,
        name: &str,
        subject: &crate::auth::principal::Principal,
        permission: &crate::auth::ownership::Permission,
        caller: &crate::auth::principal::Principal,
    ) -> Result<()> {
        let team_dir = self.resolver.team_dir(name);
        if !team_dir.exists() {
            anyhow::bail!("Team '{name}' not found");
        }

        let meta_path = team_dir.join("team.toml");
        let content = tokio::fs::read_to_string(&meta_path).await?;
        let mut meta: crate::common::types::team::TeamMetadata = toml::from_str(&content)?;

        if &meta.owner != caller {
            anyhow::bail!("Permission denied: only the owner can revoke permissions");
        }

        let perm_disc = std::mem::discriminant(permission);
        meta.permissions.retain(|g| {
            !(g.subject == *subject && std::mem::discriminant(&g.permission) == perm_disc)
        });

        let updated = toml::to_string_pretty(&meta)?;
        tokio::fs::write(&meta_path, updated).await?;
        Ok(())
    }

    /// Export a team to a .team package
    pub async fn export_team(
        &self,
        name: &str,
        output: Option<String>,
        skip_sessions: bool,
        skip_workspace: bool,
        skip_mcp: bool,
    ) -> Result<TeamExportResult> {
        // Validate team exists
        let team_info = self.get_team(name).await?;
        if team_info.is_none() {
            anyhow::bail!("Team '{name}' not found");
        }

        // Get all agents in the team
        let agents = self.get_team_agents(name).await?;
        if agents.is_empty() {
            anyhow::bail!("Team '{name}' has no agents to export");
        }

        // Prepare agents for export
        let mut agent_exports: Vec<(String, AgentConfig, Identity)> = Vec::new();
        for (agent_name, config) in &agents {
            // Generate a new identity for export
            let identity = Identity::new(agent_name, crate::identity::did::DIDScope::Local)
                .await
                .with_context(|| format!("Failed to create identity for agent: {agent_name}"))?;

            agent_exports.push((agent_name.clone(), config.clone(), identity));
        }

        // Get team metadata for description
        let team_dir = self.resolver.team_dir(name);
        let description = load_team_metadata(&team_dir, name).await.description;

        // Export options
        let export_opts = TeamExportOptions {
            output_path: output,
            include_sessions: !skip_sessions,
            include_workspace: !skip_workspace,
            include_mcp: !skip_mcp,
            description: description.or_else(|| Some(format!("Exported team: {name}"))),
        };

        // Get base directory for workspace/sessions paths
        let base_dir = self.resolver.data_dir();

        // Export the team
        let config_dir = self.resolver.config_dir().to_path_buf();
        let output_path = packaging::export_team_with_config_dir(
            name,
            None,
            &base_dir,
            &config_dir,
            agent_exports,
            export_opts,
        )
        .await
        .with_context(|| format!("Failed to export team '{name}'"))?;

        Ok(TeamExportResult {
            name: name.to_string(),
            output_path,
            agent_count: agents.len(),
        })
    }

    /// Import a team from a .team package
    pub async fn import_team(
        &self,
        file_path: &str,
        new_name: Option<String>,
        force: bool,
        rotate_keys: bool,
        host_runtime_id: Option<&str>,
    ) -> Result<TeamImportResult> {
        let path = std::path::PathBuf::from(file_path);

        if !path.exists() {
            anyhow::bail!("File not found: {file_path}");
        }

        // Create the team if it doesn't exist
        let team_name = new_name.as_deref().unwrap_or("imported");

        if !self.team_exists(team_name) {
            self.create_team(
                team_name,
                Some(&format!("Imported team from {file_path}")),
                None,
                None,
            )
            .await?;
        } else if !force {
            anyhow::bail!("Team '{team_name}' already exists. Use --force to overwrite.");
        }

        let import_opts = TeamImportOptions {
            new_name: new_name.clone(),
            import_sessions: true,
            import_workspace: true,
            import_mcp: true,
            rotate_keys,
            force: true,
            // `peko team import` does not surface the unsigned opt-in
            // to the CLI; default to false (secure by default).
            allow_unsigned: false,
        };

        let config_dir = self.resolver.config_dir();
        let result_team_dir = self.resolver.team_dir(team_name);

        let result = packaging::import_team_with_base_dir(&path, &config_dir, import_opts)
            .await
            .with_context(|| format!("Failed to import team from '{file_path}'"))?;

        // If the package restored a team.toml, preserve its name and update host_runtime_id
        let team_toml_path = result_team_dir.join("team.toml");
        if team_toml_path.exists() {
            if let Ok(content) = tokio::fs::read_to_string(&team_toml_path).await {
                if let Ok(mut metadata) = toml::from_str::<TeamMetadata>(&content) {
                    metadata.name = team_name.to_string();
                    if let Some(host_id) = host_runtime_id {
                        metadata.host_runtime_id = host_id.to_string();
                    }
                    if let Ok(updated) = toml::to_string_pretty(&metadata) {
                        let _ = tokio::fs::write(&team_toml_path, updated).await;
                    }
                }
            }
        }

        Ok(TeamImportResult {
            name: result.name,
            path: result_team_dir,
            agents_imported: result.agent_count,
        })
    }
}

/// Load team metadata from team.toml
async fn load_team_metadata(team_dir: &PathBuf, team_name: &str) -> TeamMetadata {
    let metadata_path = team_dir.join("team.toml");

    // Try to read existing team.toml
    if let Ok(content) = tokio::fs::read_to_string(&metadata_path).await {
        if let Ok(metadata) = toml::from_str::<TeamMetadata>(&content) {
            return metadata;
        }
    }

    // Fallback: generate metadata from directory creation time or current time
    let created_at = tokio::fs::metadata(team_dir)
        .await
        .ok()
        .and_then(|m| m.created().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            let dt = chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                .unwrap_or_else(|| chrono::Utc::now());
            dt.to_rfc3339()
        })
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    TeamMetadata {
        name: team_name.to_string(),
        description: None,
        created_at,
        host_runtime_id: "".to_string(),
        owner: crate::auth::principal::Principal::User(String::new()),
        owner_id: None,
        permissions: Vec::new(),
    }
}

/// Map validation error to anyhow error with descriptive message
fn map_validation_error(_name: &str, e: ValidationError) -> anyhow::Error {
    match e {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_team_service_creation() {
        let resolver = PathResolver::new();
        let _service = TeamService::new(resolver);
    }

    // ========================================================================
    // Membership Tests
    // ========================================================================

    #[tokio::test]
    async fn test_join_team_adds_bidirectional_membership() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir, cache_dir);
        let service = TeamService::new(resolver.clone());

        // Create team
        service
            .create_team("engineering", None, None, None)
            .await
            .unwrap();

        // Create agent in new layout
        let agent_dir = config_dir.join("agents").join("alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("config.toml"), "name = 'alice'\n").unwrap();

        // Join team
        let result = service
            .join_team("engineering", "alice", MembershipRole::Member)
            .await
            .unwrap();

        assert_eq!(result.agent, "alice");
        assert_eq!(result.team, "engineering");
        assert_eq!(result.role, MembershipRole::Member);

        // Verify team-side members.toml
        let members = service.get_members("engineering").await.unwrap();
        assert!(members.has_member("alice"));
        assert_eq!(members.len(), 1);

        // Verify agent-side memberships.toml
        let memberships = service.get_agent_memberships("alice").await.unwrap();
        assert!(memberships.belongs_to("engineering"));
        assert_eq!(memberships.len(), 1);
    }

    #[tokio::test]
    async fn test_leave_team_removes_bidirectional_membership() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir, cache_dir);
        let service = TeamService::new(resolver.clone());

        // Create team and agent
        service
            .create_team("engineering", None, None, None)
            .await
            .unwrap();
        let agent_dir = config_dir.join("agents").join("alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("config.toml"), "name = 'alice'\n").unwrap();

        // Join then leave
        service
            .join_team("engineering", "alice", MembershipRole::Member)
            .await
            .unwrap();
        let result = service.leave_team("engineering", "alice").await.unwrap();

        assert!(result.was_member);

        // Verify removed from both sides
        let members = service.get_members("engineering").await.unwrap();
        assert!(!members.has_member("alice"));

        let memberships = service.get_agent_memberships("alice").await.unwrap();
        assert!(!memberships.belongs_to("engineering"));
    }

    #[tokio::test]
    async fn test_delete_team_removes_memberships_but_not_agents() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir, cache_dir);
        let service = TeamService::new(resolver.clone());

        // Create team and agent
        service
            .create_team("engineering", None, None, None)
            .await
            .unwrap();
        let agent_dir = config_dir.join("agents").join("alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("config.toml"), "name = 'alice'\n").unwrap();

        service
            .join_team("engineering", "alice", MembershipRole::Member)
            .await
            .unwrap();

        // Delete team
        let result = service.delete_team("engineering").await.unwrap();
        assert_eq!(result.agents_deleted, 1);

        // Agent should still exist
        assert!(agent_dir.exists());

        // Agent should no longer have engineering membership
        let memberships = service.get_agent_memberships("alice").await.unwrap();
        assert!(!memberships.belongs_to("engineering"));
    }

    #[tokio::test]
    async fn test_move_team_updates_memberships() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir, cache_dir);
        let service = TeamService::new(resolver.clone());

        // Create teams and agent
        service
            .create_team("engineering", None, None, None)
            .await
            .unwrap();
        let agent_dir = config_dir.join("agents").join("alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("config.toml"), "name = 'alice'\n").unwrap();

        service
            .join_team("engineering", "alice", MembershipRole::Admin)
            .await
            .unwrap();

        // Move team
        let result = service.move_team("engineering", "dev").await.unwrap();
        assert_eq!(result.agents_moved, 1);

        // Agent's membership should point to new team name
        let memberships = service.get_agent_memberships("alice").await.unwrap();
        assert!(!memberships.belongs_to("engineering"));
        assert!(memberships.belongs_to("dev"));

        // Role should be preserved
        let m = memberships.get("dev").unwrap();
        assert_eq!(m.role, MembershipRole::Admin);
    }

    #[tokio::test]
    async fn test_multi_team_membership() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir, cache_dir);
        let service = TeamService::new(resolver.clone());

        // Create teams and agent
        service
            .create_team("engineering", None, None, None)
            .await
            .unwrap();
        service.create_team("ops", None, None, None).await.unwrap();

        let agent_dir = config_dir.join("agents").join("alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("config.toml"), "name = 'alice'\n").unwrap();

        // Join multiple teams
        service
            .join_team("engineering", "alice", MembershipRole::Member)
            .await
            .unwrap();
        service
            .join_team("ops", "alice", MembershipRole::Admin)
            .await
            .unwrap();

        // Verify agent belongs to both
        let memberships = service.get_agent_memberships("alice").await.unwrap();
        assert!(memberships.belongs_to("engineering"));
        assert!(memberships.belongs_to("ops"));
        assert_eq!(memberships.len(), 2);

        // Verify both teams list alice as member
        let eng_members = service.get_members("engineering").await.unwrap();
        assert!(eng_members.has_member("alice"));

        let ops_members = service.get_members("ops").await.unwrap();
        assert!(ops_members.has_member("alice"));
    }

    #[tokio::test]
    async fn test_join_nonexistent_team_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );
        let service = TeamService::new(resolver);

        let result = service
            .join_team("nonexistent", "alice", MembershipRole::Member)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_join_nonexistent_agent_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );
        let service = TeamService::new(resolver);

        service
            .create_team("engineering", None, None, None)
            .await
            .unwrap();

        let result = service
            .join_team("engineering", "nonexistent", MembershipRole::Member)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_list_teams_counts_members() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir, cache_dir);
        let service = TeamService::new(resolver.clone());

        service
            .create_team("engineering", None, None, None)
            .await
            .unwrap();

        let agent_dir = config_dir.join("agents").join("alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("config.toml"), "name = 'alice'\n").unwrap();

        service
            .join_team("engineering", "alice", MembershipRole::Member)
            .await
            .unwrap();

        let teams = service.list_teams().await.unwrap();
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].agent_count, 1);
    }
}
