//! Agent management service
//!
//! Provides unified filesystem-based agent operations used by both CLI and API.
//! All business logic for agent management lives here.
//!
//! Agents are stored in the new layout at `agents/{agent}/config.toml`.
//! Agents are first-class citizens and team membership is managed separately
//! via `memberships.toml` files.

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
use crate::common::types::membership::AgentMemberships;
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
        // team and tenant removed - agents are standalone in new layout
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

    // ========================================================================
    // Agent Discovery
    // ========================================================================

    /// List all agents.
    ///
    /// Optionally filter by team membership (reads agent memberships file).
    pub async fn list_agents(&self, team_filter: Option<&str>) -> Result<Vec<AgentSummary>> {
        let mut agents = Vec::new();

        // Scan new layout: agents/{agent}/config.toml
        let agents_root = self.resolver.agents_root_dir();
        if agents_root.exists() {
            if let Ok(mut entries) = tokio::fs::read_dir(&agents_root).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let agent_path = entry.path();
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
                            // Apply team filter using memberships
                            if let Some(filter) = team_filter {
                                let memberships =
                                    AgentMemberships::load(&self.resolver.agent_memberships(&agent_name))
                                        .unwrap_or_default();
                                if !memberships.belongs_to(filter) {
                                    continue;
                                }
                            }

                            let memberships = AgentMemberships::load(&self.resolver.agent_memberships(&agent_name))
                                .unwrap_or_default()
                                .memberships
                                .into_iter()
                                .map(|m| m.team)
                                .collect();
                            agents.push(AgentSummary {
                                name: agent_name,
                                config,
                                config_path,
                                memberships,
                            });
                        }
                    }
                }
            }
        }

        // Sort by name
        agents.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(agents)
    }

    /// Get a specific agent by name.
    pub async fn get_agent(&self, name: &str, _team: Option<&str>) -> Result<Option<AgentInfo>> {
        let (_, agent_name) = parse_agent_identifier_with_override(name, None)?;

        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: AgentConfig = toml::from_str(&content)?;

        let sessions_dir = self.resolver.agent_personal_sessions_dir(agent_name);
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

        let memberships = AgentMemberships::load(&self.resolver.agent_memberships(agent_name))
            .unwrap_or_default()
            .memberships
            .into_iter()
            .map(|m| m.team)
            .collect();

        Ok(Some(AgentInfo {
            name: agent_name.to_string(),
            config,
            config_path,
            sessions_dir,
            session_count,
            memberships,
        }))
    }

    // ========================================================================
    // Agent CRUD (New Layout)
    // ========================================================================

    /// Create a new agent in the top-level `agents/` directory.
    pub async fn create_agent(
        &self,
        request: AgentCreateRequest,
    ) -> Result<AgentCreationResult> {
        let name = &request.name;

        // Validate agent name
        if let Err(e) = validate_agent_name(name) {
            return Err(map_agent_validation_error(name, e));
        }

        let config_path = self.resolver.agent_config(name);

        // Check if agent already exists
        if self.resolver.agent_exists(name) && !request.force {
            anyhow::bail!(
                "Agent '{name}' already exists. Use --force to overwrite or delete it first."
            );
        }

        // Create agent directory
        let agent_dir = self.resolver.agent_dir(name);
        tokio::fs::create_dir_all(&agent_dir).await?;

        // Create personal workspace directory
        let workspace_dir = self.resolver.agent_personal_workspace(name);
        tokio::fs::create_dir_all(&workspace_dir).await?;

        // Build config with workspace set
        let mut config = build_default_agent_config(name, &request.provider, request.model);
        config.workspace = Some(workspace_dir.clone());
        if let Some(ref host_id) = request.host_runtime_id {
            config.host_runtime_id = host_id.clone();
        }
        if let Some(ref owner_id) = request.owner_id {
            config.owner_id = owner_id.clone();
        }
        let toml = toml::to_string_pretty(&config)?;

        tokio::fs::write(&config_path, toml).await?;

        // Initialize empty memberships file
        let memberships = AgentMemberships::new();
        memberships.save(&self.resolver.agent_memberships(name))?;

        // Bootstrap workspace with standard files
        self.bootstrap_agent_workspace(&agent_dir, name, &workspace_dir)
            .await?;

        Ok(AgentCreationResult {
            name: name.to_string(),
            config_path,
            provider: request.provider,
        })
    }

    /// Delete an agent.
    pub async fn delete_agent(
        &self,
        name: &str,
        _team: Option<&str>,
        opts: AgentDeleteOptions,
    ) -> Result<AgentDeleteResult> {
        let (_, agent_name) = parse_agent_identifier_with_override(name, None)?;

        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        // Remove agent config directory
        let agent_dir = config_path.parent().unwrap();
        tokio::fs::remove_dir_all(agent_dir).await?;

        // Remove personal sessions and workspaces
        let personal_sessions = self.resolver.agent_personal_sessions_dir(agent_name);
        let had_sessions = personal_sessions.exists();
        if had_sessions {
            tokio::fs::remove_dir_all(&personal_sessions).await.ok();
        }

        let personal_workspace = self.resolver.agent_personal_workspace(agent_name);
        if personal_workspace.exists() {
            tokio::fs::remove_dir_all(&personal_workspace).await.ok();
        }

        // Remove team-context sessions and workspaces
        let sessions_root = self.resolver.agent_sessions_root(agent_name);
        if sessions_root.exists() {
            tokio::fs::remove_dir_all(&sessions_root).await.ok();
        }

        let workspaces_root = self.resolver.agent_workspaces_root(agent_name);
        if workspaces_root.exists() {
            tokio::fs::remove_dir_all(&workspaces_root).await.ok();
        }

        if opts.purge_identity {
            // TODO: Implement identity purge
        }

        Ok(AgentDeleteResult {
            name: agent_name.to_string(),
            config_deleted: true,
            sessions_deleted: had_sessions,
        })
    }

    /// Rename an agent.
    pub async fn rename_agent(
        &self,
        old_name: &str,
        new_name: &str,
        _team: Option<&str>,
    ) -> Result<AgentRenameResult> {
        // Validate new agent name
        if let Err(e) = validate_agent_name(new_name) {
            anyhow::bail!("Invalid new agent name '{new_name}': {e}");
        }

        let (_, old_agent_name) = parse_agent_identifier_with_override(old_name, None)?;

        let old_config_path = self.resolver.agent_config(old_agent_name);
        if !old_config_path.exists() {
            anyhow::bail!("Agent '{old_agent_name}' not found");
        }

        let new_config_path = self.resolver.agent_config(new_name);
        if new_config_path.exists() {
            anyhow::bail!("Agent '{new_name}' already exists");
        }

        let old_agent_dir = self.resolver.agent_dir(old_agent_name);
        let new_agent_dir = self.resolver.agent_dir(new_name);

        // Rename config directory
        tokio::fs::rename(&old_agent_dir, &new_agent_dir).await?;

        // Rename workspace directories
        let old_workspace = self.resolver.agent_workspaces_root(old_agent_name);
        let new_workspace = self.resolver.agent_workspaces_root(new_name);
        if old_workspace.exists() {
            tokio::fs::rename(&old_workspace, &new_workspace).await.ok();
        }

        // Rename session directories
        let old_sessions = self.resolver.agent_sessions_root(old_agent_name);
        let new_sessions = self.resolver.agent_sessions_root(new_name);
        if old_sessions.exists() {
            tokio::fs::rename(&old_sessions, &new_sessions).await.ok();
        }

        // Update config with new name
        let config_path = self.resolver.agent_config(new_name);
        let mut config: AgentConfig =
            toml::from_str(&tokio::fs::read_to_string(&config_path).await?)?;
        config.name = new_name.to_string();
        let updated_toml = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, updated_toml).await?;

        Ok(AgentRenameResult {
            old_name: old_agent_name.to_string(),
            new_name: new_name.to_string(),
            new_config_path: config_path,
        })
    }

    /// Update an agent configuration
    pub async fn update_agent(
        &self,
        name: &str,
        _team: Option<&str>,
        update: AgentUpdateRequest,
    ) -> Result<AgentInfo> {
        let (_, agent_name) = parse_agent_identifier_with_override(name, None)?;

        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        if let Some(image_ref) = update.image {
            let model_name = parse_image_model_name(&image_ref)?;
            config.provider.default_model = model_name;
        }

        let updated_toml = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, updated_toml).await?;

        self.get_agent(name, None).await?.ok_or_else(|| anyhow::anyhow!("Agent not found after update"))
    }

    /// Export an agent to a package
    pub async fn export_agent(
        &self,
        name: &str,
        _team: Option<&str>,
        opts: AgentExportOptions,
    ) -> Result<AgentExportResult> {
        let (_, agent_name) = parse_agent_identifier_with_override(name, None)?;

        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let output_path = opts
            .output_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("{agent_name}.agent")));

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
        let workspace_dir = self.resolver.agent_personal_workspace(agent_name);
        let sessions_dir = self.resolver.agent_personal_sessions_dir(agent_name);
        let mcp_config_path = self.resolver.mcp_config();
        let tools_dir = self.resolver.tools_dir();

        let mcp_config_path = if mcp_config_path.exists() {
            Some(mcp_config_path)
        } else {
            None
        };

        let export_opts = PortableExportOptions {
            encrypt: false,
            passphrase: None,
            include_sessions: opts.include_sessions,
            include_workspace: true,
            rotate_keys: false,
            description: Some(format!("Exported agent: {agent_name}")),
            output_path: Some(output_path.to_string_lossy().to_string()),
            mcp_config_path,
            tools_dir: Some(tools_dir),
        };

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

        // Default to new layout (standalone agent)
        let agent_name = opts.name.as_deref().unwrap_or("imported");

        // Build portable import options
        let import_opts = PortableImportOptions {
            new_name: opts.name.clone(),
            passphrase: None,
            rotate_keys: true,
            import_sessions: true,
            import_workspace: true,
            skip_validation: false,
            force: opts.force,
            team: None, // New layout: no team
        };

        // Create unpackager with agents root as base directory
        let agents_root = self.resolver.agents_root_dir();
        let unpackager = portable::Unpackager::new(file_path).with_base_dir(&agents_root);

        // Import the package
        let result = unpackager
            .import(import_opts)
            .await
            .context("Failed to import agent package")?;

        // Initialize empty memberships file
        let memberships = AgentMemberships::new();
        memberships.save(&self.resolver.agent_memberships(&result.name))?;

        Ok(AgentImportResult {
            name: result.name,
            config_path: result.config_path,
        })
    }

    /// Check if an agent exists
    #[must_use]
    pub fn agent_exists(&self, name: &str) -> bool {
        if let Ok((_, agent_name)) = parse_agent_identifier_with_override(name, None) {
            self.resolver.agent_exists(agent_name)
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
    // Ownership and Permission (ADR-033)
    // ============================================================================

    /// Transfer ownership of an agent.
    pub async fn transfer_agent_owner(
        &self,
        name: &str,
        new_owner_id: &str,
        caller_subject: &str,
    ) -> Result<()> {
        let (_, agent_name) = parse_agent_identifier_with_override(name, None)?;
        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        // Only current owner can transfer
        if config.owner_id != caller_subject {
            anyhow::bail!("Permission denied: only the owner can transfer ownership");
        }

        config.owner_id = new_owner_id.to_string();
        let updated = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, updated).await?;
        Ok(())
    }

    /// Grant a permission on an agent.
    pub async fn grant_agent_permission(
        &self,
        name: &str,
        grant: crate::auth::ownership::PermissionGrant,
        caller_subject: &str,
    ) -> Result<()> {
        let (_, agent_name) = parse_agent_identifier_with_override(name, None)?;
        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        // Only owner can grant permissions
        if config.owner_id != caller_subject {
            anyhow::bail!("Permission denied: only the owner can grant permissions");
        }

        // Remove existing grant for same subject+permission
        config.permissions.retain(|g| {
            !(g.subject_id == grant.subject_id
                && std::mem::discriminant(&g.permission) == std::mem::discriminant(&grant.permission))
        });
        config.permissions.push(grant);

        let updated = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, updated).await?;
        Ok(())
    }

    /// Revoke a permission from an agent.
    pub async fn revoke_agent_permission(
        &self,
        name: &str,
        subject_id: &str,
        permission: &crate::auth::ownership::Permission,
        caller_subject: &str,
    ) -> Result<()> {
        let (_, agent_name) = parse_agent_identifier_with_override(name, None)?;
        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        // Only owner can revoke permissions
        if config.owner_id != caller_subject {
            anyhow::bail!("Permission denied: only the owner can revoke permissions");
        }

        config.permissions.retain(|g| {
            !(g.subject_id == subject_id
                && std::mem::discriminant(&g.permission) == std::mem::discriminant(permission))
        });

        let updated = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, updated).await?;
        Ok(())
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

    // ========================================================================
    // New Layout Tests
    // ========================================================================

    #[tokio::test]
    async fn test_create_agent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);

        let request = AgentCreateRequest::new("alice", "minimax");
        let result = service.create_agent(request).await.unwrap();

        assert_eq!(result.name, "alice");
        assert!(result.config_path.exists());
        assert!(result.config_path.to_string_lossy().contains("agents"));
        assert!(result.config_path.to_string_lossy().contains("alice"));

        // Check memberships file was created
        let memberships_path = service.resolver.agent_memberships("alice");
        assert!(memberships_path.exists());
    }

    #[tokio::test]
    async fn test_create_agent_duplicate_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);

        let request = AgentCreateRequest::new("alice", "minimax");
        service.create_agent(request).await.unwrap();

        let request = AgentCreateRequest::new("alice", "minimax");
        let result = service.create_agent(request).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_list_agents() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);

        let request = AgentCreateRequest::new("alice", "minimax");
        service.create_agent(request).await.unwrap();
        let request = AgentCreateRequest::new("bob", "minimax");
        service.create_agent(request).await.unwrap();

        let agents = service.list_agents(None).await.unwrap();
        assert_eq!(agents.len(), 2);
        assert!(agents.iter().any(|a| a.name == "alice"));
        assert!(agents.iter().any(|a| a.name == "bob"));
    }

    #[tokio::test]
    async fn test_get_agent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);

        let request = AgentCreateRequest::new("alice", "minimax");
        service.create_agent(request).await.unwrap();

        let info = service.get_agent("alice", None).await.unwrap();
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.name, "alice");
        assert_eq!(info.name, "alice");
    }

    #[tokio::test]
    async fn test_delete_agent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);
        let service = AgentService::new(resolver);

        let request = AgentCreateRequest::new("alice", "minimax");
        service.create_agent(request).await.unwrap();

        let result = service
            .delete_agent("alice", None, AgentDeleteOptions::default())
            .await
            .unwrap();

        assert_eq!(result.name, "alice");
        assert!(!config_dir.join("agents").join("alice").exists());
    }

    #[tokio::test]
    async fn test_rename_agent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);

        let request = AgentCreateRequest::new("alice", "minimax");
        service.create_agent(request).await.unwrap();

        let result = service
            .rename_agent("alice", "alicia", None)
            .await
            .unwrap();

        assert_eq!(result.old_name, "alice");
        assert_eq!(result.new_name, "alicia");

        // Old should not exist
        assert!(service.get_agent("alice", None).await.unwrap().is_none());
        // New should exist
        assert!(service.get_agent("alicia", None).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_create_agent_creates_in_new_layout() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);

        let request = AgentCreateRequest::new("alice", "minimax");
        let result = service.create_agent(request).await.unwrap();

        assert_eq!(result.name, "alice");
        assert!(result.config_path.to_string_lossy().contains("agents"));
        assert!(result.config_path.to_string_lossy().contains("alice"));
    }
}
