//! Agent Manager - Unified agent lifecycle and coordination
//!
//! Provides decentralized agent management:
//! - Agent lifecycle (spawn, start, stop, restart)
//! - Agent pool management
//! - Discovery (who's available, what can they do)
//! - Context sharing (agents know about each other)
//!
//! Design principle: Agents coordinate themselves. The manager just provides
//! the context and messaging infrastructure.

pub mod context;
pub mod lifecycle;
pub mod pool;
pub mod registry;

pub use context::{AgentContext, AgentRegistryView, CapabilityIndex};
pub use lifecycle::{LifecycleManager, LifecycleState, LifecycleTransition};
pub use pool::{AgentHandle, AgentPool, PoolConfig};
pub use registry::{LocalRegistry, Registry, RegistryEvent};

use crate::agent::Agent;
use crate::engine::state::AgentState;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

/// Unified agent manager
pub struct AgentManager {
    /// Agent pool (running agents)
    pool: Arc<RwLock<AgentPool>>,
    /// Local registry (agent metadata)
    registry: Arc<RwLock<LocalRegistry>>,
    /// Lifecycle manager
    lifecycle: LifecycleManager,
    /// Event channel
    events: mpsc::Sender<ManagerEvent>,
}

/// Events emitted by the manager
#[derive(Debug, Clone)]
pub enum ManagerEvent {
    /// Agent spawned
    AgentSpawned { did: String, name: String },
    /// Agent started
    AgentStarted { did: String },
    /// Agent stopped
    AgentStopped { did: String },
    /// Agent crashed
    AgentCrashed { did: String, error: String },
    /// Agent registered (discovered)
    AgentDiscovered { did: String, capabilities: Vec<String> },
    /// Context updated
    ContextUpdated { did: String },
}

impl AgentManager {
    /// Create a new agent manager
    pub async fn new() -> Result<(Self, mpsc::Receiver<ManagerEvent>)> {
        let (events_tx, events_rx) = mpsc::channel(100);

        let pool = Arc::new(RwLock::new(AgentPool::new()));
        let registry = Arc::new(RwLock::new(LocalRegistry::new()));
        let lifecycle = LifecycleManager::new();

        Ok((
            Self {
                pool,
                registry,
                lifecycle,
                events: events_tx,
            },
            events_rx,
        ))
    }

    /// Spawn a new agent (create + start)
    pub async fn spawn(
        &self,
        config: crate::types::agent::AgentConfig,
    ) -> Result<AgentHandle> {
        info!("Spawning agent: {}", config.name);

        // Create agent
        let agent = Agent::new(config).await?;
        let did = agent.did().to_string();
        let name = agent.name().to_string();

        // Register in registry
        {
            let mut reg = self.registry.write().await;
            reg.register(&did, &name).await?;
        }

        // Add to pool
        let handle = {
            let mut pool = self.pool.write().await;
            pool.add(Arc::new(agent)).await?
        };

        // Start the agent
        self.lifecycle.start(&did).await?;
        handle.start().await?;

        // Emit event
        let _ = self.events.send(ManagerEvent::AgentSpawned { did, name }).await;

        Ok(handle)
    }

    /// Stop an agent
    pub async fn stop(&self, did: &str) -> Result<()> {
        info!("Stopping agent: {}", did);

        self.lifecycle.stop(did).await?;

        {
            let mut pool = self.pool.write().await;
            pool.stop(did).await?;
        }

        let _ = self.events.send(ManagerEvent::AgentStopped {
            did: did.to_string(),
        }).await;

        Ok(())
    }

    /// Get agent by DID
    pub async fn get(&self, did: &str) -> Option<AgentHandle> {
        let pool = self.pool.read().await;
        pool.get(did).await
    }

    /// Get agent by name
    pub async fn get_by_name(&self, name: &str) -> Option<AgentHandle> {
        let pool = self.pool.read().await;
        pool.get_by_name(name).await
    }

    /// List all agents
    pub async fn list(&self) -> Vec<AgentInfo> {
        let pool = self.pool.read().await;
        pool.list().await
    }

    /// Get agents by capability
    pub async fn find_by_capability(&self,
        capability: &str,
    ) -> Vec<AgentHandle> {
        let reg = self.registry.read().await;
        let dids = reg.find_by_capability(capability).await;
        drop(reg);

        let mut handles = vec![];
        for did in dids {
            if let Some(handle) = self.get(&did).await {
                handles.push(handle);
            }
        }
        handles
    }

    /// Get context for an agent (what it knows about other agents)
    pub async fn get_context(&self,
        did: &str,
    ) -> Result<AgentContext> {
        let reg = self.registry.read().await;
        let view = reg.get_view(did)?;

        let pool = self.pool.read().await;
        let states = pool.get_states().await;

        Ok(AgentContext {
            self_did: did.to_string(),
            registry_view: view,
            agent_states: states,
        })
    }

    /// Broadcast message to all agents
    pub async fn broadcast(
        &self,
        message: &str,
    ) -> Result<()> {
        let pool = self.pool.read().await;
        pool.broadcast(message).await
    }

    /// Send message to specific agent
    pub async fn send(
        &self,
        target_did: &str,
        message: &str,
    ) -> Result<()> {
        if let Some(handle) = self.get(target_did).await {
            handle.send(message).await
        } else {
            Err(anyhow::anyhow!("Agent not found: {}", target_did))
        }
    }

    /// Stop all agents
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down agent manager");

        let pool = self.pool.read().await;
        let agents = pool.list().await;
        drop(pool);

        for agent in agents {
            if let Err(e) = self.stop(&agent.did).await {
                warn!("Error stopping agent {}: {}", agent.did, e);
            }
        }

        info!("Agent manager shutdown complete");
        Ok(())
    }
}

/// Information about an agent
#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// Agent DID
    pub did: String,
    /// Agent name
    pub name: String,
    /// Current state
    pub state: AgentState,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Uptime (seconds)
    pub uptime_secs: u64,
}
