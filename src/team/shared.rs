//! Shared Services Fabric
//!
//! Manages shared infrastructure accessible to all agents in a team:
//! - Shared file workspace
//! - Shared MCP servers (reference-counted)
//! - Vector memory (namespace management)

use super::config::{FilesConfig, SharedMcpConfig, TeamConfig};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared services fabric for a team
#[derive(Debug, Clone)]
pub struct SharedServicesFabric {
    /// Team ID
    pub team_id: String,
    /// Shared files configuration
    pub files_config: Option<FilesConfig>,
    /// Shared files absolute path
    pub files_path: PathBuf,
    /// Shared MCP servers
    pub mcp_servers: HashMap<String, SharedMcpServer>,
    /// Reference counts for MCP servers (`agent_id` -> set of mcp names)
    mcp_ref_counts: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

/// Shared MCP server state
#[derive(Debug, Clone)]
pub struct SharedMcpServer {
    /// Server name
    pub name: String,
    /// Server configuration
    pub config: SharedMcpConfig,
    /// Process ID (if running)
    pub pid: Option<u32>,
    /// Server status
    pub status: McpServerStatus,
}

/// MCP server status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerStatus {
    /// Not started
    Stopped,
    /// Starting up
    Starting,
    /// Running and ready
    Running,
    /// Error state
    Error,
}

impl SharedServicesFabric {
    /// Create shared services fabric from team configuration
    pub async fn new(config: &TeamConfig, workspace_path: &PathBuf) -> Result<Self> {
        let team_id = format!("team_{}", config.identity.name);

        // Determine shared files path
        let files_path = if let Some(ref shared) = config.shared {
            if let Some(ref files) = shared.files {
                if files.enabled {
                    if let Some(ref path) = files.path {
                        if std::path::Path::new(path).is_absolute() {
                            PathBuf::from(path)
                        } else {
                            workspace_path.join(path)
                        }
                    } else {
                        workspace_path.join("shared").join("files")
                    }
                } else {
                    workspace_path.join("shared").join("files") // Create anyway, just don't use
                }
            } else {
                workspace_path.join("shared").join("files")
            }
        } else {
            workspace_path.join("shared").join("files")
        };

        // Create files directory
        tokio::fs::create_dir_all(&files_path).await?;

        // Initialize MCP servers from config
        let mut mcp_servers = HashMap::new();
        if let Some(ref shared) = config.shared {
            if let Some(ref mcps) = shared.mcps {
                for mcp_config in mcps {
                    let server = SharedMcpServer {
                        name: mcp_config.name.clone(),
                        config: mcp_config.clone(),
                        pid: None,
                        status: McpServerStatus::Stopped,
                    };
                    mcp_servers.insert(mcp_config.name.clone(), server);
                }
            }
        }

        Ok(Self {
            team_id,
            files_config: config.shared.as_ref().and_then(|s| s.files.clone()),
            files_path,
            mcp_servers,
            mcp_ref_counts: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Get the shared files path
    #[must_use] 
    pub fn files_path(&self) -> &PathBuf {
        &self.files_path
    }

    /// Check if shared files are enabled
    #[must_use] 
    pub fn files_enabled(&self) -> bool {
        self.files_config
            .as_ref()
            .is_none_or(|f| f.enabled)
    }

    /// Register an agent as a consumer of shared MCP servers
    pub async fn register_agent_mcps(&self, agent_id: &str, mcp_names: Vec<String>) -> Result<()> {
        let mut ref_counts = self.mcp_ref_counts.write().await;

        // Start any MCP servers that aren't already running
        for mcp_name in &mcp_names {
            if let Some(server) = self.mcp_servers.get(mcp_name) {
                if server.status == McpServerStatus::Stopped {
                    self.start_mcp_server(mcp_name).await?;
                }
            }
        }

        ref_counts.insert(agent_id.to_string(), mcp_names);
        Ok(())
    }

    /// Unregister an agent from shared MCP servers
    pub async fn unregister_agent_mcps(&self, agent_id: &str) -> Result<()> {
        let mut ref_counts = self.mcp_ref_counts.write().await;
        ref_counts.remove(agent_id);

        // Check which MCP servers are still in use
        let mut active_mcps: std::collections::HashSet<String> = std::collections::HashSet::new();
        for mcps in ref_counts.values() {
            for mcp in mcps {
                active_mcps.insert(mcp.clone());
            }
        }

        // Stop MCP servers that are no longer referenced
        for (name, server) in &self.mcp_servers {
            if server.status == McpServerStatus::Running && !active_mcps.contains(name) {
                self.stop_mcp_server(name).await?;
            }
        }

        Ok(())
    }

    /// Start an MCP server
    async fn start_mcp_server(&self, name: &str) -> Result<()> {
        tracing::info!(
            "Starting shared MCP server {} for team {}",
            name,
            self.team_id
        );

        // TODO: Implement actual MCP server process start
        // This would involve:
        // 1. Spawning the process with the command from config
        // 2. Setting up environment variables
        // 3. Managing the process lifecycle
        // 4. Health checking

        tracing::info!(
            "Shared MCP server {} started for team {}",
            name,
            self.team_id
        );
        Ok(())
    }

    /// Stop an MCP server
    async fn stop_mcp_server(&self, name: &str) -> Result<()> {
        tracing::info!(
            "Stopping shared MCP server {} for team {}",
            name,
            self.team_id
        );

        // TODO: Implement actual MCP server process stop

        tracing::info!(
            "Shared MCP server {} stopped for team {}",
            name,
            self.team_id
        );
        Ok(())
    }

    /// Get shared MCP server info
    #[must_use] 
    pub fn get_mcp_server(&self, name: &str) -> Option<&SharedMcpServer> {
        self.mcp_servers.get(name)
    }

    /// List all shared MCP servers
    #[must_use] 
    pub fn list_mcp_servers(&self) -> Vec<&SharedMcpServer> {
        self.mcp_servers.values().collect()
    }

    /// Get vector memory namespace for an agent instance
    ///
    /// Per `DATA_MODEL.md` §2.4 (namespacing):
    /// - Private namespace: `{instance_id}`
    /// - Agent-type namespace: `{agent_name}`
    /// - Team shared namespace: `_team_shared`
    #[must_use] 
    pub fn get_memory_namespace(
        &self,
        instance_id: &str,
        agent_name: &str,
        scope: MemoryScope,
    ) -> String {
        match scope {
            MemoryScope::Private => instance_id.to_string(),
            MemoryScope::AgentType => agent_name.to_string(),
            MemoryScope::TeamShared => "_team_shared".to_string(),
        }
    }

    /// Shutdown all shared services
    pub async fn shutdown(&self) -> Result<()> {
        tracing::info!("Shutting down shared services for team {}", self.team_id);

        // Stop all MCP servers
        for name in self.mcp_servers.keys() {
            let _ = self.stop_mcp_server(name).await;
        }

        Ok(())
    }
}

/// Memory namespace scope
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    /// Private to the instance
    Private,
    /// Shared among all instances of the same agent type
    AgentType,
    /// Shared among all team members
    TeamShared,
}

/// Error types for shared services
#[derive(Debug, thiserror::Error)]
pub enum SharedServicesError {
    #[error("MCP server {0} not found")]
    McpServerNotFound(String),
    #[error("MCP server {0} failed to start: {1}")]
    McpStartFailed(String, String),
    #[error("Shared files disabled")]
    FilesDisabled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::config::TeamConfig;

    fn create_test_config() -> TeamConfig {
        let toml = r#"
[team]
name = "test-team"

[[agents]]
name = "agent1"
image = "./agent1"
instances = 1

[shared.files]
enabled = true
path = "shared/files"

[[shared.mcps]]
name = "browser"
command = ["npx", "-y", "@browserbasehq/mcp"]
"#;
        TeamConfig::from_str(toml).unwrap()
    }

    #[tokio::test]
    async fn test_shared_fabric_creation() {
        let config = create_test_config();
        let workspace = PathBuf::from("/tmp/test-team");

        let fabric = SharedServicesFabric::new(&config, &workspace)
            .await
            .unwrap();

        assert_eq!(fabric.team_id, "team_test-team");
        assert!(fabric.files_enabled());
        assert_eq!(fabric.mcp_servers.len(), 1);
        assert!(fabric.mcp_servers.contains_key("browser"));
    }

    #[test]
    fn test_memory_namespaces() {
        let fabric = SharedServicesFabric {
            team_id: "team_test".to_string(),
            files_config: None,
            files_path: PathBuf::from("/tmp"),
            mcp_servers: HashMap::new(),
            mcp_ref_counts: Arc::new(RwLock::new(HashMap::new())),
        };

        assert_eq!(
            fabric.get_memory_namespace("inst_123", "worker", MemoryScope::Private),
            "inst_123"
        );
        assert_eq!(
            fabric.get_memory_namespace("inst_123", "worker", MemoryScope::AgentType),
            "worker"
        );
        assert_eq!(
            fabric.get_memory_namespace("inst_123", "worker", MemoryScope::TeamShared),
            "_team_shared"
        );
    }
}
