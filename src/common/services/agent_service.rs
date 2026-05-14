//! Agent management service
//!
//! Provides unified filesystem-based agent operations used by both CLI and API.
//! All business logic for agent management lives here.

use crate::commands::agent_bootstrap::AgentBootstrap;
use crate::common::identifiers::{
    parse_agent_identifier_with_override, validate_agent_name, ValidationError,
};
use crate::common::paths::PathResolver;
use crate::common::services::TeamService;
use crate::common::types::agent::{
    AgentCreateRequest, AgentCreationResult, AgentDeleteOptions, AgentDeleteResult,
    AgentExportOptions, AgentExportResult, AgentImportOptions, AgentImportResult, AgentInfo,
    AgentRenameResult, AgentSummary, AgentUpdateRequest,
};
use crate::identity::Identity;
use crate::portable::{
    self, ExportOptions as PortableExportOptions, ImportOptions as PortableImportOptions,
};
use crate::types::agent::{AgentConfig, PromptConfig, SystemFileConfig};
use crate::types::provider::{ModelConfig, ProviderConfig, ProviderType};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Service for managing agents on the filesystem
#[derive(Debug, Clone)]
pub struct AgentService {
    resolver: PathResolver,
    team_service: TeamService,
}

// Helper functions for building default agent config (previously in deprecated agent_config_builder)

fn parse_provider_type(provider: &str) -> ProviderType {
    match provider.to_lowercase().as_str() {
        "openai" => ProviderType::OpenAI,
        "anthropic" => ProviderType::Anthropic,
        "ollama" => ProviderType::Ollama,
        "moonshot" => ProviderType::Moonshot,
        "kimi" => ProviderType::Kimi,
        "kimi_code" | "kimi-code" => ProviderType::Kimi,
        "minimax" => ProviderType::Minimax,
        _ => ProviderType::OpenAI,
    }
}

fn default_model_name(provider_type: ProviderType) -> String {
    match provider_type {
        ProviderType::OpenAI => "gpt-4o-mini".to_string(),
        ProviderType::Anthropic => "claude-3-sonnet".to_string(),
        ProviderType::Ollama => "llama3.2".to_string(),
        ProviderType::OpenAICompatible => "default".to_string(),
        ProviderType::Moonshot => "kimi-k2.5".to_string(),
        ProviderType::Kimi => "k2p5".to_string(),
        ProviderType::Minimax => "MiniMax-M2.7".to_string(),
    }
}

fn api_key_env_var(provider_type: ProviderType) -> Option<String> {
    match provider_type {
        ProviderType::OpenAI => Some("OPENAI_API_KEY".to_string()),
        ProviderType::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
        ProviderType::Moonshot => Some("MOONSHOT_API_KEY".to_string()),
        ProviderType::Kimi => Some("KIMI_API_KEY".to_string()),
        ProviderType::Minimax => Some("MINIMAX_API_KEY".to_string()),
        _ => None,
    }
}

fn base_url(provider_type: ProviderType) -> Option<String> {
    match provider_type {
        ProviderType::Ollama => Some("http://localhost:11434".to_string()),
        ProviderType::Moonshot => Some("https://api.moonshot.cn/v1".to_string()),
        ProviderType::Kimi => Some("https://api.kimi.com/coding".to_string()),
        ProviderType::Minimax => Some("https://api.minimaxi.com/anthropic".to_string()),
        _ => None,
    }
}

fn build_default_agent_config(name: &str, provider: &str, model: Option<String>) -> AgentConfig {
    let provider_type = parse_provider_type(provider);
    let default_model = model.unwrap_or_else(|| "default".to_string());

    let mut models = HashMap::new();
    models.insert(
        "default".to_string(),
        ModelConfig {
            name: default_model_name(provider_type),
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
        },
    );

    // Try to get API key from environment
    let api_key_env = api_key_env_var(provider_type);
    let api_key = api_key_env.as_ref().and_then(|env| std::env::var(env).ok());

    AgentConfig {
        version: "1.0".to_string(),
        name: name.to_string(),
        description: Some(format!("peko agent: {name}")),
        team: None,
        tenant: None,
        provider: ProviderConfig {
            provider_type,
            api_key,
            api_key_env,
            base_url: base_url(provider_type),
            default_model,
            models,
            timeout_seconds: 60,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
        // Include system file configuration for prompt building
        prompt: Some(PromptConfig {
            system: Some(SystemFileConfig {
                max_chars_per_file: 20_000,
                files: Some(vec!["SYSTEM.md".to_string()]),
            }),
        }),
        // Use defaults for the rest
        ..Default::default()
    }
}

impl AgentService {
    /// Create a new agent service with the given path resolver
    #[must_use]
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

            let team_name = team_path.file_name().map_or_else(
                || "unknown".to_string(),
                |n| n.to_string_lossy().to_string(),
            );

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

        // Use PathResolver for consistent path resolution (sessions in data_dir)
        let sessions_dir = self.resolver.agent_sessions_dir(agent_name, Some(team));
        let mut session_count = 0;

        if sessions_dir.exists() {
            if let Ok(mut entries) = tokio::fs::read_dir(&sessions_dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    if entry.path().extension().is_some_and(|e| e == "jsonl") {
                        session_count += 1;
                    }
                }
            }
        }

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
                .context(format!("Failed to auto-create team '{team}'"))?;
        }

        let config_path = self.resolver.agent_config(agent_name, Some(team));

        // Check if agent already exists
        if config_path.exists() && !request.force {
            anyhow::bail!(
                "Agent '{agent_name}' already exists in team '{team}'. Use --force to overwrite or delete it first."
            );
        }

        // Create agent directory
        let agent_dir = config_path.parent().unwrap();
        tokio::fs::create_dir_all(agent_dir).await?;

        // Get workspace path and create directory
        let workspace_dir = self.resolver.agent_workspace(agent_name, Some(team));
        tokio::fs::create_dir_all(&workspace_dir).await?;

        // Build config with workspace set
        let mut config = build_default_agent_config(agent_name, &request.provider, request.model);
        config.workspace = Some(workspace_dir.clone());
        let toml = toml::to_string_pretty(&config)?;

        tokio::fs::write(&config_path, toml).await?;

        // Bootstrap workspace with bootstrap files (AGENTS.md, SOUL.md, etc.)
        self.bootstrap_agent_workspace(agent_dir, agent_name, &workspace_dir)
            .await?;

        Ok(AgentCreationResult {
            name: agent_name.to_string(),
            team: team.to_string(),
            config_path,
            provider: request.provider,
        })
    }

    /// Delete an agent
    ///
    /// Removes the agent configuration, sessions, and workspace.
    pub async fn delete_agent(
        &self,
        name: &str,
        team: Option<&str>,
        opts: AgentDeleteOptions,
    ) -> Result<AgentDeleteResult> {
        let (team, agent_name) = parse_agent_identifier_with_override(name, team)?;
        let config_path = self.resolver.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found in team '{team}'");
        }

        // Remove agent config directory
        let agent_dir = config_path.parent().unwrap();
        tokio::fs::remove_dir_all(agent_dir).await?;

        // Remove sessions from data_dir
        let sessions_dir = self.resolver.agent_sessions_dir(agent_name, Some(team));
        let had_sessions = sessions_dir.exists();
        if had_sessions {
            tokio::fs::remove_dir_all(&sessions_dir).await.ok();
        }

        // Remove workspace from data_dir
        let workspace_dir = self.resolver.agent_workspace(agent_name, Some(team));
        if workspace_dir.exists() {
            tokio::fs::remove_dir_all(&workspace_dir).await.ok();
        }

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
            anyhow::bail!("Invalid new agent name '{new_name}': {e}");
        }

        let (from_team, old_agent_name) = parse_agent_identifier_with_override(old_name, team)?;
        let target_team = to_team.unwrap_or(from_team);

        let old_config_path = self.resolver.agent_config(old_agent_name, Some(from_team));
        if !old_config_path.exists() {
            anyhow::bail!("Agent '{old_agent_name}' not found in team '{from_team}'");
        }

        // Check if target team exists
        let target_team_dir = self.resolver.team_dir(target_team);
        if !target_team_dir.exists() {
            anyhow::bail!(
                "Target team '{target_team}' does not exist. Create it first with: peko team create {target_team}"
            );
        }

        // Check if target agent already exists
        let new_config_path = self.resolver.agent_config(new_name, Some(target_team));
        if new_config_path.exists() {
            anyhow::bail!("Agent '{new_name}' already exists in team '{target_team}'");
        }

        // Create target directory
        let new_agent_dir = new_config_path.parent().unwrap();
        tokio::fs::create_dir_all(new_agent_dir).await?;

        // Move config file
        tokio::fs::rename(&old_config_path, &new_config_path).await?;

        // Move sessions directory (in data_dir)
        let old_sessions_dir = self
            .resolver
            .agent_sessions_dir(old_agent_name, Some(from_team));
        let new_sessions_dir = self
            .resolver
            .agent_sessions_dir(new_name, Some(target_team));
        if old_sessions_dir.exists() {
            tokio::fs::create_dir_all(new_sessions_dir.parent().unwrap())
                .await
                .ok();
            tokio::fs::rename(&old_sessions_dir, &new_sessions_dir)
                .await
                .ok();
        }

        // Move workspace directory (in data_dir)
        let old_workspace = self
            .resolver
            .agent_workspace(old_agent_name, Some(from_team));
        let new_workspace = self.resolver.agent_workspace(new_name, Some(target_team));
        if old_workspace.exists() {
            tokio::fs::create_dir_all(new_workspace.parent().unwrap())
                .await
                .ok();
            // Remove target workspace if it exists (e.g., from previous failed move)
            if new_workspace.exists() {
                tokio::fs::remove_dir_all(&new_workspace).await.ok();
            }
            tokio::fs::rename(&old_workspace, &new_workspace).await.ok();
        }

        // Remove old agent directory
        let old_agent_dir = old_config_path.parent().unwrap();
        if old_agent_dir.exists() {
            tokio::fs::remove_dir_all(old_agent_dir).await?;
        }

        // Update config with new name, team, and workspace
        let mut config: AgentConfig =
            toml::from_str(&tokio::fs::read_to_string(&new_config_path).await?)?;
        config.name = new_name.to_string();
        config.workspace = Some(new_workspace);
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
            anyhow::bail!("Agent '{agent_name}' not found in team '{team}'");
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
            anyhow::bail!("Agent '{agent_name}' not found in team '{team}'");
        }

        let output_path = opts
            .output_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("{team}_{agent_name}.agent")));

        // Load agent config
        let config_content = tokio::fs::read_to_string(&config_path).await?;
        let config: AgentConfig =
            toml::from_str(&config_content).context("Failed to parse agent config")?;

        // Generate a new identity for the agent export
        let identity = Identity::new(agent_name, crate::identity::did::DIDScope::Local)
            .await
            .context("Failed to create identity for export")?;

        // Set up export paths
        let skills_dir = self.resolver.skills_dir();
        let workspace_dir = self.resolver.agent_workspace(agent_name, Some(team));
        let sessions_dir = self.resolver.agent_sessions_dir(agent_name, Some(team));
        let mcp_config_path = self.resolver.mcp_config();
        let tools_dir = self.resolver.tools_dir();

        // Only include MCP config if the file exists
        let mcp_config_path = if mcp_config_path.exists() {
            Some(mcp_config_path)
        } else {
            None
        };

        // Build portable export options
        let export_opts = PortableExportOptions {
            encrypt: false,
            passphrase: None,
            include_sessions: opts.include_sessions,
            include_workspace: true,
            rotate_keys: false,
            description: Some(format!("Exported agent {agent_name} from team {team}")),
            output_path: Some(output_path.to_string_lossy().to_string()),
            mcp_config_path,
            tools_dir: Some(tools_dir),
        };

        // Create packager and export
        let packager = portable::Packager::new(config, identity, None)
            .with_skills_dir(&skills_dir)
            .with_workspace_dir(&workspace_dir)
            .with_sessions_dir(&sessions_dir);

        let result_path = packager
            .export(export_opts)
            .await
            .context("Failed to export agent package")?;

        Ok(AgentExportResult {
            name: agent_name.to_string(),
            team: team.to_string(),
            output_path: result_path,
            encrypted: false,
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

        let team = opts.team.as_deref().unwrap_or("default");

        // Ensure team exists
        if !self.team_service.team_exists(team) {
            self.team_service.create_team(team, None).await?;
        }

        // Build portable import options
        let import_opts = PortableImportOptions {
            new_name: opts.name.clone(),
            passphrase: None,
            rotate_keys: true, // Always rotate keys on import for security
            import_sessions: true,
            import_workspace: true,
            skip_validation: false,
            force: false,
            team: Some(team.to_string()),
        };

        // Create unpackager with correct base directory for the team
        let team_dir = self.resolver.team_dir(team);
        let unpackager = portable::Unpackager::new(file_path)
            .with_base_dir(&team_dir)
            .with_team(team);

        // Import the package
        let result = unpackager
            .import(import_opts)
            .await
            .context("Failed to import agent package")?;

        Ok(AgentImportResult {
            name: result.name,
            team: team.to_string(),
            config_path: result.config_path,
        })
    }

    /// Check if an agent exists
    #[must_use]
    pub fn agent_exists(&self, name: &str, team: Option<&str>) -> bool {
        if let Ok((team, agent_name)) = parse_agent_identifier_with_override(name, team) {
            self.resolver.agent_config(agent_name, Some(team)).exists()
        } else {
            false
        }
    }

    /// Get the path resolver
    #[must_use]
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
        workspace_dir: &Path,
    ) -> Result<()> {
        // Create .gitignore
        let gitignore_content = r"# peko agent - gitignore
sessions/
workspace/
memories/
cron.json
*.log
";
        tokio::fs::write(agent_dir.join(".gitignore"), gitignore_content).await?;

        // Create empty directories for tools and skills (in agent_dir, not workspace)
        tokio::fs::create_dir_all(agent_dir.join("tools")).await?;
        tokio::fs::create_dir_all(agent_dir.join("skills")).await?;
        // Note: sessions directory is now in data_dir, created on first use via SessionManager

        // Create bootstrap files in workspace using AgentBootstrap
        let bootstrap = AgentBootstrap::new(agent_name, workspace_dir.to_path_buf());
        // Run in blocking task since AgentBootstrap uses std::fs
        tokio::task::spawn_blocking(move || bootstrap.run()).await??;

        Ok(())
    }
}

/// Map validation error to anyhow error with descriptive message
fn map_agent_validation_error(name: &str, e: ValidationError) -> anyhow::Error {
    match e {
        ValidationError::Empty => anyhow::anyhow!("Agent name cannot be empty"),
        ValidationError::TooLong(max) => {
            anyhow::anyhow!("Agent name '{name}' exceeds maximum length of {max} characters")
        }
        ValidationError::Reserved(reserved) => {
            anyhow::anyhow!("'{reserved}' is a reserved name and cannot be used")
        }
        ValidationError::ContainsPathSeparators => {
            anyhow::anyhow!("Agent name cannot contain path separators (/ or \\)")
        }
        ValidationError::InvalidHyphenPlacement => {
            anyhow::anyhow!("Agent name cannot start or end with a hyphen")
        }
        ValidationError::InvalidCharacter(ch) => {
            anyhow::anyhow!("Agent name contains invalid character: '{ch}'")
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
