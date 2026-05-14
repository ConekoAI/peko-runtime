//! Stateless Agent Manager - Configuration and execution management
//!
//! This module provides the `StatelessAgentManager` which replaces the legacy
//! `AgentManager` in the stateless cold-start architecture. Instead of managing
//! a pool of running agents, it manages agent configurations and executes
//! agents statelessly on-demand.

use crate::agent::lifecycle::LifecycleManager;
use crate::agent::stateless_service::{ExecutionRequest, ExecutionResult, StatelessAgentService};
use crate::common::paths::PathResolver;
use crate::common::services::{AgentConfigEntry, ConfigAuthority, ConfigAuthorityImpl};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Events emitted by the stateless agent manager
#[derive(Debug, Clone)]
pub enum StatelessManagerEvent {
    /// Agent configuration registered
    AgentRegistered { name: String, image_ref: String },
    /// Agent configuration unregistered
    AgentUnregistered { name: String },
    /// Execution started
    ExecutionStarted {
        execution_id: String,
        agent_name: String,
        session_id: String,
    },
    /// Execution completed
    ExecutionCompleted {
        execution_id: String,
        agent_name: String,
        duration_ms: u64,
        success: bool,
    },
}

/// Stateless agent manager
///
/// Manages agent configurations and provides stateless execution.
/// No persistent agent instances - agents are cold-started per request.
pub struct StatelessAgentManager {
    /// Agent configuration service
    config_service: Arc<ConfigAuthorityImpl>,
    /// Stateless agent service
    agent_service: Arc<StatelessAgentService>,
    /// Lifecycle manager (tracks active executions only)
    lifecycle: Arc<LifecycleManager>,

    /// Event channel
    events: mpsc::Sender<StatelessManagerEvent>,
    /// Data directory
    data_dir: PathBuf,
}

impl StatelessAgentManager {
    /// Create a new stateless agent manager
    pub async fn new() -> Result<(Self, mpsc::Receiver<StatelessManagerEvent>)> {
        let (events_tx, events_rx) = mpsc::channel(100);

        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("peko");

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            dirs::home_dir().map_or_else(
                || PathBuf::from(".").join(".peko"),
                |d| d.join(".peko"),
            ),
            data_dir.clone(),
            dirs::cache_dir().map_or_else(|| data_dir.join("cache"), |d| d.join("peko")),
        );

        let manager = Self::build(data_dir, events_tx, path_resolver).await?;
        Ok((manager, events_rx))
    }

    /// Create with custom data directory
    pub async fn with_data_dir(
        data_dir: impl Into<PathBuf>,
    ) -> Result<(Self, mpsc::Receiver<StatelessManagerEvent>)> {
        let data_dir = data_dir.into();
        let (events_tx, events_rx) = mpsc::channel(100);

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            data_dir.join("config"),
            data_dir.join("data"),
            data_dir.join("cache"),
        );

        let manager = Self::build(data_dir, events_tx, path_resolver).await?;
        Ok((manager, events_rx))
    }

    async fn build(
        data_dir: PathBuf,
        events_tx: mpsc::Sender<StatelessManagerEvent>,
        path_resolver: PathResolver,
    ) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;

        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver.clone()));

        // Create agent service with team-aware paths
        let agent_service = Arc::new(
            StatelessAgentService::new(config_service.clone(), path_resolver)
                .await
                .context("Failed to create agent service")?,
        );

        // Create lifecycle manager
        let lifecycle = Arc::new(LifecycleManager::new());

        let manager = Self {
            config_service,
            agent_service,
            lifecycle,
            events: events_tx,
            data_dir: data_dir.clone(),
        };

        info!(
            "StatelessAgentManager initialized at {}",
            data_dir.display()
        );

        Ok(manager)
    }

    /// Register an agent from an image
    ///
    /// Note: Image-based registration requires `ConfigRegistry` for config extraction.
    /// For now, this is a placeholder that creates a default config.
    /// Use `ConfigAuthorityImpl` directly for non-image registration.
    ///
    /// # Arguments
    /// * `name` - Unique name for the agent
    /// * `image` - Image reference (e.g., "my-agent:v1")
    /// * `team_id` - Optional team assignment
    pub async fn register(
        &self,
        name: &str,
        image: &str,
        team_id: Option<String>,
    ) -> Result<AgentConfigEntry> {
        // TODO: Implement image-based config extraction using ImageRegistry
        // For now, create a default config
        let config = crate::types::agent::AgentConfig::default();
        let team = team_id.as_deref().unwrap_or("default");

        // Save config using ConfigAuthorityImpl
        self.config_service.save(name, team, &config).await?;

        // Load and return the entry
        let entry = self
            .config_service
            .get(name, Some(team))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Failed to retrieve saved agent config"))?;

        // Emit event
        let _ = self
            .events
            .send(StatelessManagerEvent::AgentRegistered {
                name: name.to_string(),
                image_ref: image.to_string(),
            })
            .await;

        Ok(entry)
    }

    /// Unregister an agent
    pub async fn unregister(&self, name: &str, team: &str) -> Result<bool> {
        let removed = self.config_service.delete(name, team).await?;

        if removed {
            let _ = self
                .events
                .send(StatelessManagerEvent::AgentUnregistered {
                    name: name.to_string(),
                })
                .await;
        }

        Ok(removed)
    }

    /// Get agent configuration
    pub async fn get(&self, name: &str, team: Option<&str>) -> Result<Option<AgentConfigEntry>> {
        Ok(self.config_service.get(name, team).await?)
    }

    /// List all registered agents
    pub async fn list(&self) -> Result<Vec<AgentConfigEntry>> {
        Ok(self.config_service.list_all().await?)
    }

    /// List agents by team
    pub async fn list_by_team(&self, team_id: &str) -> Result<Vec<AgentConfigEntry>> {
        Ok(self.config_service.list_in_team(team_id).await?)
    }

    /// Check if agent is registered
    pub async fn exists(&self, name: &str, team: Option<&str>) -> Result<bool> {
        Ok(self.config_service.exists(name, team).await?)
    }

    /// Execute agent statelessly
    ///
    /// # Arguments
    /// * `request` - Execution request
    pub async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult> {
        let agent_name = request.agent_name.clone();
        let session_id = request.session_id.clone();

        // Start execution tracking
        let execution_id = self
            .lifecycle
            .start_execution(&agent_name, &session_id, None)
            .await?;

        // Emit event
        let _ = self
            .events
            .send(StatelessManagerEvent::ExecutionStarted {
                execution_id: execution_id.clone(),
                agent_name: agent_name.clone(),
                session_id: session_id.clone(),
            })
            .await;

        // Execute
        let result = self.agent_service.execute(request).await;

        // Complete tracking
        let _ = self.lifecycle.complete_execution(&execution_id).await;

        // Emit completion event
        if let Ok(ref r) = result {
            let _ = self
                .events
                .send(StatelessManagerEvent::ExecutionCompleted {
                    execution_id,
                    agent_name,
                    duration_ms: r.duration_ms,
                    success: r.success,
                })
                .await;
        }

        result
    }

    /// Get current execution metrics
    pub async fn metrics(&self) -> crate::agent::stateless_service::ExecutionMetrics {
        self.agent_service.metrics().await
    }

    /// List active executions
    pub async fn active_executions(&self) -> Vec<crate::agent::lifecycle::ExecutionRecord> {
        self.lifecycle.active_executions().await
    }

    /// Get count of active executions
    pub async fn active_count(&self) -> usize {
        self.lifecycle.active_count().await
    }

    /// Check if agent is currently executing
    pub async fn is_executing(&self, agent_name: &str) -> bool {
        self.lifecycle.is_executing(agent_name).await
    }

    /// Get data directory
    #[must_use]
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Get config service
    #[must_use]
    pub fn config_service(&self) -> &Arc<ConfigAuthorityImpl> {
        &self.config_service
    }

    /// Get agent service
    #[must_use]
    pub fn agent_service(&self) -> &Arc<StatelessAgentService> {
        &self.agent_service
    }

    /// Shutdown the manager
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down StatelessAgentManager");

        // Cancel any active executions
        let active = self.lifecycle.active_executions().await;
        for exec in active {
            warn!("Cancelling active execution: {}", exec.id);
            let _ = self.lifecycle.cancel_execution(&exec.id, "shutdown").await;
        }

        info!("StatelessAgentManager shutdown complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_stateless_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let (manager, _events) =
            StatelessAgentManager::with_data_dir(temp_dir.path().to_path_buf())
                .await
                .unwrap();

        // Initially no agents
        assert_eq!(manager.list().await.unwrap().len(), 0);
        assert!(!manager.exists("test-agent", None).await.unwrap());
    }

    #[tokio::test]
    async fn test_execution_request_validation() {
        // Test that execution fails for non-existent agent
        let temp_dir = TempDir::new().unwrap();
        let (manager, _events) =
            StatelessAgentManager::with_data_dir(temp_dir.path().to_path_buf())
                .await
                .unwrap();

        // Try to execute non-existent agent
        let request = ExecutionRequest::new("non-existent", "test-session", "Hello");
        let result = manager.execute(request).await;

        assert!(result.is_err());
    }
}
