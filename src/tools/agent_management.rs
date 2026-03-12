//! Agent Management Tools
//!
//! Tools for agents to interact with other agents.
//! Note: `agent_spawn` uses the session overlay architecture for subagent isolation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use crate::session::context::{SessionContext, SessionRouter};
use crate::session::types::{Peer, SpawnCleanupPolicy};
use crate::tools::Tool;
use tracing::info;

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
    /// Broadcast message
    Broadcast {
        message: String,
        respond_to: mpsc::Sender<anyhow::Result<()>>,
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

/// Tool for spawning subagent sessions
///
/// Uses the session overlay architecture to create isolated or shared
/// subagent contexts. This is different from spawning new agent processes -
/// it creates a spawn overlay within the current agent's session manager.
pub struct AgentSpawnTool {
    command_tx: Option<mpsc::Sender<ManagerCommand>>,
    /// Optional session router for standalone mode (when no manager)
    session_router: Option<SessionRouter>,
    agent_name: String,
    /// Current session context (for parent session reference)
    current_session: Option<SessionContext>,
}

impl AgentSpawnTool {
    /// Create a new spawn tool with manager command channel
    #[must_use]
    pub fn new(command_tx: mpsc::Sender<ManagerCommand>) -> Self {
        Self {
            command_tx: Some(command_tx),
            session_router: None,
            agent_name: String::new(),
            current_session: None,
        }
    }

    /// Create a new spawn tool for standalone mode (no manager)
    ///
    /// This version uses the Agent's session router directly.
    #[must_use]
    pub fn with_router(session_router: SessionRouter, agent_name: &str) -> Self {
        Self {
            command_tx: None,
            session_router: Some(session_router),
            agent_name: agent_name.to_string(),
            current_session: None,
        }
    }

    /// Create a new spawn tool with current session context
    #[must_use]
    pub fn with_session(
        command_tx: mpsc::Sender<ManagerCommand>,
        current_session: SessionContext,
    ) -> Self {
        Self {
            command_tx: Some(command_tx),
            session_router: None,
            agent_name: String::new(),
            current_session: Some(current_session),
        }
    }

    /// Create a spawn tool with router and session context
    #[must_use]
    pub fn with_router_and_session(
        session_router: SessionRouter,
        agent_name: &str,
        current_session: SessionContext,
    ) -> Self {
        Self {
            command_tx: None,
            session_router: Some(session_router),
            agent_name: agent_name.to_string(),
            current_session: Some(current_session),
        }
    }
}

#[async_trait]
impl Tool for AgentSpawnTool {
    fn name(&self) -> &'static str {
        "agent_spawn"
    }

    fn description(&self) -> &'static str {
        r#"Spawn a subagent session for isolated task execution.

This creates a spawn overlay - either isolated (new base session) or shared 
(inherits parent's base session). Results are announced back to the requester.

Parameters:
- task: Description of the task to execute (required)
- label: Label for this spawn (optional)
- isolated: If true, creates isolated session without parent context (default: false)
- timeout_seconds: Maximum runtime in seconds (optional, default: 300)
- cleanup: "keep" or "delete" - what to do with session after completion (default: "keep")

Examples:
// Shared context - can see parent's conversation history
{"task": "Continue research on Rust", "isolated": false}

// Isolated context - fresh session for sensitive work
{"task": "Analyze confidential data", "isolated": true, "cleanup": "delete"}

// With timeout and label
{"task": "Long running analysis", "label": "analysis", "timeout_seconds": 600}"#
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Parse parameters
        let task = params
            .get("task")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'task' parameter"))?
            .to_string();

        let label = params
            .get("label")
            .and_then(|l| l.as_str())
            .unwrap_or("spawn_task")
            .to_string();

        let isolated = params
            .get("isolated")
            .and_then(|i| i.as_bool())
            .unwrap_or(false);

        let timeout_seconds = params
            .get("timeout_seconds")
            .and_then(|t| t.as_u64())
            .or(Some(300)); // Default 5 minutes

        let cleanup = params
            .get("cleanup")
            .and_then(|c| c.as_str())
            .map(|s| match s.to_lowercase().as_str() {
                "delete" => SpawnCleanupPolicy::Delete,
                _ => SpawnCleanupPolicy::Keep,
            })
            .unwrap_or(SpawnCleanupPolicy::Keep);

        // Get parent session key and peer
        let (parent_session_key, peer, agent_name) = if let Some(ref ctx) = self.current_session {
            let key = ctx.full_session_key().await;
            let p = ctx.peer().await;
            let name = ctx.agent_name().await;
            (key, p, name)
        } else {
            // Use defaults if no current context
            (
                format!("agent:{}:peer:user:default", self.agent_name),
                Peer::User("default".to_string()),
                self.agent_name.clone(),
            )
        };

        // Spawn the session using either command channel (manager mode) or router (standalone)
        let result = if let Some(ref command_tx) = self.command_tx {
            // Manager mode: send command to manager
            let (tx, mut rx) = mpsc::channel(1);
            command_tx
                .send(ManagerCommand::SpawnSession {
                    agent_did: format!("did:pekobot:local:default:{}", agent_name),
                    peer: peer.clone(),
                    task: task.clone(),
                    isolated,
                    parent_session_key: parent_session_key.clone(),
                    timeout_seconds,
                    respond_to: tx,
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to send spawn session command: {e}"))?;

            rx.recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("Manager channel closed"))??
        } else if let Some(ref router) = self.session_router {
            // Standalone mode: use session router directly
            let spawn_ctx = router
                .spawn(
                    &agent_name,
                    &peer,
                    &task,
                    isolated,
                    &parent_session_key,
                    timeout_seconds,
                )
                .await?;

            let session_key = spawn_ctx.full_session_key().await;
            let is_isolated = spawn_ctx.is_isolated().await;

            // Get spawn overlay details
            let (spawn_id, depth) = if let Some(spawn_arc) = spawn_ctx.hybrid.overlay.as_spawn() {
                let spawn = spawn_arc.read().await;
                (spawn.spawn_id.clone(), spawn.depth)
            } else {
                ("unknown".to_string(), 0)
            };

            SpawnSessionResult {
                spawn_id,
                session_key,
                parent_session_key: parent_session_key.clone(),
                isolated: is_isolated,
                depth,
            }
        } else {
            return Err(anyhow::anyhow!(
                "AgentSpawnTool requires either a command channel or session router"
            ));
        };

        info!(
            "Spawned subagent session: {} (isolated: {}, depth: {})",
            result.spawn_id, result.isolated, result.depth
        );

        Ok(json!({
            "success": true,
            "spawn": {
                "id": result.spawn_id,
                "session_key": result.session_key,
                "parent_session_key": parent_session_key,
                "label": label,
                "isolated": result.isolated,
                "depth": result.depth,
                "timeout_seconds": timeout_seconds,
                "cleanup": match cleanup {
                    SpawnCleanupPolicy::Keep => "keep",
                    SpawnCleanupPolicy::Delete => "delete",
                },
                "status": "created",
                "note": "Results will be announced when task completes"
            }
        }))
    }
}

/// Tool for broadcasting messages
pub struct AgentBroadcastTool {
    command_tx: mpsc::Sender<ManagerCommand>,
}

impl AgentBroadcastTool {
    #[must_use]
    pub fn new(command_tx: mpsc::Sender<ManagerCommand>) -> Self {
        Self { command_tx }
    }
}

#[async_trait]
impl Tool for AgentBroadcastTool {
    fn name(&self) -> &'static str {
        "agent_broadcast"
    }

    fn description(&self) -> &'static str {
        r#"Broadcast a message to all agents.

Example:
{"message": "System shutdown in 5 min"}"#
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let message = params
            .get("message")
            .and_then(|m| m.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?
            .to_string();

        let (tx, mut rx) = mpsc::channel(1);
        self.command_tx
            .send(ManagerCommand::Broadcast {
                message,
                respond_to: tx,
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {e}"))?;
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
