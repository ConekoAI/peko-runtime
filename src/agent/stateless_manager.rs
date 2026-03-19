//! Stateless Agent Manager - Configuration and execution management
//!
//! This module provides the StatelessAgentManager which replaces the legacy
//! AgentManager in the stateless cold-start architecture. Instead of managing
//! a pool of running agents, it manages agent configurations and executes
//! agents statelessly on-demand.

use crate::agent::config_registry::{AgentConfigEntry, ConfigRegistry};
use crate::agent::lifecycle::LifecycleManager;
use crate::agent::stateless_service::{ExecutionRequest, ExecutionResult, StatelessAgentService};
use crate::image::registry::{ImageRegistry, RegistryConfig};
use crate::image::ImageRef;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
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
    /// Configuration registry
    config_registry: Arc<ConfigRegistry>,
    /// Stateless agent service
    agent_service: Arc<StatelessAgentService>,
    /// Lifecycle manager (tracks active executions only)
    lifecycle: Arc<LifecycleManager>,
    /// Image registry for resolving images
    image_registry: Arc<RwLock<ImageRegistry>>,
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
            .join("pekobot");

        std::fs::create_dir_all(&data_dir)?;

        // Create config registry
        let config_registry = Arc::new(
            ConfigRegistry::new(data_dir.join("configs"))
                .await
                .context("Failed to create config registry")?,
        );

        // Create image registry
        let registry_path = data_dir.join("registry");
        let registry_config = RegistryConfig::new(&registry_path);
        let image_registry = Arc::new(RwLock::new(ImageRegistry::new(registry_config)));

        // Create agent service
        let sessions_path = data_dir.join("sessions");
        let agent_service = Arc::new(
            StatelessAgentService::new(config_registry.clone(), sessions_path)
                .await
                .context("Failed to create agent service")?,
        );

        // Create lifecycle manager
        let lifecycle = Arc::new(LifecycleManager::new());

        let manager = Self {
            config_registry,
            agent_service,
            lifecycle,
            image_registry,
            events: events_tx,
            data_dir: data_dir.clone(),
        };

        info!(
            "StatelessAgentManager initialized at {}",
            data_dir.display()
        );

        Ok((manager, events_rx))
    }

    /// Create with custom data directory
    pub async fn with_data_dir(
        data_dir: impl Into<PathBuf>,
    ) -> Result<(Self, mpsc::Receiver<StatelessManagerEvent>)> {
        let data_dir = data_dir.into();
        let (events_tx, events_rx) = mpsc::channel(100);

        std::fs::create_dir_all(&data_dir)?;

        // Create config registry
        let config_registry = Arc::new(
            ConfigRegistry::new(data_dir.join("configs"))
                .await
                .context("Failed to create config registry")?,
        );

        // Create image registry
        let registry_path = data_dir.join("registry");
        let registry_config = RegistryConfig::new(&registry_path);
        let image_registry = Arc::new(RwLock::new(ImageRegistry::new(registry_config)));

        // Create agent service
        let sessions_path = data_dir.join("sessions");
        let agent_service = Arc::new(
            StatelessAgentService::new(config_registry.clone(), sessions_path)
                .await
                .context("Failed to create agent service")?,
        );

        // Create lifecycle manager
        let lifecycle = Arc::new(LifecycleManager::new());

        let manager = Self {
            config_registry,
            agent_service,
            lifecycle,
            image_registry,
            events: events_tx,
            data_dir: data_dir.clone(),
        };

        info!(
            "StatelessAgentManager initialized at {}",
            data_dir.display()
        );

        Ok((manager, events_rx))
    }

    /// Register an agent from an image
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
        let image_ref = ImageRef::parse(image)
            .with_context(|| format!("Invalid image reference: {}", image))?;

        let registry = self.image_registry.read().await;

        let entry = self
            .config_registry
            .register(name, &image_ref, &registry, team_id)
            .await?;

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
    pub async fn unregister(&self, name: &str) -> Result<bool> {
        let removed = self.config_registry.unregister(name).await?;

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
    pub async fn get(&self, name: &str) -> Option<AgentConfigEntry> {
        self.config_registry.get(name).await
    }

    /// List all registered agents
    pub async fn list(&self) -> Vec<AgentConfigEntry> {
        self.config_registry.list().await
    }

    /// List agents by team
    pub async fn list_by_team(&self, team_id: &str) -> Vec<AgentConfigEntry> {
        self.config_registry.list_by_team(team_id).await
    }

    /// Check if agent is registered
    pub async fn exists(&self, name: &str) -> bool {
        self.config_registry.exists(name).await
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

    /// Get config registry
    #[must_use]
    pub fn config_registry(&self) -> &Arc<ConfigRegistry> {
        &self.config_registry
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
        assert_eq!(manager.list().await.len(), 0);
        assert!(!manager.exists("test-agent").await);
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
