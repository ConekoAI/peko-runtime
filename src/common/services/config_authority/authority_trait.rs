//! `ConfigAuthority` trait definition
//!
//! Defines the async trait for agent configuration management.
//! This trait provides a clean interface for testing and dependency injection.

use super::entry::AgentConfigEntry;
use crate::types::agent::AgentConfig;
use async_trait::async_trait;
use std::path::PathBuf;

/// Error type for configuration operations
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Agent not found: {0}")]
    NotFound(String),
    #[error("Agent already exists: {0}")]
    AlreadyExists(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] toml::ser::Error),
    #[error("{0}")]
    Other(String),
}

/// Result type for configuration operations
pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// Trait for agent configuration authority
///
/// This is the single interface for all agent configuration operations.
/// It provides a clean abstraction that can be implemented by different
/// backends and easily mocked for testing.
///
/// Agents are standalone first-class citizens (ADR-031). They live at
/// `agents/{agent}/config.toml` and team membership is tracked separately.
#[async_trait]
pub trait ConfigAuthority: Send + Sync {
    /// Get agent configuration by name
    async fn get(&self, agent_name: &str) -> ConfigResult<Option<AgentConfigEntry>>;

    /// Save agent configuration
    ///
    /// Creates parent directories if they don't exist.
    /// Updates cache after saving.
    async fn save(
        &self,
        agent_name: &str,
        config: &AgentConfig,
    ) -> ConfigResult<PathBuf>;

    /// Check if an agent exists
    async fn exists(&self, agent_name: &str) -> ConfigResult<bool>;

    /// List all agents in a team
    ///
    /// Currently lists all agents (membership filtering will be added later).
    async fn list_in_team(&self, team: &str) -> ConfigResult<Vec<AgentConfigEntry>>;

    /// List all agents across all teams
    async fn list_all(&self) -> ConfigResult<Vec<AgentConfigEntry>>;

    /// Delete an agent configuration
    ///
    /// Removes from cache and deletes the config file.
    /// Note: This does NOT delete the agent directory or sessions.
    async fn delete(&self, agent_name: &str) -> ConfigResult<bool>;

    /// Clear the configuration cache
    async fn clear_cache(&self);

    /// Invalidate a specific entry in the cache
    async fn invalidate_cache(&self, agent_name: &str);

    /// Get the path resolver for this authority
    fn path_resolver(&self) -> &crate::common::paths::PathResolver;
}
