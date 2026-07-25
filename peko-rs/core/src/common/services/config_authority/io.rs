//! Configuration I/O
//!
//! Provides `ConfigIo`: TOML file read/write operations with atomic writes.

use crate::agents::agent_config::AgentConfig;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::fs;

/// Configuration I/O operations for TOML files
///
/// Handles reading and writing agent configurations to TOML files
/// with atomic write operations.
#[derive(Debug, Clone)]
pub struct ConfigIo;

impl ConfigIo {
    /// Create a new `ConfigIo` instance
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Load agent configuration from a TOML file
    pub async fn load_toml(&self, path: &PathBuf) -> Result<AgentConfig> {
        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: AgentConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML config: {}", path.display()))?;

        Ok(config)
    }

    /// Save agent configuration to a TOML file atomically
    ///
    /// Writes to a temp file first, then renames for atomicity.
    pub async fn save_toml(&self, path: &PathBuf, config: &AgentConfig) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("Failed to create agent directory: {}", parent.display())
            })?;
        }

        // Serialize to TOML
        let toml_content =
            toml::to_string_pretty(config).with_context(|| "Failed to serialize config to TOML")?;

        // Write atomically using temp file
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &toml_content)
            .await
            .with_context(|| {
                format!("Failed to write temp config file: {}", temp_path.display())
            })?;

        fs::rename(&temp_path, path)
            .await
            .with_context(|| format!("Failed to rename config file to: {}", path.display()))?;

        Ok(())
    }

    /// Check if a config file exists
    pub async fn exists(&self, path: &PathBuf) -> bool {
        path.exists()
    }

    /// Delete a config file
    pub async fn delete(&self, path: &PathBuf) -> Result<bool> {
        if !path.exists() {
            return Ok(false);
        }

        fs::remove_file(path)
            .await
            .with_context(|| format!("Failed to delete config file: {}", path.display()))?;

        Ok(true)
    }
}

impl Default for ConfigIo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_load_missing_config() {
        let io = ConfigIo::new();
        let path = PathBuf::from("/nonexistent/path/config.toml");
        assert!(io.load_toml(&path).await.is_err());
    }
}
