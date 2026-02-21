//! Agent Pool - Manages running agent instances

use crate::agent::Agent;
use crate::engine::state::AgentState;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// Pool configuration
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum agents in pool
    pub max_agents: usize,
    /// Auto-restart crashed agents
    pub auto_restart: bool,
    /// Restart delay (seconds)
    pub restart_delay_secs: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_agents: 100,
            auto_restart: false,
            restart_delay_secs: 5,
        }
    }
}

/// Agent pool - manages running agents
pub struct AgentPool {
    /// Configuration
    config: PoolConfig,
    /// Agents by DID
    agents: HashMap<String, AgentEntry>,
    /// Message channels to agents
    channels: HashMap<String, mpsc::Sender<PoolMessage>>,
}

/// Entry in the pool
struct AgentEntry {
    /// The agent
    agent: Arc<Agent>,
    /// When added to pool
    started_at: Instant,
    /// Last activity
    last_activity: Instant,
}

/// Messages sent to agents in pool
#[derive(Debug)]
pub enum PoolMessage {
    /// Execute a task
    Execute {
        prompt: String,
        respond_to: mpsc::Sender<Result<String>>,
    },
    /// Stop the agent
    Stop,
    /// Ping (health check)
    Ping,
}

/// Handle to an agent in the pool
#[derive(Debug, Clone)]
pub struct AgentHandle {
    /// Agent DID
    did: String,
    /// Agent name
    name: String,
    /// Channel to communicate
    channel: mpsc::Sender<PoolMessage>,
}

impl AgentHandle {
    /// Get DID
    pub fn did(&self) -> &str {
        &self.did
    }

    /// Get name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Start the agent
    pub async fn start(&self) -> Result<()> {
        // In a real implementation, this would spawn the agent task
        info!("Starting agent: {}", self.did);
        Ok(())
    }

    /// Stop the agent
    pub async fn stop(&self) -> Result<()> {
        self.channel
            .send(PoolMessage::Stop)
            .await
            .map_err(|e| anyhow!("Failed to stop agent: {}", e))
    }

    /// Execute a prompt
    pub async fn execute(&self, prompt: &str) -> Result<String> {
        let (tx, mut rx) = mpsc::channel(1);

        self.channel
            .send(PoolMessage::Execute {
                prompt: prompt.to_string(),
                respond_to: tx,
            })
            .await
            .map_err(|e| anyhow!("Failed to send execute: {}", e))?;

        rx.recv()
            .await
            .ok_or_else(|| anyhow!("No response from agent"))?
    }

    /// Send a message
    pub async fn send(&self, _message: &str) -> Result<()> {
        // For now, just log
        debug!("Message to {}: {}", self.did, _message);
        Ok(())
    }

    /// Check if agent is alive
    pub async fn is_alive(&self) -> bool {
        // Send ping and check response
        self.channel.send(PoolMessage::Ping).await.is_ok()
    }
}

/// Pool agent info - basic info from pool
#[derive(Debug, Clone)]
pub struct PoolAgentInfo {
    /// Agent DID
    pub did: String,
    /// Agent name
    pub name: String,
    /// Current state
    pub state: AgentState,
    /// Uptime (seconds)
    pub uptime_secs: u64,
}

impl AgentPool {
    /// Create new pool
    pub fn new() -> Self {
        Self {
            config: PoolConfig::default(),
            agents: HashMap::new(),
            channels: HashMap::new(),
        }
    }

    /// Create with config
    pub fn with_config(config: PoolConfig) -> Self {
        Self {
            config,
            agents: HashMap::new(),
            channels: HashMap::new(),
        }
    }

    /// Add agent to pool
    pub async fn add(&mut self, agent: Arc<Agent>) -> Result<AgentHandle> {
        let did = agent.did().to_string();
        let name = agent.name().to_string();

        if self.agents.len() >= self.config.max_agents {
            return Err(anyhow!("Pool full (max {} agents)", self.config.max_agents));
        }

        if self.agents.contains_key(&did) {
            return Err(anyhow!("Agent already in pool: {}", did));
        }

        // Create channel
        let (tx, rx) = mpsc::channel(100);

        // Store agent
        self.agents.insert(
            did.clone(),
            AgentEntry {
                agent,
                started_at: Instant::now(),
                last_activity: Instant::now(),
            },
        );
        self.channels.insert(did.clone(), tx.clone());

        // Spawn agent task (simplified)
        // In full implementation, this would spawn a tokio task
        // that runs the agent loop

        info!(
            "Added agent to pool: {} (total: {})",
            did,
            self.agents.len()
        );

        Ok(AgentHandle {
            did,
            name,
            channel: tx,
        })
    }

    /// Get agent by DID
    pub async fn get(&self, did: &str) -> Option<AgentHandle> {
        self.agents.get(did).map(|entry| AgentHandle {
            did: did.to_string(),
            name: entry.agent.name().to_string(),
            channel: self.channels.get(did).cloned().unwrap(),
        })
    }

    /// Get agent by name
    pub async fn get_by_name(&self, name: &str) -> Option<AgentHandle> {
        for (did, entry) in &self.agents {
            if entry.agent.name() == name {
                return Some(AgentHandle {
                    did: did.clone(),
                    name: name.to_string(),
                    channel: self.channels.get(did).cloned().unwrap(),
                });
            }
        }
        None
    }

    /// Stop an agent
    pub async fn stop(&mut self, did: &str) -> Result<()> {
        if let Some(channel) = self.channels.get(did) {
            channel
                .send(PoolMessage::Stop)
                .await
                .map_err(|e| anyhow!("Failed to stop: {}", e))?;
        }

        self.agents.remove(did);
        self.channels.remove(did);

        info!("Removed agent from pool: {}", did);
        Ok(())
    }

    /// List all agents
    pub async fn list(&self) -> Vec<PoolAgentInfo> {
        self.agents
            .iter()
            .map(|(did, entry)| {
                PoolAgentInfo {
                    did: did.clone(),
                    name: entry.agent.name().to_string(),
                    state: AgentState::Idle, // Would get from lifecycle
                    uptime_secs: entry.started_at.elapsed().as_secs(),
                }
            })
            .collect()
    }

    /// Get agent states
    pub async fn get_states(&self) -> HashMap<String, String> {
        self.agents
            .iter()
            .map(|(did, _)| (did.clone(), "idle".to_string()))
            .collect()
    }

    /// Broadcast message to all agents
    pub async fn broadcast(&self, message: &str) -> Result<()> {
        for (did, channel) in &self.channels {
            if let Err(e) = channel.send(PoolMessage::Ping).await {
                warn!("Failed to broadcast to {}: {}", did, e);
            }
        }
        debug!("Broadcast to {} agents: {}", self.channels.len(), message);
        Ok(())
    }

    /// Get pool size
    pub fn size(&self) -> usize {
        self.agents.len()
    }

    /// Check if pool is full
    pub fn is_full(&self) -> bool {
        self.agents.len() >= self.config.max_agents
    }
}

impl Default for AgentPool {
    fn default() -> Self {
        Self::new()
    }
}
