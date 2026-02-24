//! Agent Management Tools
//!
//! Tools for agents to interact with other agents.
//! Note: agent_spawn, agent_info, agent_broadcast require ManagerCommand channel
//! which needs to be properly integrated with AgentManager's event loop.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use crate::tools::Tool;

/// Messages that can be sent to the agent manager
#[derive(Debug)]
pub enum ManagerCommand {
    /// List all agents
    ListAgents {
        respond_to: mpsc::Sender<Vec<crate::manager::AgentInfo>>,
    },
    /// Spawn a new agent
    Spawn {
        config: crate::types::agent::AgentConfig,
        respond_to: mpsc::Sender<anyhow::Result<crate::manager::AgentHandle>>,
    },
    /// Broadcast message
    Broadcast {
        message: String,
        respond_to: mpsc::Sender<anyhow::Result<()>>,
    },
}

/// Agent list entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentListEntry {
    /// Agent ID
    pub id: String,
    /// Agent name
    pub name: String,
    /// Current state
    pub state: String,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Uptime in seconds
    pub uptime_secs: u64,
}

/// List agents result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsListResult {
    /// Available agents
    pub agents: Vec<AgentListEntry>,
    /// Total count
    pub total: usize,
    /// Whether any agent can be targeted
    pub allow_any: bool,
}

/// Tool for listing available agents (OpenClaw compatible)
pub struct AgentsListTool {
    command_tx: mpsc::Sender<ManagerCommand>,
}

impl AgentsListTool {
    pub fn new(command_tx: mpsc::Sender<ManagerCommand>) -> Self {
        Self { command_tx }
    }
}

#[async_trait]
impl Tool for AgentsListTool {
    fn name(&self) -> &str {
        "agents_list"
    }

    fn description(&self) -> &str {
        "List agent IDs that can be targeted with sessions_spawn"
    }

    async fn execute(&self,
        _params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::ListAgents { respond_to: tx })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {}", e))?;

        let agents: Vec<crate::manager::AgentInfo> = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Manager channel closed"))?;

        let entries: Vec<AgentListEntry> = agents
            .into_iter()
            .map(|a| AgentListEntry {
                id: a.did.clone(),
                name: a.name,
                state: format!("{:?}", a.state),
                capabilities: a.capabilities,
                uptime_secs: a.uptime_secs,
            })
            .collect();

        let result = AgentsListResult {
            total: entries.len(),
            allow_any: true,
            agents: entries,
        };

        Ok(serde_json::to_value(result)?)
    }
}

/// Tool for agents to query information about other agents
pub struct AgentInfoTool {
    command_tx: mpsc::Sender<ManagerCommand>,
}

impl AgentInfoTool {
    pub fn new(command_tx: mpsc::Sender<ManagerCommand>) -> Self {
        Self { command_tx }
    }
}

#[async_trait]
impl Tool for AgentInfoTool {
    fn name(&self) -> &str {
        "agent_info"
    }

    fn description(&self) -> &str {
        r#"Query detailed information about a specific agent.

Parameters:
- agent_id: DID of the agent to query (required)

Example:
{"agent_id": "did:peko:abc123"}"#
    }

    async fn execute(
        &self,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let agent_id = params
            .get("agent_id")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'agent_id' parameter"))?;

        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::ListAgents { respond_to: tx })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {}", e))?;

        let agents: Vec<crate::manager::AgentInfo> = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Manager channel closed"))?;

        // Find the specific agent
        let agent = agents
            .into_iter()
            .find(|a| a.did == agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", agent_id))?;

        Ok(json!({
            "success": true,
            "agent": {
                "did": agent.did,
                "name": agent.name,
                "state": format!("{:?}", agent.state),
                "capabilities": agent.capabilities,
                "uptime_secs": agent.uptime_secs,
                "identity": {
                    "did": agent.identity_info.did,
                    "scope": format!("{:?}", agent.identity_info.scope)
                }
            }
        }))
    }
}

/// Tool for spawning subagents
pub struct AgentSpawnTool {
    command_tx: mpsc::Sender<ManagerCommand>,
}

impl AgentSpawnTool {
    pub fn new(command_tx: mpsc::Sender<ManagerCommand>) -> Self {
        Self { command_tx }
    }
}

#[async_trait]
impl Tool for AgentSpawnTool {
    fn name(&self) -> &str {
        "agent_spawn"
    }

    fn description(&self) -> &str {
        r#"Spawn a new subagent.

Parameters:
- name: Name for the new agent (required)
- capabilities: List of capabilities (optional)
- prompt: Initial task for the agent (required)

Example:
{"name": "ResearchAgent", "prompt": "Research Rust", "capabilities": ["web_search"]}"#
    }

    async fn execute(
        &self,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let name = params
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?
            .to_string();

        let _prompt = params.get("prompt").and_then(|p| p.as_str()).unwrap_or("");

        let capabilities = params
            .get("capabilities")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let config = crate::types::agent::AgentConfig {
            name: name.clone(),
            capabilities: capabilities
                .iter()
                .map(|c| crate::types::agent::AgentCapability {
                    name: c.clone(),
                    version: "1.0".to_string(),
                    description: None,
                    estimated_cost: None,
                    estimated_duration: None,
                    parameters: None,
                    required_auth: None,
                })
                .collect(),
            ..Default::default()
        };

        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::Spawn { config, respond_to: tx })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {}", e))?;
        let handle = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Manager channel closed"))??;
        let did = handle.did().to_string();

        Ok(json!({
            "success": true,
            "agent": {
                "did": did,
                "name": name,
                "status": "spawned"
            }
        }))
    }
}

/// Tool for broadcasting messages
pub struct AgentBroadcastTool {
    command_tx: mpsc::Sender<ManagerCommand>,
}

impl AgentBroadcastTool {
    pub fn new(command_tx: mpsc::Sender<ManagerCommand>) -> Self {
        Self { command_tx }
    }
}

#[async_trait]
impl Tool for AgentBroadcastTool {
    fn name(&self) -> &str {
        "agent_broadcast"
    }

    fn description(&self) -> &str {
        r#"Broadcast a message to all agents.

Example:
{"message": "System shutdown in 5 min"}"#
    }

    async fn execute(
        &self,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let message = params
            .get("message")
            .and_then(|m| m.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?
            .to_string();

        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::Broadcast { message, respond_to: tx })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {}", e))?;
        rx.recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Manager channel closed"))??;

        Ok(json!({
            "success": true,
            "message": "Broadcast sent"
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_exist() {
        // Just verify types compile
    }
}
