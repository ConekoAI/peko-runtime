//! Team management service
//!
//! Provides filesystem-based team operations used by both CLI and API.
//! All business logic for team management lives here.

use crate::common::identifiers::{validate_team_name, ValidationError};
use crate::common::paths::PathResolver;
use crate::common::types::team::{TeamCreationResult, TeamDeletionResult, TeamInfo, TeamMetadata};
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
            anyhow::bail!("Team '{}' already exists", name);
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
    pub async fn get_team_agents(
        &self,
        name: &str,
    ) -> Result<Vec<(String, AgentConfig)>> {
        let team_dir = self.resolver.team_dir(name);

        if !team_dir.exists() {
            anyhow::bail!("Team '{}' not found", name);
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
            anyhow::bail!("Team '{}' not found", name);
        }

        let agent_count = count_agents_in_team(&team_dir).await;

        // Delete team directory
        tokio::fs::remove_dir_all(&team_dir).await?;

        Ok(TeamDeletionResult {
            name: name.to_string(),
            agents_deleted: agent_count,
        })
    }

    /// Check if a team exists
    pub fn team_exists(&self, name: &str) -> bool {
        self.resolver.team_dir(name).exists()
    }

    /// Get the path resolver
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }
}

/// Load team metadata from team.toml
async fn load_team_metadata(team_dir: &PathBuf) -> Result<TeamMetadata> {
    let metadata_path = team_dir.join("team.toml");
    let content = tokio::fs::read_to_string(&metadata_path)
        .await
        .with_context(|| format!("Failed to read team metadata from {:?}", metadata_path))?;
    let metadata: TeamMetadata = toml::from_str(&content)
        .with_context(|| format!("Failed to parse team metadata from {:?}", metadata_path))?;
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
fn map_validation_error(name: &str, e: ValidationError) -> anyhow::Error {
    match e {
        ValidationError::Empty => anyhow::anyhow!("Team name cannot be empty"),
        ValidationError::TooLong(max) => {
            anyhow::anyhow!("Team name exceeds maximum length of {} characters", max)
        }
        ValidationError::Reserved(reserved) => {
            anyhow::anyhow!("'{}' is a reserved name and cannot be used", reserved)
        }
        ValidationError::ContainsPathSeparators => {
            anyhow::anyhow!("Team name cannot contain path separators (/ or \\)")
        }
        ValidationError::InvalidHyphenPlacement => {
            anyhow::anyhow!("Team name cannot start or end with a hyphen")
        }
        ValidationError::InvalidCharacter(ch) => {
            anyhow::anyhow!("Team name contains invalid character: '{}'", ch)
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
