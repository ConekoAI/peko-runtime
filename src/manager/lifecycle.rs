//! Lifecycle Manager - Agent state transitions

use crate::engine::state::{AgentState, StateMachine};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Manages agent lifecycle states
pub struct LifecycleManager {
    /// State machines for each agent
    states: Arc<RwLock<HashMap<String, StateMachine>>>,
}

/// Simplified lifecycle states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleState {
    /// Created but not started
    Created,
    /// Starting up
    Starting,
    /// Running normally
    Running,
    /// Busy processing
    Busy,
    /// Stopping gracefully
    Stopping,
    /// Stopped
    Stopped,
    /// Error state
    Error,
}

impl LifecycleManager {
    /// Create new lifecycle manager
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new agent
    pub async fn register(&self, did: &str) -> Result<()> {
        let mut states = self.states.write().await;
        states.insert(did.to_string(), StateMachine::new(AgentState::Idle));
        debug!("Registered agent lifecycle: {}", did);
        Ok(())
    }

    /// Start an agent
    pub async fn start(&self, did: &str) -> Result<()> {
        let mut states = self.states.write().await;

        if let Some(sm) = states.get_mut(did) {
            match sm.transition_with_reason(AgentState::Running, "Starting agent") {
                Ok(_) => {
                    info!("Agent {} started", did);
                    Ok(())
                }
                Err(e) => {
                    warn!("Failed to start agent {}: {}", did, e);
                    Err(e)
                }
            }
        } else {
            Err(anyhow!("Agent not registered: {}", did))
        }
    }

    /// Mark agent as busy
    pub async fn set_busy(&self, did: &str) -> Result<()> {
        let mut states = self.states.write().await;

        if let Some(sm) = states.get_mut(did) {
            sm.transition(AgentState::Running)?;
        }
        Ok(())
    }

    /// Mark agent as idle
    pub async fn set_idle(&self, did: &str) -> Result<()> {
        let mut states = self.states.write().await;

        if let Some(sm) = states.get_mut(did) {
            sm.transition(AgentState::Idle)?;
        }
        Ok(())
    }

    /// Stop an agent
    pub async fn stop(&self, did: &str) -> Result<()> {
        let mut states = self.states.write().await;

        if let Some(sm) = states.get_mut(did) {
            match sm.transition_with_reason(AgentState::Stopping, "Stopping agent") {
                Ok(_) => {
                    // Then to idle (stopped)
                    let _ = sm.transition(AgentState::Idle);
                    info!("Agent {} stopped", did);
                    Ok(())
                }
                Err(e) => {
                    warn!("Failed to stop agent {}: {}", did, e);
                    Err(e)
                }
            }
        } else {
            Err(anyhow!("Agent not registered: {}", did))
        }
    }

    /// Mark agent as error
    pub async fn set_error(&self, did: &str, error: &str) -> Result<()> {
        let mut states = self.states.write().await;

        if let Some(sm) = states.get_mut(did) {
            sm.transition_with_reason(AgentState::Error, error)?;
            error!("Agent {} entered error state: {}", did, error);
        }
        Ok(())
    }

    /// Get current state
    pub async fn get_state(&self, did: &str) -> Option<AgentState> {
        let states = self.states.read().await;
        states.get(did).map(|sm| sm.current())
    }

    /// Check if agent is running
    pub async fn is_running(&self, did: &str) -> bool {
        matches!(
            self.get_state(did).await,
            Some(AgentState::Running | AgentState::Idle)
        )
    }

    /// Unregister an agent
    pub async fn unregister(&self, did: &str) {
        let mut states = self.states.write().await;
        states.remove(did);
        debug!("Unregistered agent lifecycle: {}", did);
    }
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

/// A lifecycle transition record
#[derive(Debug, Clone)]
pub struct LifecycleTransition {
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// From state
    pub from: LifecycleState,
    /// To state
    pub to: LifecycleState,
    /// Reason
    pub reason: Option<String>,
}
