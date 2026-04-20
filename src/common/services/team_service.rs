//! Team management service
//!
//! Provides filesystem-based team operations used by both CLI and API.
//! All business logic for team management lives here.

use crate::common::identifiers::{validate_team_name, ValidationError};
use crate::common::paths::PathResolver;
use crate::common::types::team::{
    TeamCreationResult, TeamDeletionResult, TeamExportResult, TeamImportResult, TeamInfo,
    TeamMetadata, TeamMoveResult,
};
use crate::identity::Identity;
use crate::portable::{self, TeamExportOptions, TeamImportOptions};
use crate::types::agent::AgentConfig;
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
        let agents_dir = team_dir.join("agents");
        tokio::fs::create_dir_all(&agents_dir).await?;

        // Create team metadata file
        let metadata = TeamMetadata {
            name: name.to_string(),
            description: description.map(String::from),
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let metadata_path = team_dir.join("team.toml");
        let metadata_content = toml::to_string_pretty(&metadata)?;
        tokio::fs::write(&metadata_path, metadata_content).await?;

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

            let metadata = load_team_metadata(&path).await.ok();
            let agent_count = count_agents_in_team(&path).await;

            teams.push(TeamInfo {
                name: team_name,
                metadata,
                agent_count,
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

        let metadata = load_team_metadata(&team_dir).await.ok();
        let agent_count = count_agents_in_team(&team_dir).await;

        Ok(Some(TeamInfo {
            name: name.to_string(),
            metadata,
            agent_count,
            path: team_dir,
        }))
    }

    /// Get agents in a team with their configs
    pub async fn get_team_agents(&self, name: &str) -> Result<Vec<(String, AgentConfig)>> {
        let team_dir = self.resolver.team_dir(name);

        if !team_dir.exists() {
            anyhow::bail!("Team '{name}' not found");
        }

        list_agents_in_team(&team_dir).await
    }

    /// Delete a team and all its agents
    ///
    /// # Errors
    /// Returns an error if:
    /// - The team name is invalid
    /// - The team is the default team (cannot be deleted)
    /// - The team doesn't exist
    /// - The filesystem operation fails
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

        let agent_count = count_agents_in_team(&team_dir).await;

        // Delete team directory
        tokio::fs::remove_dir_all(&team_dir).await?;

        Ok(TeamDeletionResult {
            name: name.to_string(),
            agents_deleted: agent_count,
        })
    }

    /// Move/rename a team
    ///
    /// # Errors
    /// Returns an error if:
    /// - Either team name is invalid
    /// - The source team doesn't exist
    /// - The target team already exists
    /// - The team is the default team (cannot be renamed)
    /// - The filesystem operation fails
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
        let agents_moved = count_agents_in_team(&old_team_dir).await;

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
        let description = load_team_metadata(&team_dir)
            .await
            .ok()
            .and_then(|m| m.description);

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
        let output_path = portable::export_team(name, None, &base_dir, agent_exports, export_opts)
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
    ) -> Result<TeamImportResult> {
        let path = std::path::PathBuf::from(file_path);

        if !path.exists() {
            anyhow::bail!("File not found: {file_path}");
        }

        // Create the team if it doesn't exist
        let team_name = new_name.as_deref().unwrap_or("imported");

        if !self.team_exists(team_name) {
            self.create_team(team_name, Some(&format!("Imported team from {file_path}")))
                .await?;
        } else if !force {
            anyhow::bail!("Team '{team_name}' already exists. Use --force to overwrite.");
        }

        // Import options
        // Note: force is always true here because TeamService already handled the existence check
        let import_opts = TeamImportOptions {
            new_name: new_name.clone(),
            import_sessions: true,
            import_workspace: true,
            import_mcp: true,
            rotate_keys,
            force: true,
        };

        // Get config directory for base path (must match PathResolver's config_dir)
        let config_dir = self.resolver.config_dir();
        let result_team_dir = self.resolver.team_dir(team_name);

        // Import the team with correct base directory
        let result = portable::import_team_with_base_dir(&path, &config_dir, import_opts)
            .await
            .with_context(|| format!("Failed to import team from '{file_path}'"))?;

        Ok(TeamImportResult {
            name: result.name,
            path: result_team_dir,
            agents_imported: result.agent_count,
        })
    }
}

/// Load team metadata from team.toml
async fn load_team_metadata(team_dir: &PathBuf) -> Result<TeamMetadata> {
    let metadata_path = team_dir.join("team.toml");
    let content = tokio::fs::read_to_string(&metadata_path)
        .await
        .with_context(|| format!("Failed to read team metadata from {metadata_path:?}"))?;
    let metadata: TeamMetadata = toml::from_str(&content)
        .with_context(|| format!("Failed to parse team metadata from {metadata_path:?}"))?;
    Ok(metadata)
}

/// Count agents in a team
/// Only counts directories with a valid, parseable config.toml
async fn count_agents_in_team(team_dir: &PathBuf) -> usize {
    use crate::types::agent::AgentConfig;

    let agents_dir = team_dir.join("agents");

    if !agents_dir.exists() {
        return 0;
    }

    match tokio::fs::read_dir(&agents_dir).await {
        Ok(mut entries) => {
            let mut count = 0;
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }

                // Only count if config.toml exists and is valid
                let config_path = path.join("config.toml");
                if config_path.exists() {
                    if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
                        if toml::from_str::<AgentConfig>(&content).is_ok() {
                            count += 1;
                        }
                    }
                }
            }
            count
        }
        Err(_) => 0,
    }
}

/// List agents in a team with their configs
async fn list_agents_in_team(team_dir: &PathBuf) -> Result<Vec<(String, AgentConfig)>> {
    let agents_dir = team_dir.join("agents");
    let mut agents = Vec::new();

    if !agents_dir.exists() {
        return Ok(agents);
    }

    let mut entries = tokio::fs::read_dir(&agents_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let agent_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let config_path = path.join("config.toml");
        if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
            if let Ok(config) = toml::from_str::<AgentConfig>(&content) {
                agents.push((agent_name, config));
            }
        }
    }

    // Sort alphabetically
    agents.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(agents)
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
}
