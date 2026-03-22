//! Agent Creation Service (DEPRECATED)
//!
//! DEPRECATED: This service is being merged into AgentService.
//! Use `AgentService::create_agent()` instead for all new code.
//!
//! Provides unified agent creation for both CLI and HTTP API.
//! Supports creating agents from images or from direct configuration.
#![deprecated(
    since = "0.9.0",
    note = "AgentCreationService is deprecated. Use AgentService::create_agent() instead."
)]

use crate::commands::agent_bootstrap::AgentBootstrap;
use crate::common::identifiers::{parse_agent_identifier_with_override, validate_agent_name};
use crate::common::paths::PathResolver;
use crate::common::services::auth_resolver::AuthResolver;
use crate::common::services::{AgentConfigBuilder, AgentConfigService};
use crate::team::TeamManager;
use crate::types::agent::AgentConfig;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// Source of agent creation (DEPRECATED)
#[deprecated(
    since = "0.9.0",
    note = "Use AgentCreateRequest in AgentService instead"
)]
#[derive(Debug, Clone)]
pub enum AgentSource {
    /// Create from an image reference
    Image { image_ref: String },
    /// Create from provider configuration
    Config {
        provider: String,
        model: Option<String>,
        env: HashMap<String, String>,
    },
}

/// Request to create an agent (DEPRECATED)
#[deprecated(
    since = "0.9.0",
    note = "Use AgentCreateRequest in types::agent instead"
)]
#[derive(Debug, Clone)]
pub struct AgentCreationRequest {
    /// Agent name
    pub name: String,
    /// Team (optional, defaults to "default")
    pub team: Option<String>,
    /// Source of agent configuration
    pub source: AgentSource,
    /// Whether to auto-create team if it doesn't exist
    pub auto_create_team: bool,
    /// Description (optional)
    pub description: Option<String>,
}

impl AgentCreationRequest {
    /// Create a new request with the given name
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            team: None,
            source: AgentSource::Config {
                provider: "openai".to_string(),
                model: None,
                env: HashMap::new(),
            },
            auto_create_team: true,
            description: None,
        }
    }

    /// Set the team
    pub fn with_team(mut self, team: impl Into<String>) -> Self {
        self.team = Some(team.into());
        self
    }

    /// Set the source to an image
    pub fn with_image(mut self, image_ref: impl Into<String>) -> Self {
        self.source = AgentSource::Image {
            image_ref: image_ref.into(),
        };
        self
    }

    /// Set the source to a provider config
    pub fn with_provider(
        mut self,
        provider: impl Into<String>,
        model: Option<String>,
        env: HashMap<String, String>,
    ) -> Self {
        self.source = AgentSource::Config {
            provider: provider.into(),
            model,
            env,
        };
        self
    }

    /// Set auto-create team
    pub fn with_auto_create_team(mut self, auto_create: bool) -> Self {
        self.auto_create_team = auto_create;
        self
    }

    /// Set description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Result of agent creation (DEPRECATED)
#[deprecated(
    since = "0.9.0",
    note = "Use AgentCreationResult in types::agent instead"
)]
#[derive(Debug, Clone)]
pub struct AgentCreationResult {
    /// Agent name
    pub name: String,
    /// Team name
    pub team: String,
    /// Config path
    pub config_path: PathBuf,
    /// Provider used
    pub provider: String,
    /// Whether the team was created
    pub team_created: bool,
}

/// Unified service for creating agents (DEPRECATED)
///
/// Use `AgentService` instead. This type will be removed in a future version.
#[deprecated(since = "0.9.0", note = "Use AgentService instead")]
pub struct AgentCreationService {
    config_service: Arc<AgentConfigService>,
    path_resolver: PathResolver,
    team_manager: Arc<TeamManager>,
}

impl AgentCreationService {
    /// Create a new agent creation service
    pub fn new(
        config_service: Arc<AgentConfigService>,
        path_resolver: PathResolver,
        team_manager: Arc<TeamManager>,
    ) -> Self {
        Self {
            config_service,
            path_resolver,
            team_manager,
        }
    }

    /// Create an agent
    ///
    /// This method handles:
    /// 1. Team validation/creation
    /// 2. Name validation
    /// 3. Config building (from image or provider)
    /// 4. Registration in ConfigRegistry
    /// 5. Workspace bootstrapping (optional)
    pub async fn create(
        &self,
        request: AgentCreationRequest,
        auth_resolver: &dyn AuthResolver,
    ) -> Result<AgentCreationResult> {
        // Parse identifier to extract team and agent name
        let (team, agent_name) =
            parse_agent_identifier_with_override(&request.name, request.team.as_deref())?;

        // Validate agent name
        if let Err(e) = validate_agent_name(agent_name) {
            return Err(map_validation_error(agent_name, e));
        }

        // Ensure team exists (auto-create if requested)
        let team_created = if request.auto_create_team {
            self.ensure_team_exists(team).await?
        } else {
            if !self.path_resolver.team_dir(team).exists() {
                anyhow::bail!("Team '{}' does not exist", team);
            }
            false
        };

        // Check if agent already exists
        if self.config_service.exists(agent_name, Some(team)).await? {
            anyhow::bail!(
                "Agent '{}' already exists in team '{}'. Use update() to modify.",
                agent_name,
                team
            );
        }

        // Get workspace path
        let workspace_dir = self.path_resolver.agent_workspace(agent_name, Some(team));

        // Build configuration based on source
        let (mut config, provider) = match request.source {
            AgentSource::Image { image_ref } => {
                self.create_from_image(agent_name, team, &image_ref).await?
            }
            AgentSource::Config {
                provider,
                model,
                env,
            } => {
                self.create_from_config(
                    agent_name,
                    team,
                    &provider,
                    model,
                    env,
                    request.description,
                    auth_resolver,
                )
                .await?
            }
        };

        // Set workspace in config
        config.workspace = Some(workspace_dir.clone());

        // Save agent configuration to TOML
        self.config_service
            .save(agent_name, team, &config)
            .await
            .with_context(|| format!("Failed to save agent '{}' config", agent_name))?;

        // Create agent directory structure (workspace, sessions, etc.)
        let config_path = self
            .setup_agent_directory(agent_name, team, &workspace_dir)
            .await?;

        info!(
            "Created agent '{}' in team '{}' (provider: {}, team_created: {})",
            agent_name, team, provider, team_created
        );

        Ok(AgentCreationResult {
            name: agent_name.to_string(),
            team: team.to_string(),
            config_path,
            provider,
            team_created,
        })
    }

    /// Ensure a team exists, creating it if necessary
    async fn ensure_team_exists(&self, team: &str) -> Result<bool> {
        if self.path_resolver.team_dir(team).exists() {
            return Ok(false);
        }

        // Validate team name
        use crate::common::identifiers::validate_team_name;
        if let Err(e) = validate_team_name(team) {
            return Err(map_team_validation_error(team, e));
        }

        // Create team directory
        let team_dir = self.path_resolver.team_dir(team);
        tokio::fs::create_dir_all(&team_dir).await?;

        // Create team metadata
        let metadata = crate::common::types::team::TeamMetadata {
            name: team.to_string(),
            description: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let metadata_path = team_dir.join("team.toml");
        let metadata_content = toml::to_string_pretty(&metadata)?;
        tokio::fs::write(&metadata_path, metadata_content).await?;

        info!("Created team '{}'", team);
        Ok(true)
    }

    /// Create agent configuration from an image
    async fn create_from_image(
        &self,
        _name: &str,
        _team: &str,
        image_ref: &str,
    ) -> Result<(AgentConfig, String)> {
        use crate::image::registry::{ImageRegistry, RegistryConfig};
        use crate::image::ImageRef;

        // Parse image reference
        let image_ref = ImageRef::parse(image_ref)
            .with_context(|| format!("Invalid image reference: {}", image_ref))?;

        // Resolve image in registry
        let registry_path = self.path_resolver.data_dir().join("registry");
        let config = RegistryConfig::new(&registry_path);
        let registry = ImageRegistry::new(config);

        // For now, we need to register via the image path
        // This will load the config from the image
        let manifest = registry
            .resolve(&image_ref)
            .await
            .with_context(|| format!("Failed to resolve image: {}", image_ref.display()))?
            .ok_or_else(|| anyhow::anyhow!("Image not found: {}", image_ref.display()))?;

        // Load config from manifest
        let config = self
            .load_config_from_manifest(&manifest, &registry)
            .await
            .with_context(|| "Failed to load agent config from image")?;

        // Determine provider from config
        let provider = format!("{:?}", config.provider.provider_type).to_lowercase();

        Ok((config, provider))
    }

    /// Create agent configuration from provider/config
    async fn create_from_config(
        &self,
        name: &str,
        team: &str,
        provider: &str,
        model: Option<String>,
        env: HashMap<String, String>,
        description: Option<String>,
        auth_resolver: &dyn AuthResolver,
    ) -> Result<(AgentConfig, String)> {
        // Build config using the builder
        let mut builder = AgentConfigBuilder::new(name)
            .with_provider(provider)
            .with_team(team)
            .with_envs(env);

        if let Some(model) = model {
            builder = builder.with_model(model);
        }

        if let Some(desc) = description {
            builder = builder.with_description(desc);
        }

        let config = builder.build(auth_resolver).await?;

        Ok((config, provider.to_string()))
    }

    /// Set up agent directory structure
    async fn setup_agent_directory(
        &self,
        agent_name: &str,
        team: &str,
        workspace_dir: &PathBuf,
    ) -> Result<PathBuf> {
        let agent_dir = self.path_resolver.agents_dir(Some(team)).join(agent_name);

        // Create directories
        tokio::fs::create_dir_all(&agent_dir).await?;
        tokio::fs::create_dir_all(agent_dir.join("sessions")).await?;
        tokio::fs::create_dir_all(workspace_dir).await?;

        // Create .gitignore
        let gitignore_content = r#"# Pekobot agent - gitignore
sessions/
workspace/
memories/
cron.json
*.log
"#;
        tokio::fs::write(agent_dir.join(".gitignore"), gitignore_content).await?;

        // Create bootstrap files using AgentBootstrap
        let bootstrap = AgentBootstrap::new(agent_name, workspace_dir.clone());
        tokio::task::block_in_place(|| bootstrap.run())?;

        Ok(agent_dir.join("config.toml"))
    }

    /// Load config from image manifest (helper)
    async fn load_config_from_manifest(
        &self,
        manifest: &crate::image::manifest::ImageManifest,
        registry: &crate::image::registry::ImageRegistry,
    ) -> Result<AgentConfig> {
        use crate::image::manifest::ImageDigest;
        use crate::image::manifest::LayerType;

        // Find config layer
        let config_layer = manifest
            .layers
            .iter()
            .find(|l| l.layer_type == LayerType::Config)
            .ok_or_else(|| anyhow::anyhow!("No config layer found in image"))?;

        // Load config layer data
        let digest = ImageDigest::new(&config_layer.digest)?;
        let config_data = registry
            .get_layer(&digest)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Config layer not found in registry"))?;

        // Parse config.toml from layer (it's a tar.gz, extract config.toml)
        self.extract_config_from_layer(&config_data).await
    }

    /// Extract config.toml from layer tar.gz data (helper)
    async fn extract_config_from_layer(&self, data: &[u8]) -> Result<AgentConfig> {
        use flate2::read::GzDecoder;
        use std::io::Read;

        // Decompress gzip
        let mut decoder = GzDecoder::new(data);
        let mut tar_data = Vec::new();
        decoder.read_to_end(&mut tar_data)?;

        // Parse tar archive
        let mut archive = tar::Archive::new(&tar_data[..]);

        // Look for config.toml
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;

            if path.file_name().map_or(false, |n| n == "config.toml") {
                let mut content = String::new();
                entry.read_to_string(&mut content)?;

                // Parse TOML config
                let config: AgentConfig = toml::from_str(&content)
                    .with_context(|| "Failed to parse config.toml from image")?;

                return Ok(config);
            }
        }

        Err(anyhow::anyhow!("config.toml not found in image layer"))
    }
}

/// Map agent validation error to anyhow error
fn map_validation_error(
    name: &str,
    e: crate::common::identifiers::ValidationError,
) -> anyhow::Error {
    use crate::common::identifiers::ValidationError;
    match e {
        ValidationError::Empty => anyhow::anyhow!("Agent name cannot be empty"),
        ValidationError::TooLong(max) => anyhow::anyhow!(
            "Agent name '{}' exceeds maximum length of {} characters",
            name,
            max
        ),
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

/// Map team validation error to anyhow error
fn map_team_validation_error(
    team: &str,
    e: crate::common::identifiers::ValidationError,
) -> anyhow::Error {
    use crate::common::identifiers::ValidationError;
    match e {
        ValidationError::Empty => anyhow::anyhow!("Team name cannot be empty"),
        ValidationError::TooLong(max) => anyhow::anyhow!(
            "Team name '{}' exceeds maximum length of {} characters",
            team,
            max
        ),
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
    use crate::common::services::DirectAuthResolver;

    #[test]
    fn test_agent_creation_request_builder() {
        let request = AgentCreationRequest::new("my-agent")
            .with_team("my-team")
            .with_provider("kimi", Some("k2p5".to_string()), HashMap::new())
            .with_auto_create_team(true);

        assert_eq!(request.name, "my-agent");
        assert_eq!(request.team, Some("my-team".to_string()));
        assert!(request.auto_create_team);

        match request.source {
            AgentSource::Config { provider, .. } => {
                assert_eq!(provider, "kimi");
            }
            _ => panic!("Expected Config source"),
        }
    }

    #[test]
    fn test_agent_creation_request_with_image() {
        let request =
            AgentCreationRequest::new("my-agent").with_image("pekohub.com/agents/test:v1.0");

        match request.source {
            AgentSource::Image { image_ref } => {
                assert_eq!(image_ref, "pekohub.com/agents/test:v1.0");
            }
            _ => panic!("Expected Image source"),
        }
    }
}
