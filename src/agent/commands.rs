//! Agent manager command handler

use crate::agent::{
    pool::{AgentHandle, AgentPool},
    registry::LocalRegistry,
    types::{AgentInfo, IdentityInfo},
};
use crate::tools::ManagerCommand;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, warn};

/// Command handler loop - processes commands from agent tools
///
/// This runs in a separate task and handles:
/// - Listing agents (for `agent_info` tool)
/// - Spawning agents (for `agent_spawn` tool)  
/// - Broadcasting messages (for `agent_broadcast` tool)
pub async fn command_handler_loop(
    pool: Arc<RwLock<AgentPool>>,
    _registry: Arc<RwLock<LocalRegistry>>,
    mut rx: mpsc::Receiver<ManagerCommand>,
) {
    debug!("Agent manager command handler loop started");

    while let Some(cmd) = rx.recv().await {
        match cmd {
            ManagerCommand::ListAgents { respond_to } => {
                // Need to await while holding the lock due to lifetime issues
                let basic_list = pool.read().await.list().await;

                // Convert PoolAgentInfo to AgentInfo
                let agents: Vec<AgentInfo> = basic_list
                    .into_iter()
                    .map(|info| {
                        let did = info.did.clone();
                        AgentInfo {
                            did: info.did,
                            name: info.name,
                            state: info.state,
                            capabilities: vec![],
                            uptime_secs: info.uptime_secs,
                            identity_info: IdentityInfo {
                                did,
                                scope: "local".to_string(),
                                created_at: None,
                            },
                        }
                    })
                    .collect();

                if let Err(e) = respond_to.send(agents).await {
                    warn!("Failed to send agent list response: {}", e);
                }
            }

            ManagerCommand::Spawn { config, respond_to } => {
                // Note: Spawning requires access to the full manager state
                // For now, return an error - the agent should use Manager::spawn directly
                let _ = config;
                let result: anyhow::Result<AgentHandle> = Err(anyhow::anyhow!(
                    "agent_spawn via command not yet fully implemented. "
                ));
                if let Err(e) = respond_to.send(result).await {
                    warn!("Failed to send spawn response: {}", e);
                }
            }

            ManagerCommand::Broadcast {
                message,
                respond_to,
            } => {
                // Need to await while holding the lock due to lifetime issues
                let result = pool.read().await.broadcast(&message).await;

                if let Err(e) = respond_to.send(result).await {
                    warn!("Failed to send broadcast response: {}", e);
                }
            }

            ManagerCommand::SpawnSession {
                agent_did,
                peer,
                task,
                isolated,
                parent_session_key,
                timeout_seconds,
                respond_to,
            } => {
                // Find the agent by DID and spawn a session
                let result = spawn_session_for_agent(
                    pool.clone(),
                    &agent_did,
                    peer,
                    &task,
                    isolated,
                    &parent_session_key,
                    timeout_seconds,
                )
                .await;

                if let Err(e) = respond_to.send(result).await {
                    warn!("Failed to send spawn session response: {}", e);
                }
            }
        }
    }

    debug!("Agent manager command handler loop ended");
}

use crate::session::types::Peer;
use crate::tools::agent_management::SpawnSessionResult;

/// Spawn a session for an agent in the pool
async fn spawn_session_for_agent(
    pool: Arc<RwLock<AgentPool>>,
    agent_did: &str,
    peer: Peer,
    task: &str,
    isolated: bool,
    parent_session_key: &str,
    timeout_seconds: Option<u64>,
) -> anyhow::Result<SpawnSessionResult> {
    let pool_guard = pool.read().await;

    // Find agent by DID
    let agent = pool_guard
        .get_agent(agent_did)
        .await
        .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", agent_did))?;

    // Spawn the session using the agent's session router
    let spawn_ctx = agent
        .spawn_session(&peer, task, isolated, parent_session_key, timeout_seconds)
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

    Ok(SpawnSessionResult {
        spawn_id,
        session_key,
        parent_session_key: parent_session_key.to_string(),
        isolated: is_isolated,
        depth,
    })
}
