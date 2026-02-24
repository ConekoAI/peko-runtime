//! Lifecycle Manager - Simple agent state tracking

use crate::engine::state::StateMachine;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Simple lifecycle manager - just tracks if agent is idle or busy
pub struct LifecycleManager {
    /// State machines for each agent
    states: Arc<RwLock<HashMap<String, StateMachine>>>,
}

impl LifecycleManager {
    /// Create new lifecycle manager
    #[must_use] 
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new agent (starts in Idle)
    pub async fn register(&self,
        did: &str,
    ) -> Result<()> {
        let mut states = self.states.write().await;
        states.insert(did.to_string(), StateMachine::new());
        debug!("Registered agent: {}", did);
        Ok(())
    }

    /// Start an agent (mark as idle)
    pub async fn start(&self,
        did: &str,
    ) -> Result<()> {
        let states = self.states.read().await;
        
        if let Some(sm) = states.get(did) {
            sm.set_idle();
            info!("Agent {} started", did);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Agent not registered: {did}"))
        }
    }

    /// Mark agent as busy
    pub async fn set_busy(&self,
        did: &str,
    ) -> Result<()> {
        let states = self.states.read().await;
        
        if let Some(sm) = states.get(did) {
            sm.set_busy();
        }
        Ok(())
    }

    /// Mark agent as idle
    pub async fn set_idle(&self,
        did: &str,
    ) -> Result<()> {
        let states = self.states.read().await;
        
        if let Some(sm) = states.get(did) {
            sm.set_idle();
        }
        Ok(())
    }

    /// Stop an agent (mark as idle)
    pub async fn stop(&self,
        did: &str,
    ) -> Result<()> {
        let states = self.states.read().await;
        
        if let Some(sm) = states.get(did) {
            sm.set_idle();
            info!("Agent {} stopped", did);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Agent not registered: {did}"))
        }
    }

    /// Check if agent is idle
    pub async fn is_idle(&self,
        did: &str,
    ) -> bool {
        let states = self.states.read().await;
        states.get(did).is_some_and(|sm| sm.current().is_idle())
    }

    /// Check if agent is busy
    pub async fn is_busy(&self,
        did: &str,
    ) -> bool {
        let states = self.states.read().await;
        states.get(did).is_some_and(|sm| sm.current().is_busy())
    }

    /// Try to acquire agent (Idle -> Busy)
    /// Returns true if successful
    pub async fn try_acquire(&self,
        did: &str,
    ) -> bool {
        let states = self.states.read().await;
        states.get(did).is_some_and(super::super::engine::state::StateMachine::try_acquire)
    }

    /// Unregister an agent
    pub async fn unregister(&self,
        did: &str,
    ) {
        let mut states = self.states.write().await;
        states.remove(did);
        debug!("Unregistered agent: {}", did);
    }
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}
