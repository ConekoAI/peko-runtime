//! Agent management service
//!
//! Provides filesystem-based agent operations used by both CLI and API.
//! All business logic for agent management lives here.

use crate::common::identifiers::{
    parse_agent_identifier_with_override, validate_agent_name, ValidationError,
};
use crate::common::paths::PathResolver;
use crate::common::services::agent_config_builder::build_default_config;
use crate::common::types::agent::{
    AgentCreationResult, AgentInfo, AgentRenameResult, AgentSummary,
};
use crate::types::agent::AgentConfig;
use anyhow::Result;

/// Service for managing agents on the filesystem
#[derive(Debug, Clone)]
pub struct AgentService {
    resolver: PathResolver,
}

impl AgentService {
    /// Create a new agent service with the given path resolver
    pub fn new(resolver: PathResolver) -> Self {
        Self { resolver }
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
    pub async fn create_agent(
        &self,
        name: &str,
        team: Option<&str>,
        provider: &str,
        _model: Option<String>,
    ) -> Result<AgentCreationResult> {
        let (team, agent_name) = parse_agent_identifier_with_override(name, team)?;

        // Validate agent name
        if let Err(e) = validate_agent_name(agent_name) {
            return Err(map_agent_validation_error(agent_name, e));
        }

        let config_path = self.resolver.agent_config(agent_name, Some(team));

        // Check if agent already exists
        if config_path.exists() {
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
        let config = build_default_config(agent_name, provider, _model, None);
        let toml = toml::to_string_pretty(&config)?;

        tokio::fs::write(&config_path, toml).await?;

        Ok(AgentCreationResult {
            name: agent_name.to_string(),
            team: team.to_string(),
            config_path,
            provider: provider.to_string(),
        })
    }

    /// Delete an agent
    pub async fn delete_agent(&self, name: &str, team: Option<&str>) -> Result<()> {
        let (team, agent_name) = parse_agent_identifier_with_override(name, team)?;
        let config_path = self.resolver.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
        }

        // Remove entire agent directory
        let agent_dir = config_path.parent().unwrap();
        tokio::fs::remove_dir_all(agent_dir).await?;

        Ok(())
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
    pub async fn init_agent(
        &self,
        path: &str,
        name: Option<&str>,
        provider: &str,
        model: Option<String>,
        force: bool,
    ) -> Result<AgentCreationResult> {
        // Determine agent name from directory if not provided
        let agent_name = name.map(String::from).unwrap_or_else(|| {
            std::path::Path::new(path)
                .file_name()
                .map_or_else(|| "agent".to_string(), |n| n.to_string_lossy().to_string())
        });

        let dir = std::path::PathBuf::from(path);
        let config_path = dir.join("config.toml");

        // Check if directory already exists and has files
        if dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&dir)?.collect();
            if !entries.is_empty() && !force {
                anyhow::bail!("Directory not empty: {}", path);
            }
            // If force is true, remove existing directory
            if force {
                tokio::fs::remove_dir_all(&dir).await?;
            }
        }

        // Create directory
        tokio::fs::create_dir_all(&dir).await?;

        // Create config
        let config = build_default_config(&agent_name, provider, model, None);
        let config_content = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, config_content).await?;

        // Create .gitignore
        let gitignore_content = r#"# Pekobot agent - gitignore
sessions/
workspace/
memories/
cron.json
*.log
"#;
        tokio::fs::write(dir.join(".gitignore"), gitignore_content).await?;

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
        tokio::fs::write(dir.join("AGENT.md"), agent_md).await?;

        // Create empty directories
        tokio::fs::create_dir_all(dir.join("tools")).await?;
        tokio::fs::create_dir_all(dir.join("skills")).await?;
        tokio::fs::create_dir_all(dir.join("workspace")).await?;

        Ok(AgentCreationResult {
            name: agent_name,
            team: "default".to_string(),
            config_path,
            provider: provider.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_service_creation() {
        let resolver = PathResolver::new();
        let _service = AgentService::new(resolver);
    }
}
