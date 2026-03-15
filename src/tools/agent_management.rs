//! Agent Management Tools
//!
//! Tools for agents to interact with other agents.
//! Note: `agent_spawn` uses the session overlay architecture for subagent isolation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use crate::session::types::Peer;
use crate::tools::Tool;

/// Messages that can be sent to the agent manager
#[derive(Debug)]
pub enum ManagerCommand {
    /// List all agents
    ListAgents {
        respond_to: mpsc::Sender<Vec<crate::agent::AgentInfo>>,
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

/// Tool for listing available agents (`OpenClaw` compatible)
pub struct AgentsListTool {
    command_tx: mpsc::Sender<ManagerCommand>,
}

impl AgentsListTool {
    #[must_use]
    pub fn new(command_tx: mpsc::Sender<ManagerCommand>) -> Self {
        Self { command_tx }
    }
}

#[async_trait]
impl Tool for AgentsListTool {
    fn name(&self) -> &'static str {
        "agents_list"
    }

    fn description(&self) -> &'static str {
        "List agent IDs that can be targeted with sessions_spawn"
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::ListAgents { respond_to: tx })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {e}"))?;

        let agents: Vec<crate::agent::AgentInfo> = rx
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
    #[must_use]
    pub fn new(command_tx: mpsc::Sender<ManagerCommand>) -> Self {
        Self { command_tx }
    }
}

#[async_trait]
impl Tool for AgentInfoTool {
    fn name(&self) -> &'static str {
        "agent_info"
    }

    fn description(&self) -> &'static str {
        r#"Query detailed information about a specific agent.

Parameters:
- agent_id: DID of the agent to query (required)

Example:
{"agent_id": "did:peko:abc123"}"#
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let agent_id = params
            .get("agent_id")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'agent_id' parameter"))?;

        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::ListAgents { respond_to: tx })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {e}"))?;

        let agents: Vec<crate::agent::AgentInfo> = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("Manager channel closed"))?;

        // Find the specific agent
        let agent = agents
            .into_iter()
            .find(|a| a.did == agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {agent_id}"))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_exist() {
        // Just verify types compile
    }
}
