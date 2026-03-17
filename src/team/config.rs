//! Team configuration parser (team.toml)
//!
//! Implements DATA_MODEL.md §4 - Team Definition

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Team configuration root structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    /// Team identity
    #[serde(rename = "team")]
    pub identity: TeamIdentity,

    /// Agent definitions
    #[serde(rename = "agents")]
    pub agents: Vec<AgentDefinition>,

    /// Shared services configuration
    #[serde(rename = "shared")]
    pub shared: Option<SharedServices>,
}

/// Team identity section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamIdentity {
    /// Team name (lowercase alphanumeric + hyphens)
    pub name: String,

    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Agent definition within a team
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Agent name (unique within team, used as prefix for instances)
    pub name: String,

    /// Image reference (path, local tag, or registry ref)
    pub image: String,

    /// Number of instances to create (default: 1)
    #[serde(default = "default_instances")]
    pub instances: u32,

    /// Agent role (coordinator, worker, or null)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<AgentRole>,

    /// Instance-level environment variable overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// Agent role within team
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Team coordinator
    Coordinator,
    /// Worker agent
    Worker,
}

/// Shared services configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SharedServices {
    /// Event bus configuration
    #[serde(rename = "bus")]
    pub bus: Option<BusConfig>,

    /// Vector memory configuration
    #[serde(rename = "memory")]
    pub memory: Option<MemoryConfig>,

    /// Shared file workspace
    #[serde(rename = "files")]
    pub files: Option<FilesConfig>,

    /// Shared MCP servers
    #[serde(rename = "mcps")]
    pub mcps: Option<Vec<SharedMcpConfig>>,
}

/// Event bus backend configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusConfig {
    /// Backend type: in-memory, redis, nats
    #[serde(default = "default_bus_backend")]
    pub backend: BusBackend,

    /// Connection URL (required for redis/nats)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Bus backend types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusBackend {
    /// In-memory backend (single process)
    InMemory,
    /// Redis Streams backend
    Redis,
    /// NATS JetStream backend
    Nats,
}

/// Vector memory configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Memory type: chroma, qdrant, in-memory
    #[serde(default = "default_memory_type")]
    #[serde(rename = "type")]
    pub type_: MemoryType,

    /// Connection URL (required for chroma/qdrant)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Whether to persist data
    #[serde(default = "default_true")]
    pub persist: bool,
}

/// Memory backend types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// In-memory vector store
    InMemory,
    /// Chroma vector store
    Chroma,
    /// Qdrant vector store
    Qdrant,
}

/// Shared file workspace configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesConfig {
    /// Whether shared files are enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Custom path for shared files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Shared MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedMcpConfig {
    /// MCP server name
    pub name: String,

    /// Command to start the MCP server
    pub command: Vec<String>,

    /// Environment variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

// Default functions
fn default_instances() -> u32 {
    1
}

fn default_bus_backend() -> BusBackend {
    BusBackend::InMemory
}

fn default_memory_type() -> MemoryType {
    MemoryType::InMemory
}

fn default_true() -> bool {
    true
}

impl TeamConfig {
    /// Load team configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_str(&content)
    }

    /// Parse team configuration from TOML string
    pub fn from_str(content: &str) -> anyhow::Result<Self> {
        let config: TeamConfig = toml::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration
    pub fn validate(&self) -> anyhow::Result<()> {
        // Validate team name
        if self.identity.name.is_empty() {
            anyhow::bail!("Team name cannot be empty");
        }
        if !is_valid_team_name(&self.identity.name) {
            anyhow::bail!(
                "Invalid team name '{}'. Must be lowercase alphanumeric with hyphens only.",
                self.identity.name
            );
        }

        // Validate agents
        if self.agents.is_empty() {
            anyhow::bail!("Team must have at least one agent definition");
        }

        let mut names = std::collections::HashSet::new();
        for agent in &self.agents {
            // Check for duplicate names
            if !names.insert(&agent.name) {
                anyhow::bail!("Duplicate agent name '{}' in team", agent.name);
            }

            // Validate agent name
            if !is_valid_agent_name(&agent.name) {
                anyhow::bail!(
                    "Invalid agent name '{}'. Must be lowercase alphanumeric with hyphens only.",
                    agent.name
                );
            }

            // Validate instances
            if agent.instances == 0 {
                anyhow::bail!("Agent '{}' must have at least 1 instance", agent.name);
            }

            // Validate image reference is not empty
            if agent.image.is_empty() {
                anyhow::bail!("Agent '{}' must have an image reference", agent.name);
            }
        }

        // Validate shared services
        if let Some(ref shared) = self.shared {
            // Validate bus URL requirements
            if let Some(ref bus) = shared.bus {
                match bus.backend {
                    BusBackend::Redis | BusBackend::Nats => {
                        if bus.url.is_none() {
                            anyhow::bail!("Bus backend '{:?}' requires a URL", bus.backend);
                        }
                    }
                    BusBackend::InMemory => {}
                }
            }

            // Validate memory URL requirements
            if let Some(ref memory) = shared.memory {
                match memory.type_ {
                    MemoryType::Chroma | MemoryType::Qdrant => {
                        if memory.url.is_none() {
                            anyhow::bail!("Memory type '{:?}' requires a URL", memory.type_);
                        }
                    }
                    MemoryType::InMemory => {}
                }
            }
        }

        Ok(())
    }

    /// Get the default shared files path for this team
    pub fn default_shared_files_path(&self) -> String {
        format!(".pekobot/teams/{}/shared/files", self.identity.name)
    }

    /// Get the shared files path (custom or default)
    pub fn shared_files_path(&self) -> String {
        self.shared
            .as_ref()
            .and_then(|s| s.files.as_ref())
            .and_then(|f| f.path.clone())
            .unwrap_or_else(|| self.default_shared_files_path())
    }

    /// Check if shared files are enabled
    pub fn shared_files_enabled(&self) -> bool {
        self.shared
            .as_ref()
            .and_then(|s| s.files.as_ref())
            .map(|f| f.enabled)
            .unwrap_or(true)
    }

    /// Get the bus backend configuration
    pub fn bus_config(&self) -> BusConfig {
        self.shared
            .as_ref()
            .and_then(|s| s.bus.clone())
            .unwrap_or_else(|| BusConfig {
                backend: BusBackend::InMemory,
                url: None,
            })
    }
}

/// Validate team name format (lowercase alphanumeric + hyphens)
fn is_valid_team_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Validate agent name format (lowercase alphanumeric + hyphens)
fn is_valid_agent_name(name: &str) -> bool {
    is_valid_team_name(name) // Same rules as team name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_team_config() {
        let toml = r#"
[team]
name = "research-team"
description = "A research team"

[[agents]]
name = "coordinator"
image = "./agents/coordinator"
instances = 1
role = "coordinator"

[[agents]]
name = "researcher"
image = "pekohub.com/agents/researcher:v2.5"
instances = 3
role = "worker"

[shared.bus]
backend = "in_memory"

[shared.files]
enabled = true
"#;

        let config = TeamConfig::from_str(toml).unwrap();
        assert_eq!(config.identity.name, "research-team");
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.agents[0].name, "coordinator");
        assert_eq!(config.agents[1].instances, 3);
    }

    #[test]
    fn test_validate_team_name() {
        assert!(is_valid_team_name("research-team"));
        assert!(is_valid_team_name("agent123"));
        assert!(!is_valid_team_name("Research-Team")); // uppercase
        assert!(!is_valid_team_name("research_team")); // underscore
        assert!(!is_valid_team_name("-research")); // starts with hyphen
        assert!(!is_valid_team_name("research-")); // ends with hyphen
    }

    #[test]
    fn test_duplicate_agent_names() {
        let toml = r#"
[team]
name = "test-team"

[[agents]]
name = "agent1"
image = "./agent1"

[[agents]]
name = "agent1"
image = "./agent1"
"#;

        let result = TeamConfig::from_str(toml);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Duplicate"));
    }

    #[test]
    fn test_bus_backend_requires_url() {
        let toml = r#"
[team]
name = "test-team"

[[agents]]
name = "agent1"
image = "./agent1"

[shared.bus]
backend = "redis"
"#;

        let result = TeamConfig::from_str(toml);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires a URL"));
    }
}
