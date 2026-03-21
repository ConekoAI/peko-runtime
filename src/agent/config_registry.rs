//! Configuration Registry - Read-only agent configuration store
//!
//! This module provides the ConfigRegistry which replaces the InstanceStore
//! in the stateless cold-start architecture. Instead of tracking running
//! agent instances, it maintains a registry of agent configurations that
//! can be used to cold-start agents on demand.
//!
//! ## Architecture
//!
//! - Configurations are stored as JSON files on disk
//! - In-memory cache for fast lookups
//! - Content-addressable by agent name
//! - Immutable once registered (versioned updates)

use crate::image::manifest::ImageManifest;
use crate::image::registry::ImageRegistry;
use crate::image::ImageRef;
use crate::types::agent::AgentConfig;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Source of agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigSource {
    /// Configuration loaded from an image
    Image {
        /// Image reference
        image_ref: String,
        /// Image digest
        image_digest: String,
    },
    /// Configuration created directly (e.g., via CLI or API)
    Direct {
        /// Reason/source of creation
        reason: String,
    },
}

impl ConfigSource {
    /// Get image reference (if from image)
    pub fn image_ref(&self) -> Option<&str> {
        match self {
            ConfigSource::Image { image_ref, .. } => Some(image_ref),
            ConfigSource::Direct { .. } => None,
        }
    }

    /// Get image digest (if from image)
    pub fn image_digest(&self) -> Option<&str> {
        match self {
            ConfigSource::Image { image_digest, .. } => Some(image_digest),
            ConfigSource::Direct { .. } => None,
        }
    }
}

impl Default for ConfigSource {
    fn default() -> Self {
        ConfigSource::Direct {
            reason: "default".to_string(),
        }
    }
}

/// Agent configuration entry in the registry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigEntry {
    /// Agent name (unique identifier)
    pub name: String,
    /// Agent configuration
    pub config: AgentConfig,
    /// Source of configuration
    #[serde(flatten)]
    pub source: ConfigSource,
    /// Team assignment
    pub team_id: Option<String>,
    /// Registration timestamp
    pub registered_at: DateTime<Utc>,
    /// Last updated timestamp
    pub updated_at: DateTime<Utc>,
}

impl AgentConfigEntry {
    /// Get capabilities as a list of strings
    pub fn capabilities(&self) -> Vec<String> {
        self.config
            .capabilities
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    /// Check if agent has a specific capability
    pub fn has_capability(&self, name: &str) -> bool {
        self.config.capabilities.iter().any(|c| c.name == name)
    }

    /// Get image reference (backward compatibility)
    pub fn image_ref(&self) -> &str {
        self.source.image_ref().unwrap_or("direct")
    }

    /// Get image digest (backward compatibility)
    pub fn image_digest(&self) -> &str {
        self.source.image_digest().unwrap_or("direct")
    }
}

/// Configuration registry for stateless agent execution
pub struct ConfigRegistry {
    /// In-memory cache of configurations
    configs: RwLock<HashMap<String, AgentConfigEntry>>,
    /// Storage directory
    data_dir: PathBuf,
}

impl ConfigRegistry {
    /// Create a new configuration registry
    pub async fn new(data_dir: PathBuf) -> Result<Self> {
        // Ensure directory exists
        tokio::fs::create_dir_all(&data_dir)
            .await
            .with_context(|| {
                format!(
                    "Failed to create config registry dir: {}",
                    data_dir.display()
                )
            })?;

        let registry = Self {
            configs: RwLock::new(HashMap::new()),
            data_dir,
        };

        // Load existing configurations
        registry.load_all().await?;

        info!(
            "ConfigRegistry initialized at {} with {} entries",
            registry.data_dir.display(),
            registry.configs.read().await.len()
        );

        Ok(registry)
    }

    /// Register a new agent configuration from an image
    ///
    /// # Arguments
    /// * `name` - Unique name for the agent
    /// * `image_ref` - Image reference to load configuration from
    /// * `image_registry` - Image registry to resolve the reference
    /// * `team_id` - Optional team assignment
    pub async fn register(
        &self,
        name: &str,
        image_ref: &ImageRef,
        image_registry: &ImageRegistry,
        team_id: Option<String>,
    ) -> Result<AgentConfigEntry> {
        // Check if name already exists
        {
            let configs = self.configs.read().await;
            if configs.contains_key(name) {
                return Err(anyhow::anyhow!(
                    "Agent '{}' already registered. Use update() to modify.",
                    name
                ));
            }
        }

        // Resolve image reference to manifest
        let manifest = image_registry
            .resolve(image_ref)
            .await
            .with_context(|| format!("Failed to resolve image: {}", image_ref.display()))?
            .ok_or_else(|| anyhow::anyhow!("Image not found: {}", image_ref.display()))?;

        // Load agent configuration from manifest
        let config = self
            .load_config_from_manifest(&manifest, image_registry)
            .await
            .with_context(|| "Failed to load agent config from image")?;

        let entry = AgentConfigEntry {
            name: name.to_string(),
            config,
            source: ConfigSource::Image {
                image_ref: image_ref.display(),
                image_digest: manifest.digest.clone(),
            },
            team_id,
            registered_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // Save to disk
        self.save(&entry).await?;

        // Add to in-memory cache
        {
            let mut configs = self.configs.write().await;
            configs.insert(name.to_string(), entry.clone());
        }

        info!(
            "Registered agent '{}' from image {} (digest: {})",
            name,
            entry.image_ref(),
            entry.image_digest()
        );

        Ok(entry)
    }

    /// Register a new agent configuration directly (without an image)
    ///
    /// # Arguments
    /// * `name` - Unique name for the agent
    /// * `config` - Agent configuration
    /// * `team_id` - Optional team assignment
    /// * `reason` - Reason for direct registration (e.g., "created_via_api")
    pub async fn register_direct(
        &self,
        name: &str,
        config: AgentConfig,
        team_id: Option<String>,
        reason: impl Into<String>,
    ) -> Result<AgentConfigEntry> {
        // Check if name already exists
        {
            let configs = self.configs.read().await;
            if configs.contains_key(name) {
                return Err(anyhow::anyhow!(
                    "Agent '{}' already registered. Use update() to modify.",
                    name
                ));
            }
        }

        let entry = AgentConfigEntry {
            name: name.to_string(),
            config,
            source: ConfigSource::Direct {
                reason: reason.into(),
            },
            team_id,
            registered_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // Save to disk
        self.save(&entry).await?;

        // Add to in-memory cache
        {
            let mut configs = self.configs.write().await;
            configs.insert(name.to_string(), entry.clone());
        }

        info!(
            "Registered agent '{}' directly (reason: {})",
            name,
            entry.source.image_ref().unwrap_or("direct")
        );

        Ok(entry)
    }

    /// Update an existing agent configuration
    pub async fn update(
        &self,
        name: &str,
        image_ref: Option<&ImageRef>,
        image_registry: Option<&ImageRegistry>,
        team_id: Option<String>,
    ) -> Result<AgentConfigEntry> {
        // Check if exists
        {
            let configs = self.configs.read().await;
            if !configs.contains_key(name) {
                return Err(anyhow::anyhow!(
                    "Agent '{}' not found. Use register() to create.",
                    name
                ));
            }
        }

        // Load new config if image changed
        let (config, source) = if let (Some(img_ref), Some(img_reg)) = (image_ref, image_registry) {
            let manifest = img_reg
                .resolve(img_ref)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Image not found: {}", img_ref.display()))?;

            let config = self.load_config_from_manifest(&manifest, img_reg).await?;
            let source = ConfigSource::Image {
                image_ref: img_ref.display(),
                image_digest: manifest.digest.clone(),
            };
            (config, source)
        } else {
            // Keep existing source/config, just update team
            let configs = self.configs.read().await;
            let existing = configs.get(name).unwrap();
            (existing.config.clone(), existing.source.clone())
        };

        let registered_at = {
            let configs = self.configs.read().await;
            configs
                .get(name)
                .map(|e| e.registered_at)
                .unwrap_or_else(Utc::now)
        };

        let entry = AgentConfigEntry {
            name: name.to_string(),
            config,
            source,
            team_id: team_id.or_else(|| {
                let configs = self.configs.blocking_read();
                configs.get(name).and_then(|e| e.team_id.clone())
            }),
            registered_at,
            updated_at: Utc::now(),
        };

        // Save to disk
        self.save(&entry).await?;

        // Update in-memory cache
        {
            let mut configs = self.configs.write().await;
            configs.insert(name.to_string(), entry.clone());
        }

        info!("Updated agent '{}' configuration", name);

        Ok(entry)
    }

    /// Get agent configuration by name
    pub async fn get(&self, name: &str) -> Option<AgentConfigEntry> {
        let configs = self.configs.read().await;
        configs.get(name).cloned()
    }

    /// List all registered agent configurations
    pub async fn list(&self) -> Vec<AgentConfigEntry> {
        let configs = self.configs.read().await;
        configs.values().cloned().collect()
    }

    /// List configurations by team
    pub async fn list_by_team(&self, team_id: &str) -> Vec<AgentConfigEntry> {
        let configs = self.configs.read().await;
        configs
            .values()
            .filter(|e| e.team_id.as_ref() == Some(&team_id.to_string()))
            .cloned()
            .collect()
    }

    /// Unregister an agent configuration
    pub async fn unregister(&self, name: &str) -> Result<bool> {
        // Remove from disk
        let path = self.config_path(name);
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .with_context(|| format!("Failed to delete config file: {}", path.display()))?;
        }

        // Remove from in-memory cache
        let removed = {
            let mut configs = self.configs.write().await;
            configs.remove(name).is_some()
        };

        if removed {
            info!("Unregistered agent '{}'", name);
        }

        Ok(removed)
    }

    /// Check if an agent is registered
    pub async fn exists(&self, name: &str) -> bool {
        let configs = self.configs.read().await;
        configs.contains_key(name)
    }

    /// Get count of registered agents
    pub async fn count(&self) -> usize {
        let configs = self.configs.read().await;
        configs.len()
    }

    /// Save configuration entry to disk
    async fn save(&self, entry: &AgentConfigEntry) -> Result<()> {
        let path = self.config_path(&entry.name);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let json =
            serde_json::to_string_pretty(entry).with_context(|| "Failed to serialize config")?;

        tokio::fs::write(&path, json)
            .await
            .with_context(|| format!("Failed to write config file: {}", path.display()))?;

        debug!("Saved config for '{}' to {}", entry.name, path.display());

        Ok(())
    }

    /// Load all configurations from disk
    async fn load_all(&self) -> Result<()> {
        let mut configs = self.configs.write().await;
        configs.clear();

        let mut entries = match tokio::fs::read_dir(&self.data_dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            // Only process .json files
            if path.extension().map_or(false, |e| e == "json") {
                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => match serde_json::from_str::<AgentConfigEntry>(&content) {
                        Ok(config) => {
                            debug!(
                                "Loaded config for '{}' from {}",
                                config.name,
                                path.display()
                            );
                            configs.insert(config.name.clone(), config);
                        }
                        Err(e) => {
                            warn!("Failed to parse config file {}: {}", path.display(), e);
                        }
                    },
                    Err(e) => {
                        warn!("Failed to read config file {}: {}", path.display(), e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Get the config file path for an agent
    fn config_path(&self, name: &str) -> PathBuf {
        // Sanitize name for filesystem
        let safe_name = name.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        self.data_dir.join(format!("{}.json", safe_name))
    }

    /// Load agent configuration from image manifest
    async fn load_config_from_manifest(
        &self,
        manifest: &ImageManifest,
        registry: &ImageRegistry,
    ) -> Result<AgentConfig> {
        // Find config layer
        let config_layer = manifest
            .layers
            .iter()
            .find(|l| l.layer_type == crate::image::manifest::LayerType::Config)
            .ok_or_else(|| anyhow::anyhow!("No config layer found in image"))?;

        // Load config layer data
        let digest = crate::image::manifest::ImageDigest::new(&config_layer.digest)?;
        let config_data = registry
            .get_layer(&digest)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Config layer not found in registry"))?;

        // Parse config.toml from layer (it's a tar.gz, extract config.toml)
        let config = self.extract_config_from_layer(&config_data).await?;

        Ok(config)
    }

    /// Extract config.toml from layer tar.gz data
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_config_registry_basic() {
        let temp_dir = TempDir::new().unwrap();
        let registry = ConfigRegistry::new(temp_dir.path().to_path_buf())
            .await
            .expect("Failed to create registry");

        // Initially empty
        assert_eq!(registry.count().await, 0);
        assert!(!registry.exists("test-agent").await);
    }

    #[tokio::test]
    async fn test_config_registry_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();

        // Create registry and add entry
        {
            let registry = ConfigRegistry::new(data_dir.clone()).await.unwrap();

            // Create a minimal test config entry directly
            let entry = AgentConfigEntry {
                name: "test-agent".to_string(),
                config: AgentConfig::default(),
                source: ConfigSource::Image {
                    image_ref: "test:latest".to_string(),
                    image_digest: "sha256:abc123".to_string(),
                },
                team_id: None,
                registered_at: Utc::now(),
                updated_at: Utc::now(),
            };

            registry.save(&entry).await.unwrap();
        }

        // Create new registry instance and verify persistence
        {
            let registry = ConfigRegistry::new(data_dir).await.unwrap();
            assert!(registry.exists("test-agent").await);
            assert_eq!(registry.count().await, 1);

            let entry = registry.get("test-agent").await.unwrap();
            assert_eq!(entry.name, "test-agent");
            assert_eq!(entry.image_ref(), "test:latest");
        }
    }
}
