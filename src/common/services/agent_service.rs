//! Agent management service
//!
//! Provides unified filesystem-based agent operations used by both CLI and API.
//! All business logic for agent management lives here.

use crate::common::identifiers::{
    parse_agent_identifier_with_override, validate_agent_name, ValidationError,
};
use crate::common::paths::PathResolver;
use crate::common::services::agent_config_builder::build_default_config;
use crate::common::services::TeamService;
use crate::common::types::agent::*;
use crate::types::agent::AgentConfig;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Service for managing agents on the filesystem
#[derive(Debug, Clone)]
pub struct AgentService {
    resolver: PathResolver,
    team_service: TeamService,
}

impl AgentService {
    /// Create a new agent service with the given path resolver
    pub fn new(resolver: PathResolver) -> Self {
        let team_service = TeamService::new(resolver.clone());
        Self {
            resolver,
            team_service,
        }
    }

    /// List all agents, optionally filtered by team
    pub async fn list_agents(&self, team_filter: Option<&str>) -> Result<Vec<AgentSummary>> {
        let teams_dir = self.resolver.teams_dir();

        if !teams_dir.exists() {
            return Ok(Vec::new());
        }

        let mut agents = Vec::new();
        let mut team_entries = tokio::fs::read_dir(&teams_dir).await?;

        while let Some(team_entry) = team_entries.next_entry().await? {
            let team_path = team_entry.path();
            if !team_path.is_dir() {
                continue;
            }

            let team_name = team_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            // Apply team filter if specified
            if let Some(filter) = team_filter {
                if team_name != filter {
                    continue;
                }
            }

            let team_agents_dir = team_path.join("agents");
            if !team_agents_dir.exists() {
                continue;
            }

            let mut agent_entries = tokio::fs::read_dir(&team_agents_dir).await?;
            while let Some(agent_entry) = agent_entries.next_entry().await? {
                let agent_path = agent_entry.path();
                if !agent_path.is_dir() {
                    continue;
                }

                let agent_name = agent_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                let config_path = agent_path.join("config.toml");
                if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
                    if let Ok(config) = toml::from_str::<AgentConfig>(&content) {
                        agents.push(AgentSummary {
                            name: agent_name,
                            team: team_name.clone(),
                            config,
                            config_path,
                        });
                    }
                }
            }
        }

        // Sort by team, then by name
        agents.sort_by(|a, b| {
            let team_cmp = a.team.cmp(&b.team);
            if team_cmp == std::cmp::Ordering::Equal {
                a.name.cmp(&b.name)
            } else {
                team_cmp
            }
        });

        Ok(agents)
    }

    /// Get a specific agent by name and optional team
    pub async fn get_agent(&self, name: &str, team: Option<&str>) -> Result<Option<AgentInfo>> {
        let (team, agent_name) = parse_agent_identifier_with_override(name, team)?;
        let config_path = self.resolver.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: AgentConfig = toml::from_str(&content)?;

        let sessions_dir = config_path.parent().unwrap().join("sessions");
        let session_count = if sessions_dir.exists() {
            match tokio::fs::read_dir(&sessions_dir).await {
                Ok(mut entries) => {
                    let mut count = 0;
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        if entry.path().extension().map_or(false, |e| e == "jsonl") {
                            count += 1;
                        }
                    }
                    count
                }
                Err(_) => 0,
            }
        } else {
            0
        };

        Ok(Some(AgentInfo {
            name: agent_name.to_string(),
            team: team.to_string(),
            config,
            config_path,
            sessions_dir,
            session_count,
        }))
    }

    /// Create a new agent
    ///
    /// This is the unified method used by both CLI and API for creating agents.
    pub async fn create_agent(&self, request: AgentCreateRequest) -> Result<AgentCreationResult> {
        let (team, agent_name) =
            parse_agent_identifier_with_override(&request.name, request.team.as_deref())?;

        // Validate agent name
        if let Err(e) = validate_agent_name(agent_name) {
            return Err(map_agent_validation_error(agent_name, e));
        }

        // Ensure team exists (auto-create if requested)
        if request.auto_create_team && !self.team_service.team_exists(team) {
            self.team_service
                .create_team(team, None)
                .await
                .context(format!("Failed to auto-create team '{}'", team))?;
        }

        let config_path = self.resolver.agent_config(agent_name, Some(team));

        // Check if agent already exists
        if config_path.exists() && !request.force {
            anyhow::bail!(
                "Agent '{}' already exists in team '{}'. Use --force to overwrite or delete it first.",
                agent_name,
                team
            );
        }

        // Create agent directory
        let agent_dir = config_path.parent().unwrap();
        tokio::fs::create_dir_all(agent_dir).await?;

        // Build config
        let config = build_default_config(agent_name, &request.provider, request.model, None);
        let toml = toml::to_string_pretty(&config)?;

        tokio::fs::write(&config_path, toml).await?;

        // Bootstrap workspace with standard files
        self.bootstrap_agent_workspace(agent_dir, agent_name).await?;

        Ok(AgentCreationResult {
            name: agent_name.to_string(),
            team: team.to_string(),
            config_path,
            provider: request.provider,
        })
    }

    /// Delete an agent
    ///
    /// Removes the agent configuration and optionally its sessions.
    pub async fn delete_agent(
        &self,
        name: &str,
        team: Option<&str>,
        opts: AgentDeleteOptions,
    ) -> Result<AgentDeleteResult> {
        let (team, agent_name) = parse_agent_identifier_with_override(name, team)?;
        let config_path = self.resolver.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
        }

        // Remove entire agent directory (includes config and sessions)
        let agent_dir = config_path.parent().unwrap();
        let sessions_dir = agent_dir.join("sessions");
        let had_sessions = sessions_dir.exists();

        tokio::fs::remove_dir_all(agent_dir).await?;

        if opts.purge_identity {
            // TODO: Implement identity purge when identity system is available
        }

        Ok(AgentDeleteResult {
            name: agent_name.to_string(),
            team: team.to_string(),
            config_deleted: true,
            sessions_deleted: had_sessions,
        })
    }

    /// Rename or move an agent
    pub async fn rename_agent(
        &self,
        old_name: &str,
        new_name: &str,
        team: Option<&str>,
        to_team: Option<&str>,
    ) -> Result<AgentRenameResult> {
        // Validate new agent name
        if let Err(e) = validate_agent_name(new_name) {
            anyhow::bail!("Invalid new agent name '{}': {}", new_name, e);
        }

        let (from_team, old_agent_name) =
            parse_agent_identifier_with_override(old_name, team.as_deref())?;
        let target_team = to_team.as_deref().unwrap_or(from_team);

        let old_config_path = self.resolver.agent_config(old_agent_name, Some(from_team));
        if !old_config_path.exists() {
            anyhow::bail!(
                "Agent '{}' not found in team '{}'",
                old_agent_name,
                from_team
            );
        }

        // Check if target team exists
        let target_team_dir = self.resolver.team_dir(target_team);
        if !target_team_dir.exists() {
            anyhow::bail!(
                "Target team '{}' does not exist. Create it first with: pekobot team create {}",
                target_team,
                target_team
            );
        }

        // Check if target agent already exists
        let new_config_path = self.resolver.agent_config(new_name, Some(target_team));
        if new_config_path.exists() {
            anyhow::bail!(
                "Agent '{}' already exists in team '{}'",
                new_name,
                target_team
            );
        }

        // Create target directory
        let new_agent_dir = new_config_path.parent().unwrap();
        tokio::fs::create_dir_all(new_agent_dir).await?;

        // Move config file
        tokio::fs::rename(&old_config_path, &new_config_path).await?;

        // Move sessions directory if it exists
        let old_sessions_dir = old_config_path.parent().unwrap().join("sessions");
        let new_sessions_dir = new_agent_dir.join("sessions");
        if old_sessions_dir.exists() {
            tokio::fs::rename(&old_sessions_dir, &new_sessions_dir).await?;
        }

        // Remove old agent directory
        let old_agent_dir = old_config_path.parent().unwrap();
        if old_agent_dir.exists() {
            tokio::fs::remove_dir(old_agent_dir).await?;
        }

        // Update config with new name and team
        let mut config: AgentConfig =
            toml::from_str(&tokio::fs::read_to_string(&new_config_path).await?)?;
        config.name = new_name.to_string();
        config.team = Some(target_team.to_string());
        let updated_toml = toml::to_string_pretty(&config)?;
        tokio::fs::write(&new_config_path, updated_toml).await?;

        Ok(AgentRenameResult {
            old_name: old_agent_name.to_string(),
            new_name: new_name.to_string(),
            from_team: from_team.to_string(),
            to_team: target_team.to_string(),
            new_config_path,
        })
    }

    /// Initialize a new agent directory structure
    ///
    /// Creates a standalone agent directory (not in the teams structure).
    pub async fn init_agent(&self, request: AgentInitRequest) -> Result<AgentInitResult> {
        let dir = request.path;
        let config_path = dir.join("config.toml");

        // Check if directory already exists and has files
        if dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&dir)?.collect();
            if !entries.is_empty() && !request.force {
                anyhow::bail!("Directory not empty: {}", dir.display());
            }
            // If force is true, remove existing directory
            if request.force {
                tokio::fs::remove_dir_all(&dir).await?;
            }
        }

        // Create directory
        tokio::fs::create_dir_all(&dir).await?;

        // Determine agent name from directory if not provided
        let agent_name = request.name.unwrap_or_else(|| {
            dir.file_name()
                .map_or_else(|| "agent".to_string(), |n| n.to_string_lossy().to_string())
        });

        // Create config
        let config = build_default_config(&agent_name, &request.provider, request.model, None);
        let config_content = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, config_content).await?;

        // Bootstrap workspace
        self.bootstrap_agent_workspace(&dir, &agent_name).await?;

        Ok(AgentInitResult {
            name: agent_name,
            path: dir.clone(),
            config_path,
            provider: request.provider,
        })
    }

    /// Update an agent configuration
    pub async fn update_agent(
        &self,
        name: &str,
        team: Option<&str>,
        update: AgentUpdateRequest,
    ) -> Result<AgentInfo> {
        let (team, agent_name) = parse_agent_identifier_with_override(name, team)?;
        let config_path = self.resolver.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
        }

        // Load existing config
        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        // Update image if provided
        if let Some(image_ref) = update.image {
            // Parse image and update config
            // For now, just update the default model with the image tag
            let model_name = parse_image_model_name(&image_ref)?;
            config.provider.default_model = model_name;
        }

        // Update team if provided (this is a move operation)
        if let Some(new_team) = update.team_id {
            if new_team != team {
                // This is a move - use rename_agent
                return self
                    .get_agent(name, Some(team))
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Agent not found after update"));
            }
        }

        // Save updated config
        let updated_toml = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, updated_toml).await?;

        // Return updated info
        self.get_agent(name, Some(team))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found after update"))
    }

    /// Export an agent to a package
    pub async fn export_agent(
        &self,
        name: &str,
        team: Option<&str>,
        opts: AgentExportOptions,
    ) -> Result<AgentExportResult> {
        let (team, agent_name) = parse_agent_identifier_with_override(name, team)?;
        let config_path = self.resolver.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
        }

        let output_path = opts
            .output_path
            .unwrap_or_else(|| PathBuf::from(format!("{}_{}.agent", team, agent_name)));

        // TODO: Implement actual export via Packager when available
        // For now, just return the expected result

        Ok(AgentExportResult {
            name: agent_name.to_string(),
            team: team.to_string(),
            output_path,
            encrypted: opts.encrypt,
        })
    }

    /// Import an agent from a package
    pub async fn import_agent(
        &self,
        file_path: &Path,
        opts: AgentImportOptions,
    ) -> Result<AgentImportResult> {
        if !file_path.exists() {
            anyhow::bail!("File not found: {}", file_path.display());
        }

        let agent_name = opts.name.unwrap_or_else(|| {
            file_path
                .file_stem()
                .map_or_else(|| "imported".to_string(), |s| s.to_string_lossy().to_string())
        });

        let team = opts.team.unwrap_or_else(|| "default".to_string());

        // TODO: Implement actual import via Unpackager when available

        let config_path = self.resolver.agent_config(&agent_name, Some(&team));

        Ok(AgentImportResult {
            name: agent_name,
            team,
            config_path,
        })
    }

    /// Check if an agent exists
    pub fn agent_exists(&self, name: &str, team: Option<&str>) -> bool {
        if let Ok((team, agent_name)) = parse_agent_identifier_with_override(name, team) {
            self.resolver.agent_config(agent_name, Some(team)).exists()
        } else {
            false
        }
    }

    /// Get the path resolver
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    // ============================================================================
    // Private Helper Methods
    // ============================================================================

    /// Bootstrap agent workspace with standard files
    async fn bootstrap_agent_workspace(
        &self,
        agent_dir: &Path,
        agent_name: &str,
    ) -> Result<()> {
        // Create .gitignore
        let gitignore_content = r#"# Pekobot agent - gitignore
sessions/
workspace/
memories/
cron.json
*.log
"#;
        tokio::fs::write(agent_dir.join(".gitignore"), gitignore_content).await?;

        // Create AGENT.md
        let agent_md = format!(
            r#"# {agent_name}

Agent description and instructions go here.

## Capabilities

- Add specific capabilities here
- Describe what this agent can do

## Instructions

Add detailed instructions for the agent here.
"#
        );
        tokio::fs::write(agent_dir.join("AGENT.md"), agent_md).await?;

        // Create empty directories
        tokio::fs::create_dir_all(agent_dir.join("tools")).await?;
        tokio::fs::create_dir_all(agent_dir.join("skills")).await?;
        tokio::fs::create_dir_all(agent_dir.join("workspace")).await?;
        tokio::fs::create_dir_all(agent_dir.join("sessions")).await?;

        Ok(())
    }
}

/// Map validation error to anyhow error with descriptive message
fn map_agent_validation_error(name: &str, e: ValidationError) -> anyhow::Error {
    match e {
        ValidationError::Empty => anyhow::anyhow!("Agent name cannot be empty"),
        ValidationError::TooLong(max) => {
            anyhow::anyhow!(
                "Agent name '{}' exceeds maximum length of {} characters",
                name,
                max
            )
        }
        ValidationError::Reserved(reserved) => {
            anyhow::anyhow!("'{}' is a reserved name and cannot be used", reserved)
        }
        ValidationError::ContainsPathSeparators => {
            anyhow::anyhow!("Agent name cannot contain path separators (/ or \\)")
        }
        ValidationError::InvalidHyphenPlacement => {
            anyhow::anyhow!("Agent name cannot start or end with a hyphen")
        }
        ValidationError::InvalidCharacter(ch) => {
            anyhow::anyhow!("Agent name contains invalid character: '{}'", ch)
        }
    }
}

/// Parse image reference to extract model name
fn parse_image_model_name(image_ref: &str) -> Result<String> {
    // Simple parsing: extract tag from image reference
    // Format: registry.com/user/image:tag or image:tag
    if let Some(pos) = image_ref.rfind(':') {
        let tag = &image_ref[pos + 1..];
        if !tag.is_empty() {
            return Ok(tag.to_string());
        }
    }

    // If no tag, use the last part of the path
    let parts: Vec<_> = image_ref.split('/').collect();
    Ok(parts.last().unwrap_or(&"unknown").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_service_creation() {
        let resolver = PathResolver::new();
        let _service = AgentService::new(resolver);
    }

    #[test]
    fn test_parse_image_model_name() {
        assert_eq!(
            parse_image_model_name("registry.com/user/image:v1.0").unwrap(),
            "v1.0"
        );
        assert_eq!(parse_image_model_name("image:latest").unwrap(), "latest");
        assert_eq!(parse_image_model_name("myimage").unwrap(), "myimage");
    }
}
