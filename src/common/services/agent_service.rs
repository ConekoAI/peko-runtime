//! Agent management service
//!
//! Provides unified filesystem-based agent operations used by both CLI and API.
//! All business logic for agent management lives here.
//!
//! Agents are stored in the new layout at `agents/{agent}/config.toml`.

use crate::agents::agent_config::{AgentConfig, PromptConfig, SystemFileConfig};
use crate::commands::agent_bootstrap::AgentBootstrap;
use crate::common::identifiers::{
    parse_agent_name, validate_agent_name, ValidationError,
};
use crate::common::paths::PathResolver;
use crate::common::types::agent::{
    AgentCreateRequest, AgentCreationResult, AgentDeleteOptions, AgentDeleteResult, AgentInfo,
    AgentRenameResult, AgentSummary, AgentUpdateRequest,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Service for managing agents on the filesystem
#[derive(Debug, Clone)]
pub struct AgentService {
    resolver: PathResolver,
    /// Optional principal workspace. When set, the `Agent` tool will first
    /// look for subagents under `<workspace>/agents/<name>/AGENT.md` before
    /// falling back to the global `~/.peko/agents/<name>/config.toml` layout.
    principal_workspace: Option<PathBuf>,
}

fn build_default_agent_config(name: &str, provider: &str, model: Option<String>) -> AgentConfig {
    // v3: agent config carries only soft hints (`preferred_*`); the
    // actual provider/model wiring lives in the catalog + keychain.
    // The old `ProviderConfig` field is `skip_serializing` and goes
    // away in commit 2.
    let preferred_model_id = Some(model.unwrap_or_else(|| "default".to_string()));

    AgentConfig {
        version: "3.0".to_string(),
        name: name.to_string(),
        description: Some(format!("peko agent: {name}")),
        preferred_provider_id: Some(provider.to_string()),
        preferred_model_id,
        prompt: Some(PromptConfig {
            system: Some(SystemFileConfig {
                max_chars_per_file: 20_000,
                files: Some(vec!["SYSTEM.md".to_string()]),
            }),
        }),
        ..Default::default()
    }
}

impl AgentService {
    /// Create a new agent service with the given path resolver
    #[must_use]
    pub fn new(resolver: PathResolver) -> Self {
        Self {
            resolver,
            principal_workspace: None,
        }
    }

    /// Create an agent service scoped to a Principal workspace.
    ///
    /// Subagent resolution will prefer principal agents (`agents/<name>/AGENT.md`)
    /// and fall back to global agents if no matching principal agent exists.
    #[must_use]
    pub fn for_principal(workspace: impl Into<PathBuf>) -> Self {
        Self {
            resolver: PathResolver::new(),
            principal_workspace: Some(workspace.into()),
        }
    }

    // ========================================================================
    // Agent Discovery
    // ========================================================================

    /// List all agents.
    pub async fn list_agents(&self, _team_filter: Option<&str>) -> Result<Vec<AgentSummary>> {
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
                            agents.push(AgentSummary {
                                name: agent_name,
                                config,
                                config_path,
                                memberships: Vec::new(),
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
        let agent_name = parse_agent_name(name)?;

        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: AgentConfig = toml::from_str(&content)?;

        let sessions_dir = self.resolver.agent_sessions_dir(agent_name);
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

        let memberships = Vec::new();

        // Resolve the first configured system prompt file, if any.
        let system_prompt = config
            .prompt
            .as_ref()
            .and_then(|p| p.system.as_ref())
            .and_then(|s| s.files.as_ref())
            .and_then(|files| files.first())
            .and_then(|file| {
                let workspace_dir = self.resolver.agent_workspace(agent_name);
                let path = workspace_dir.join(file);
                std::fs::read_to_string(&path).ok()
            });

        Ok(Some(AgentInfo {
            name: agent_name.to_string(),
            config,
            config_path,
            sessions_dir,
            session_count,
            memberships,
            system_prompt,
        }))
    }

    /// Load an agent config by name from the filesystem.
    ///
    /// This is the resolution hook for the `Agent` tool's `subagent_type`
    /// parameter: `subagent_type` maps to `~/.peko/agents/<name>/config.toml`.
    ///
    /// If a principal workspace was configured, the service first tries to
    /// resolve the agent from `<workspace>/agents/<name>/AGENT.md`.
    pub async fn resolve_subagent_type(&self, name: &str) -> Result<AgentConfig> {
        if let Some(ref workspace) = self.principal_workspace {
            match self.resolve_principal_agent(name, workspace).await {
                Ok(config) => return Ok(config),
                Err(e) => {
                    tracing::debug!(
                        "Principal agent '{name}' not found in workspace '{}': {e}; falling back to global agent",
                        workspace.display()
                    );
                }
            }
        }

        let agent_name = parse_agent_name(name)?;
        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Subagent type '{name}' not found at {config_path:?}");
        }
        let content = tokio::fs::read_to_string(&config_path).await?;
        let config: AgentConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse agent config for '{name}'"))?;
        Ok(config)
    }

    /// Resolve a principal agent from its `AGENT.md` extension.
    async fn resolve_principal_agent(
        &self,
        name: &str,
        workspace: &Path,
    ) -> Result<AgentConfig> {
        let agent_md = workspace.join("agents").join(name).join("AGENT.md");
        if !agent_md.exists() {
            anyhow::bail!("No AGENT.md for principal agent '{name}' at {agent_md:?}");
        }

        let prompt = crate::principal::agent_prompt::load_agent_prompt(&agent_md)
            .with_context(|| format!("Failed to load principal agent prompt '{name}'"))?;

        let extensions = crate::common::types::agent_legacy::ExtensionConfig::default();

        Ok(AgentConfig {
            version: "3.0".to_string(),
            name: prompt.name,
            description: prompt.frontmatter.description,
            prompt: Some(crate::agents::agent_config::PromptConfig {
                system: Some(crate::agents::agent_config::SystemFileConfig {
                    max_chars_per_file: 200_000,
                    files: Some(vec![agent_md.to_string_lossy().to_string()]),
                }),
            }),
            extensions: Some(extensions),
            ..AgentConfig::default()
        })
    }

    // ========================================================================
    // Agent CRUD (New Layout)
    // ========================================================================

    /// Create a new agent in the top-level `agents/` directory.
    pub async fn create_agent(&self, request: AgentCreateRequest) -> Result<AgentCreationResult> {
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

        // v3: validate the requested provider exists in the catalog
        // before writing the agent config. The runtime owns the
        // provider wiring — agents are now thin shells with only
        // soft hints.
        //
        // Skip the check on a fresh install (empty catalog) to match
        // the v1-to-v3 migration's behavior: it seeds catalog entries
        // from the legacy `[provider]` block. The strict check fires
        // once the user has run `peko provider add` and we know
        // they understand the catalog concept.
        let catalog_path = self.resolver.config_dir().join("providers.toml");
        let provider_id = request.provider.clone();
        if let Ok(catalog) = crate::providers::ProviderCatalog::load_or_init(&catalog_path).await {
            let enabled = catalog.list_enabled().await;
            if !enabled.is_empty() && catalog.get_enabled(&provider_id).await.is_none() {
                let available = enabled
                    .iter()
                    .map(|e| e.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!(
                    "provider '{}' is not in the catalog. Run `peko provider add {}` first. \
                     Available providers: {}. See `peko provider templates` for built-in templates.",
                    provider_id,
                    provider_id,
                    available
                );
            }
        }

        // Create agent directory
        let agent_dir = self.resolver.agent_dir(name);
        tokio::fs::create_dir_all(&agent_dir).await?;

        // Create personal workspace directory
        let workspace_dir = self.resolver.agent_workspace(name);
        tokio::fs::create_dir_all(&workspace_dir).await?;

        // Build config with workspace set
        let mut config = build_default_agent_config(name, &request.provider, request.model);
        config.workspace = Some(workspace_dir.clone());
        if let Some(ref host_id) = request.host_runtime_id {
            config.host_runtime_id = host_id.clone();
        }
        if let Some(owner) = request.owner {
            config.owner = owner;
        }
        let toml = toml::to_string_pretty(&config)?;

        tokio::fs::write(&config_path, toml).await?;

        // Bootstrap workspace with standard files
        self.bootstrap_agent_workspace(&agent_dir, name, &workspace_dir)
            .await?;

        Ok(AgentCreationResult {
            name: name.clone(),
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
        let agent_name = parse_agent_name(name)?;

        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        // Remove agent config directory
        let agent_dir = config_path.parent().unwrap();
        tokio::fs::remove_dir_all(agent_dir).await?;

        // Remove personal sessions and workspaces
        let personal_sessions = self.resolver.agent_sessions_dir(agent_name);
        let had_sessions = personal_sessions.exists();
        if had_sessions {
            tokio::fs::remove_dir_all(&personal_sessions).await.ok();
        }

        let personal_workspace = self.resolver.agent_workspace(agent_name);
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

        let old_agent_name = parse_agent_name(old_name)?;

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
        let agent_name = parse_agent_name(name)?;

        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        if let Some(image_ref) = update.image {
            let model_name = parse_image_model_name(&image_ref)?;
            // v3: model lives in the catalog + secret store; the
            // agent only carries a soft hint.
            config.preferred_model_id = Some(model_name);
        }

        if let Some(model) = update.model {
            config.preferred_model_id = Some(model);
        }

        if update.description.is_some() {
            config.description = update.description;
        }

        if let Some(system_prompt) = update.system_prompt {
            if !system_prompt.is_empty() {
                config.prompt = Some(PromptConfig {
                    system: Some(SystemFileConfig {
                        files: Some(vec!["SYSTEM.md".to_string()]),
                        ..Default::default()
                    }),
                });
                // Write the system prompt to a SYSTEM.md file in the agent workspace
                let workspace_dir = self.resolver.agent_workspace(agent_name);
                tokio::fs::create_dir_all(&workspace_dir).await.ok();
                let system_md_path = workspace_dir.join("SYSTEM.md");
                tokio::fs::write(&system_md_path, &system_prompt).await?;
            } else {
                config.prompt = None;
            }
        }

        if let Some(patch) = update.config {
            // Merge the JSON patch into the TOML config by converting both to serde_json::Value
            let mut config_json = serde_json::to_value(&config)?;
            if let Some(patch_obj) = patch.as_object() {
                for (key, value) in patch_obj {
                    config_json[key] = value.clone();
                }
            }
            config = serde_json::from_value(config_json)?;
        }

        let updated_toml = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, updated_toml).await?;

        self.get_agent(name, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found after update"))
    }

    #[must_use]
    pub fn agent_exists(&self, name: &str) -> bool {
        if let Ok(agent_name) = parse_agent_name(name) {
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
        new_owner: crate::auth::Subject,
        caller: &crate::auth::Subject,
    ) -> Result<()> {
        let agent_name = parse_agent_name(name)?;
        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        // Only current owner can transfer
        if &config.owner != caller {
            anyhow::bail!("Permission denied: only the owner can transfer ownership");
        }

        config.owner = new_owner;
        let updated = toml::to_string_pretty(&config)?;
        tokio::fs::write(&config_path, updated).await?;
        Ok(())
    }

    /// Grant a permission on an agent.
    pub async fn grant_agent_permission(
        &self,
        name: &str,
        grant: crate::auth::ownership::PermissionGrant,
        caller: &crate::auth::Subject,
    ) -> Result<()> {
        let agent_name = parse_agent_name(name)?;
        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        // Only owner can grant permissions
        if &config.owner != caller {
            anyhow::bail!("Permission denied: only the owner can grant permissions");
        }

        // Remove existing grant for same subject+permission
        let grant_disc = std::mem::discriminant(&grant.permission);
        config.permissions.retain(|g| {
            !(g.subject == grant.subject && std::mem::discriminant(&g.permission) == grant_disc)
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
        subject: &crate::auth::Subject,
        permission: &crate::auth::ownership::Permission,
        caller: &crate::auth::Subject,
    ) -> Result<()> {
        let agent_name = parse_agent_name(name)?;
        let config_path = self.resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = tokio::fs::read_to_string(&config_path).await?;
        let mut config: AgentConfig = toml::from_str(&content)?;

        // Only owner can revoke permissions
        if &config.owner != caller {
            anyhow::bail!("Permission denied: only the owner can revoke permissions");
        }

        let perm_disc = std::mem::discriminant(permission);
        config.permissions.retain(|g| {
            !(g.subject == *subject && std::mem::discriminant(&g.permission) == perm_disc)
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

    /// Seed a `ProviderCatalog` on disk with the providers the legacy
    /// `create_agent` tests request (`minimax`, `openai`, etc.).
    /// Production code does this via `peko provider add`; the unit
    /// tests below all predate the catalog-validation step in
    /// `create_agent` (commit 1.5) and would otherwise fail at the
    /// catalog check. Centralized so each test is one line.
    ///
    /// We write to disk directly (rather than upserting through the
    /// `ProviderCatalog` API) because the daemon's `create_agent`
    /// re-loads the catalog via `load_or_init` — two separate
    /// in-memory `Arc<ProviderCatalog>`s would not share state.
    async fn seed_test_catalog(resolver: &PathResolver) {
        use crate::providers::catalog::{
            ApiFormat, ModelInfo, ProviderCatalogEntry, ProviderCatalogFile,
        };
        let catalog_path = resolver.config_dir().join("providers.toml");
        if let Some(parent) = catalog_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let entries = [
            (
                "minimax",
                ApiFormat::AnthropicMessages,
                "https://api.minimaxi.com/anthropic",
                "MiniMax-M3",
            ),
            (
                "openai",
                ApiFormat::OpenaiCompletions,
                "https://api.openai.com/v1",
                "gpt-4o-mini",
            ),
            (
                "anthropic",
                ApiFormat::AnthropicMessages,
                "https://api.anthropic.com",
                "claude-3-5-sonnet-latest",
            ),
        ];
        let now = chrono::Utc::now();
        let provider_entries: std::collections::BTreeMap<String, ProviderCatalogEntry> = entries
            .iter()
            .map(|(id, fmt, base, default_model)| {
                (
                    id.to_string(),
                    ProviderCatalogEntry {
                        id: id.to_string(),
                        display_name: id.to_string(),
                        template_id: None,
                        api_format: *fmt,
                        base_url: base.to_string(),
                        default_model_id: default_model.to_string(),
                        models: vec![ModelInfo {
                            id: default_model.to_string(),
                            display_name: None,
                            context_length: None,
                            max_output_tokens: None,
                            capabilities: vec![],
                        }],
                        headers: std::collections::BTreeMap::new(),
                        requires_key: true,
                        enabled: true,
                        created_at: now,
                        updated_at: now,
                    },
                )
            })
            .collect();
        let file = ProviderCatalogFile {
            version: "3.0".to_string(),
            entries: provider_entries,
            default_provider_id: None,
            default_model_id: None,
        };
        let toml = toml::to_string_pretty(&file).unwrap();
        std::fs::write(&catalog_path, toml).unwrap();
    }

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
        seed_test_catalog(&service.resolver).await;

        let request = AgentCreateRequest::new("alice", "minimax");
        let result = service.create_agent(request).await.unwrap();

        assert_eq!(result.name, "alice");
        assert!(result.config_path.exists());
        assert!(result.config_path.to_string_lossy().contains("agents"));
        assert!(result.config_path.to_string_lossy().contains("alice"));
    }

    #[tokio::test]
    async fn test_create_agent_duplicate_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);
        seed_test_catalog(&service.resolver).await;

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
        seed_test_catalog(&service.resolver).await;

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
        seed_test_catalog(&service.resolver).await;

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
        seed_test_catalog(&service.resolver).await;

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
        seed_test_catalog(&service.resolver).await;

        let request = AgentCreateRequest::new("alice", "minimax");
        service.create_agent(request).await.unwrap();

        let result = service.rename_agent("alice", "alicia", None).await.unwrap();

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
        seed_test_catalog(&service.resolver).await;

        let request = AgentCreateRequest::new("alice", "minimax");
        let result = service.create_agent(request).await.unwrap();

        assert_eq!(result.name, "alice");
        assert!(result.config_path.to_string_lossy().contains("agents"));
        assert!(result.config_path.to_string_lossy().contains("alice"));
    }

    #[tokio::test]
    async fn test_update_agent_model_description_and_system_prompt() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);
        seed_test_catalog(&service.resolver).await;

        let request = AgentCreateRequest::new("alice", "minimax");
        service.create_agent(request).await.unwrap();

        let update = AgentUpdateRequest {
            model: Some("mini-4".to_string()),
            description: Some("Agent for testing".to_string()),
            system_prompt: Some("You are a test assistant.".to_string()),
            ..Default::default()
        };
        service.update_agent("alice", None, update).await.unwrap();

        let info = service.get_agent("alice", None).await.unwrap().unwrap();
        // As of v3, the model is a soft hint, not an embedded provider
        // field. `agent update --model X` populates `preferred_model_id`.
        assert_eq!(info.config.preferred_model_id.as_deref(), Some("mini-4"));
        assert_eq!(
            info.config.description.as_deref(),
            Some("Agent for testing")
        );
        assert_eq!(
            info.system_prompt.as_deref(),
            Some("You are a test assistant.")
        );
        assert!(info
            .config
            .prompt
            .as_ref()
            .and_then(|p| p.system.as_ref())
            .and_then(|s| s.files.as_ref())
            .unwrap()
            .contains(&"SYSTEM.md".to_string()));
    }

    #[tokio::test]
    async fn test_update_agent_clears_system_prompt() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let data_dir = temp_dir.path().join("data");
        let cache_dir = temp_dir.path().join("cache");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let service = AgentService::new(resolver);
        seed_test_catalog(&service.resolver).await;

        let request = AgentCreateRequest::new("alice", "minimax");
        service.create_agent(request).await.unwrap();

        service
            .update_agent(
                "alice",
                None,
                AgentUpdateRequest {
                    system_prompt: Some("You are a test assistant.".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        service
            .update_agent(
                "alice",
                None,
                AgentUpdateRequest {
                    system_prompt: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let info = service.get_agent("alice", None).await.unwrap().unwrap();
        assert!(info.config.prompt.is_none());
        assert!(info.system_prompt.is_none());
    }
}
