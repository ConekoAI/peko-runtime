//! Team Runtime Module
//!
//! Implements Milestone 7: Team Runtime and Event Bus
//! - Multi-agent teams with shared services
//! - A2A communication via event bus
//! - Horizontal scaling support

use crate::daemon::state::AppState;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub mod bus;
pub mod config;
pub mod shared;

use bus::EventBus;
use config::{AgentDefinition, TeamConfig};
use shared::SharedServicesFabric;

/// Unique team ID
pub type TeamId = String;

/// Team status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamStatus {
    /// Team is being created/started
    Starting,
    /// Team is running and ready
    Running,
    /// Team is being stopped
    Stopping,
    /// Team is stopped
    Stopped,
    /// Team encountered an error
    Error,
}

impl std::fmt::Display for TeamStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamStatus::Starting => write!(f, "starting"),
            TeamStatus::Running => write!(f, "running"),
            TeamStatus::Stopping => write!(f, "stopping"),
            TeamStatus::Stopped => write!(f, "stopped"),
            TeamStatus::Error => write!(f, "error"),
        }
    }
}

/// Team instance - a running team
#[derive(Debug, Clone)]
pub struct Team {
    /// Unique team ID
    pub id: TeamId,
    /// Team name
    pub name: String,
    /// Current status
    pub status: TeamStatus,
    /// Workspace directory
    pub workspace_path: PathBuf,
    /// Team configuration
    pub config: TeamConfig,
    /// Instance IDs by agent name
    pub agent_instances: HashMap<String, Vec<String>>,
    /// Error message (if status is Error)
    pub error: Option<String>,
    /// Creation timestamp
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Team {
    /// Generate a unique team ID
    fn generate_id() -> TeamId {
        format!("team_{}", Uuid::new_v4().simple())
    }

    /// Get the default workspace path for a team
    #[must_use]
    pub fn default_workspace_path(name: &str) -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("peko")
            .join("teams")
            .join(name)
    }

    /// Get the shared files path
    #[must_use]
    pub fn shared_files_path(&self) -> PathBuf {
        let relative_path = self.config.shared_files_path();
        if std::path::Path::new(&relative_path).is_absolute() {
            PathBuf::from(relative_path)
        } else {
            self.workspace_path.join(relative_path)
        }
    }

    /// Count total instances in the team
    #[must_use]
    pub fn total_instances(&self) -> usize {
        self.agent_instances.values().map(std::vec::Vec::len).sum()
    }

    /// Get all instance IDs
    #[must_use]
    pub fn all_instance_ids(&self) -> Vec<String> {
        self.agent_instances
            .values()
            .flat_map(|v| v.iter().cloned())
            .collect()
    }
}

/// Team manager - manages all teams
pub struct TeamManager {
    /// Teams by ID
    teams: Arc<RwLock<HashMap<TeamId, Team>>>,
    /// Event buses by team ID
    buses: Arc<RwLock<HashMap<TeamId, Arc<dyn EventBus>>>>,
    /// Shared services by team ID
    shared_services: Arc<RwLock<HashMap<TeamId, SharedServicesFabric>>>,
}

impl TeamManager {
    /// Create a new team manager
    #[must_use]
    pub fn new() -> Self {
        Self {
            teams: Arc::new(RwLock::new(HashMap::new())),
            buses: Arc::new(RwLock::new(HashMap::new())),
            shared_services: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create with custom data directory
    #[must_use]
    pub fn with_data_dir(_data_dir: PathBuf) -> Self {
        Self::new()
    }

    /// Deploy a team from configuration
    pub async fn deploy(&self, config: TeamConfig, app_state: Arc<AppState>) -> Result<Team> {
        let team_id = Team::generate_id();
        let team_name = config.identity.name.clone();

        tracing::info!("Deploying team {} ({})", team_name, team_id);

        // Create workspace directory
        let workspace_path = Team::default_workspace_path(&team_name);
        std::fs::create_dir_all(&workspace_path)?;

        // Create shared directories
        let shared_path = workspace_path.join("shared");
        std::fs::create_dir_all(&shared_path)?;

        if config.shared_files_enabled() {
            let files_path = config.shared_files_path();
            let files_full_path = if std::path::Path::new(&files_path).is_absolute() {
                PathBuf::from(&files_path)
            } else {
                workspace_path.join(&files_path)
            };
            std::fs::create_dir_all(&files_full_path)?;
        }

        // Create event bus
        let bus_config = config.bus_config();
        let bus = bus::create_bus(bus_config.backend, bus_config.url).await?;

        // Initialize shared services fabric
        let shared_fabric = SharedServicesFabric::new(&config, &workspace_path).await?;

        // Create team instance
        let mut team = Team {
            id: team_id.clone(),
            name: team_name.clone(),
            status: TeamStatus::Starting,
            workspace_path: workspace_path.clone(),
            config: config.clone(),
            agent_instances: HashMap::new(),
            error: None,
            created_at: chrono::Utc::now(),
        };

        // Deploy agents
        let mut agent_instances: HashMap<String, Vec<String>> = HashMap::new();

        for agent_def in &config.agents {
            let instances = self
                .deploy_agent_instances(&team_id, agent_def, &workspace_path, &app_state)
                .await?;
            agent_instances.insert(agent_def.name.clone(), instances);
        }

        team.agent_instances = agent_instances;
        team.status = TeamStatus::Running;

        // Store team, bus, and shared services
        {
            let mut teams = self.teams.write().await;
            teams.insert(team_id.clone(), team.clone());
        }
        {
            let mut buses = self.buses.write().await;
            buses.insert(team_id.clone(), bus);
        }
        {
            let mut shared = self.shared_services.write().await;
            shared.insert(team_id.clone(), shared_fabric);
        }

        tracing::info!(
            "Team {} deployed successfully with {} instances",
            team_id,
            team.total_instances()
        );

        Ok(team)
    }

    /// Deploy instances for a single agent type
    async fn deploy_agent_instances(
        &self,
        team_id: &TeamId,
        agent_def: &AgentDefinition,
        team_workspace: &PathBuf,
        _app_state: &Arc<AppState>,
    ) -> Result<Vec<String>> {
        let mut instances = Vec::new();

        for i in 1..=agent_def.instances {
            let instance_name = format!("{}-{}", agent_def.name, i);
            let agent_workspace = team_workspace.join("agents").join(&instance_name);
            std::fs::create_dir_all(&agent_workspace)?;

            // TODO: Create instance from image
            // This requires integration with the instance creation API
            // For now, we'll return a placeholder ID
            let instance_id = format!("inst_{}", Uuid::new_v4().simple());
            instances.push(instance_id);

            tracing::debug!(
                "Deployed agent instance {} for team {}",
                instance_name,
                team_id
            );
        }

        Ok(instances)
    }

    /// Get a team by ID
    pub async fn get_team(&self, team_id: &TeamId) -> Option<Team> {
        let teams = self.teams.read().await;
        teams.get(team_id).cloned()
    }

    /// List all teams
    pub async fn list_teams(&self) -> Vec<Team> {
        let teams = self.teams.read().await;
        teams.values().cloned().collect()
    }

    /// Get team by name
    pub async fn get_team_by_name(&self, name: &str) -> Option<Team> {
        let teams = self.teams.read().await;
        teams.values().find(|t| t.name == name).cloned()
    }

    /// Stop and remove a team
    pub async fn remove_team(&self, team_id: &TeamId) -> Result<()> {
        tracing::info!("Stopping team {}", team_id);

        // Get team info
        let team = {
            let mut teams = self.teams.write().await;
            match teams.remove(team_id) {
                Some(t) => t,
                None => anyhow::bail!("Team {team_id} not found"),
            }
        };

        // Stop all instances
        for (agent_name, instance_ids) in &team.agent_instances {
            for instance_id in instance_ids {
                tracing::debug!(
                    "Stopping instance {} (agent {} in team {})",
                    instance_id,
                    agent_name,
                    team_id
                );
                // TODO: Call instance stop API
            }
        }

        // Shutdown event bus
        {
            let mut buses = self.buses.write().await;
            if let Some(bus) = buses.remove(team_id) {
                bus.shutdown().await?;
            }
        }

        // Shutdown shared services
        {
            let mut shared = self.shared_services.write().await;
            if let Some(fabric) = shared.remove(team_id) {
                fabric.shutdown().await?;
            }
        }

        tracing::info!("Team {} stopped and removed", team_id);
        Ok(())
    }

    /// Scale an agent type within a team
    pub async fn scale_agent(
        &self,
        team_id: &TeamId,
        agent_name: &str,
        new_count: u32,
        _app_state: Arc<AppState>,
    ) -> Result<ScaleResult> {
        let mut teams = self.teams.write().await;
        let team = teams
            .get_mut(team_id)
            .ok_or_else(|| anyhow::anyhow!("Team {team_id} not found"))?;

        if team.status != TeamStatus::Running {
            anyhow::bail!("Team {team_id} is not running");
        }

        // Find agent definition
        let _agent_def = team
            .config
            .agents
            .iter()
            .find(|a| a.name == agent_name)
            .ok_or_else(|| anyhow::anyhow!("Agent {agent_name} not found in team {team_id}"))?
            .clone();

        let current_instances = team
            .agent_instances
            .get(agent_name)
            .map_or(0, |v| v.len() as u32);

        if new_count == current_instances {
            return Ok(ScaleResult {
                team_id: team_id.clone(),
                agent_name: agent_name.to_string(),
                previous_count: current_instances,
                new_count,
                added_instance_ids: vec![],
                removed_instance_ids: vec![],
            });
        }

        let mut added = Vec::new();
        let mut removed = Vec::new();

        if new_count > current_instances {
            // Scale up - add new instances
            let additional = new_count - current_instances;
            for i in (current_instances + 1)..=(current_instances + additional) {
                let instance_name = format!("{agent_name}-{i}");
                let agent_workspace = team.workspace_path.join("agents").join(&instance_name);
                std::fs::create_dir_all(&agent_workspace)?;

                // TODO: Create instance from image
                let instance_id = format!("inst_{}", Uuid::new_v4().simple());
                added.push(instance_id.clone());

                team.agent_instances
                    .entry(agent_name.to_string())
                    .or_default()
                    .push(instance_id);

                tracing::debug!(
                    "Added instance {} for agent {} in team {}",
                    instance_name,
                    agent_name,
                    team_id
                );
            }
        } else {
            // Scale down - remove excess instances
            let to_remove = current_instances - new_count;
            let instances = team
                .agent_instances
                .get_mut(agent_name)
                .ok_or_else(|| anyhow::anyhow!("No instances for agent {agent_name}"))?;

            for _ in 0..to_remove {
                if let Some(instance_id) = instances.pop() {
                    // TODO: Stop the instance gracefully
                    removed.push(instance_id.clone());
                    tracing::debug!(
                        "Removed instance {} for agent {} in team {}",
                        instance_id,
                        agent_name,
                        team_id
                    );
                }
            }
        }

        tracing::info!(
            "Scaled agent {} in team {} from {} to {} instances",
            agent_name,
            team_id,
            current_instances,
            new_count
        );

        Ok(ScaleResult {
            team_id: team_id.clone(),
            agent_name: agent_name.to_string(),
            previous_count: current_instances,
            new_count,
            added_instance_ids: added,
            removed_instance_ids: removed,
        })
    }

    /// Get the event bus for a team
    pub async fn get_bus(&self, team_id: &TeamId) -> Option<Arc<dyn EventBus>> {
        let buses = self.buses.read().await;
        buses.get(team_id).cloned()
    }

    /// Get shared services fabric for a team
    pub async fn get_shared_services(&self, team_id: &TeamId) -> Option<SharedServicesFabric> {
        let shared = self.shared_services.read().await;
        shared.get(team_id).cloned()
    }
}

impl Default for TeamManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a scale operation
#[derive(Debug, Clone)]
pub struct ScaleResult {
    /// Team ID
    pub team_id: TeamId,
    /// Agent name that was scaled
    pub agent_name: String,
    /// Previous instance count
    pub previous_count: u32,
    /// New instance count
    pub new_count: u32,
    /// IDs of newly added instances
    pub added_instance_ids: Vec<String>,
    /// IDs of removed instances
    pub removed_instance_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> TeamConfig {
        let toml = r#"
[team]
name = "test-team"

[[agents]]
name = "coordinator"
image = "./agents/coordinator"
instances = 1
role = "coordinator"

[[agents]]
name = "worker"
image = "./agents/worker"
instances = 2
role = "worker"
"#;
        TeamConfig::from_str(toml).unwrap()
    }

    #[test]
    fn test_team_status_display() {
        assert_eq!(TeamStatus::Running.to_string(), "running");
        assert_eq!(TeamStatus::Stopped.to_string(), "stopped");
    }

    #[test]
    fn test_team_workspace_paths() {
        let config = create_test_config();
        let team = Team {
            id: "team_123".to_string(),
            name: "test-team".to_string(),
            status: TeamStatus::Running,
            workspace_path: PathBuf::from("/tmp/peko/teams/test-team"),
            config,
            agent_instances: HashMap::new(),
            error: None,
            created_at: chrono::Utc::now(),
        };

        assert_eq!(
            team.shared_files_path(),
            PathBuf::from("/tmp/peko/teams/test-team/.peko/teams/test-team/shared/files")
        );
    }
}
