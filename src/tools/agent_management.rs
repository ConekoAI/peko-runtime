//! Agent Management Tools (DEPRECATED)
#![allow(deprecated)]
//!
//! Tools for agents to interact with other agents within their team.
//! Implements CAPABILITY_INTERFACE.md §3.7, §3.8
//! - Team-scoped: returns only instances within the current team
//! - Cross-team access requires explicit grant

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use crate::session::types::Peer;
use crate::tools::Tool;

/// Messages that can be sent to the agent manager
#[derive(Debug)]
pub enum ManagerCommand {
    /// List all agents with optional team filter
    ListAgents {
        team_id: Option<String>,
        include_cross_team: bool,
        respond_to: mpsc::Sender<Vec<crate::agent::AgentInfo>>,
    },
    /// Get specific agent info
    GetAgent {
        agent_id: String,
        team_id: Option<String>,
        include_cross_team: bool,
        respond_to: mpsc::Sender<Option<crate::agent::AgentInfo>>,
    },
    /// Spawn a new agent
    Spawn {
        config: crate::types::agent::AgentConfig,
        respond_to: mpsc::Sender<anyhow::Result<crate::agent::AgentHandle>>,
    },
    /// Spawn a subagent session (session overlay architecture)
    SpawnSession {
        agent_did: String,
        peer: Peer,
        task: String,
        isolated: bool,
        parent_session_key: String,
        timeout_seconds: Option<u64>,
        respond_to: mpsc::Sender<anyhow::Result<SpawnSessionResult>>,
    },
}

/// Result of spawning a subagent session
#[derive(Debug, Clone)]
pub struct SpawnSessionResult {
    /// Spawn ID
    pub spawn_id: String,
    /// Full session key for the spawn overlay
    pub session_key: String,
    /// Parent session key
    pub parent_session_key: String,
    /// Whether this spawn is isolated
    pub isolated: bool,
    /// Spawn depth
    pub depth: u32,
}

/// Agent list entry - spec compliant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentListEntry {
    /// Agent ID
    pub id: String,
    /// Agent name
    pub name: String,
    /// Image reference
    pub image: String,
    /// Current status
    pub status: String,
    /// Role in team
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Team ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}

/// List agents result - spec compliant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsListResult {
    /// Available instances
    pub instances: Vec<AgentListEntry>,
}

/// Agent info result - spec compliant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfoResult {
    pub id: String,
    pub name: String,
    pub image_ref: String,
    pub image_digest: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
    pub created_at: String,
    pub capabilities: AgentCapabilities,
}

/// Agent capabilities
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentCapabilities {
    pub tools: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

/// Tool for listing available agents (`agents_list`)
/// Team-scoped: returns only instances within the current team
pub struct AgentsListTool {
    command_tx: mpsc::Sender<ManagerCommand>,
    team_id: Option<String>,
    allow_cross_team: bool,
}

impl AgentsListTool {
    #[must_use]
    pub fn new(
        command_tx: mpsc::Sender<ManagerCommand>,
        team_id: Option<String>,
        allow_cross_team: bool,
    ) -> Self {
        Self {
            command_tx,
            team_id,
            allow_cross_team,
        }
    }
}

/// Arguments for agents_list
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsListArgs {
    /// Filter by status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_filter: Option<String>,
    /// Filter by role
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_filter: Option<String>,
}

#[async_trait]
impl Tool for AgentsListTool {
    fn name(&self) -> &'static str {
        "agents_list"
    }

    fn description(&self) -> &'static str {
        "List agent instances that can be targeted via agent_spawn or sessions_send. \
         Returns only instances within the current team (or empty list for standalone agents)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "status_filter": {
                    "type": "string",
                    "description": "Filter by instance status (e.g., 'running')"
                },
                "role_filter": {
                    "type": "string",
                    "description": "Filter by agent role (e.g., 'worker')"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: AgentsListArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::ListAgents {
                team_id: self.team_id.clone(),
                include_cross_team: self.allow_cross_team,
                respond_to: tx,
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {}", e))?;

        let agents: Vec<crate::agent::AgentInfo> = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Manager channel closed"))?;

        // Filter and map to result format
        let mut entries: Vec<AgentListEntry> = agents
            .into_iter()
            .filter(|a| {
                // Apply status filter if provided
                if let Some(ref status_filter) = args.status_filter {
                    let status_str = format!("{:?}", a.state).to_lowercase();
                    if !status_str.contains(&status_filter.to_lowercase()) {
                        return false;
                    }
                }
                true
            })
            .map(|a| AgentListEntry {
                id: a.did,
                name: a.name,
                image: a.image_ref.unwrap_or_default(),
                status: format!("{:?}", a.state).to_lowercase(),
                role: a.role,
                team_id: a.team_id,
            })
            .collect();

        // Apply role filter if provided
        if let Some(ref role_filter) = args.role_filter {
            entries.retain(|e| {
                e.role
                    .as_ref()
                    .map(|r| r.to_lowercase() == role_filter.to_lowercase())
                    .unwrap_or(false)
            });
        }

        let result = AgentsListResult { instances: entries };

        Ok(serde_json::to_value(result)?)
    }
}

/// Tool for agents to query information about other agents (`agent_info`)
/// Team-scoped: only returns info for agents in the same team
pub struct AgentInfoTool {
    command_tx: mpsc::Sender<ManagerCommand>,
    team_id: Option<String>,
    allow_cross_team: bool,
}

impl AgentInfoTool {
    #[must_use]
    pub fn new(
        command_tx: mpsc::Sender<ManagerCommand>,
        team_id: Option<String>,
        allow_cross_team: bool,
    ) -> Self {
        Self {
            command_tx,
            team_id,
            allow_cross_team,
        }
    }
}

/// Arguments for agent_info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfoArgs {
    pub instance_id: String,
}

#[async_trait]
impl Tool for AgentInfoTool {
    fn name(&self) -> &'static str {
        "agent_info"
    }

    fn description(&self) -> &'static str {
        "Query detailed information about a specific agent instance. \
         Only returns info for agents in the same team."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "instance_id": {
                    "type": "string",
                    "description": "ID of the agent instance to query"
                }
            },
            "required": ["instance_id"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: AgentInfoArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::GetAgent {
                agent_id: args.instance_id.clone(),
                team_id: self.team_id.clone(),
                include_cross_team: self.allow_cross_team,
                respond_to: tx,
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {}", e))?;

        let agent_opt: Option<crate::agent::AgentInfo> = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Manager channel closed"))?;

        let agent = agent_opt.ok_or_else(|| {
            anyhow::anyhow!(
                "Agent not found: {} (may not be in your team)",
                args.instance_id
            )
        })?;

        let result = AgentInfoResult {
            id: agent.did,
            name: agent.name,
            image_ref: agent.image_ref.unwrap_or_default(),
            image_digest: agent.image_digest.unwrap_or_default(),
            status: format!("{:?}", agent.state).to_lowercase(),
            role: agent.role,
            team_id: agent.team_id,
            active_session_id: agent.active_session_id,
            created_at: agent.created_at.to_rfc3339(),
            capabilities: AgentCapabilities {
                tools: agent.capabilities,
                skills: agent.skills.unwrap_or_default(),
            },
        };

        Ok(serde_json::to_value(result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_list_entry_serialization() {
        let entry = AgentListEntry {
            id: "inst_123".to_string(),
            name: "researcher-1".to_string(),
            image: "researcher:v2".to_string(),
            status: "running".to_string(),
            role: Some("worker".to_string()),
            team_id: Some("team_456".to_string()),
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("inst_123"));
        assert!(json.contains("worker"));
    }

    #[test]
    fn test_agents_list_args_parsing() {
        let json = r#"{"status_filter": "running", "role_filter": "worker"}"#;
        let args: AgentsListArgs = serde_json::from_str(json).unwrap();

        assert_eq!(args.status_filter, Some("running".to_string()));
        assert_eq!(args.role_filter, Some("worker".to_string()));
    }

    #[test]
    fn test_agent_info_args_parsing() {
        let json = r#"{"instance_id": "inst_abc123"}"#;
        let args: AgentInfoArgs = serde_json::from_str(json).unwrap();

        assert_eq!(args.instance_id, "inst_abc123");
    }
}
