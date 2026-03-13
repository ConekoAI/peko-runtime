//! Event Router for orchestration layer
//!
//! Routes system events to appropriate agents based on registered handlers.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::agent::AgentManager;
use crate::orchestration::events::SystemEvent;

/// Handler function type for event processing
type EventHandler = Arc<dyn Fn(&SystemEvent) -> Option<AgentAction> + Send + Sync>;

/// Action to take when an event is handled
#[derive(Debug, Clone)]
pub enum AgentAction {
    /// Invoke an agent with a prompt
    Invoke {
        agent_id: String,
        prompt: String,
        context: HashMap<String, serde_json::Value>,
    },
    /// Broadcast to multiple agents
    Broadcast {
        agent_ids: Vec<String>,
        message: String,
    },
    /// Queue for later processing
    Queue {
        queue_name: String,
        event: SystemEvent,
    },
}

/// Event router that dispatches events to appropriate handlers
pub struct EventRouter {
    /// Event type -> handlers mapping
    handlers: RwLock<HashMap<String, Vec<EventHandler>>>,
    /// Agent manager for invoking agents
    agent_manager: Arc<RwLock<AgentManager>>,
    /// Event history for audit/debugging
    event_history: RwLock<Vec<(chrono::DateTime<chrono::Utc>, SystemEvent)>>,
    /// Maximum history size
    max_history: usize,
}

impl EventRouter {
    /// Create a new event router
    pub fn new(agent_manager: Arc<RwLock<AgentManager>>) -> Self {
        Self {
            handlers: RwLock::new(HashMap::new()),
            agent_manager,
            event_history: RwLock::new(Vec::new()),
            max_history: 1000,
        }
    }

    /// Register a handler for a specific event type
    pub async fn register_handler<F>(&self, event_type: &str, handler: F)
    where
        F: Fn(&SystemEvent) -> Option<AgentAction> + Send + Sync + 'static,
    {
        let mut handlers = self.handlers.write().await;
        handlers
            .entry(event_type.to_string())
            .or_insert_with(Vec::new)
            .push(Arc::new(handler));

        info!("Registered handler for event type: {}", event_type);
    }

    /// Route an event to appropriate handlers
    pub async fn route_event(&self, event: SystemEvent) -> anyhow::Result<()> {
        let event_type = event.event_type().to_string();

        // Log event to history
        self.log_event(&event).await;

        info!("Routing event: type={}", event_type);

        // Get handlers for this event type
        let handlers = {
            let handlers = self.handlers.read().await;
            handlers.get(&event_type).cloned()
        };

        if let Some(handlers) = handlers {
            for handler in handlers {
                match handler(&event) {
                    Some(action) => {
                        if let Err(e) = self.execute_action(action).await {
                            error!("Failed to execute action: {}", e);
                        }
                    }
                    None => {
                        debug!("Handler returned no action for event");
                    }
                }
            }
        } else {
            warn!("No handlers registered for event type: {}", event_type);
        }

        Ok(())
    }

    /// Execute an agent action
    async fn execute_action(&self, action: AgentAction) -> anyhow::Result<()> {
        match action {
            AgentAction::Invoke {
                agent_id,
                prompt,
                context: _context,
            } => self.execute_invoke(agent_id, prompt).await,
            AgentAction::Broadcast { agent_ids, message } => {
                info!("Broadcasting to {} agents", agent_ids.len());
                for agent_id in agent_ids {
                    if let Err(e) = self.execute_invoke(agent_id, message.clone()).await {
                        error!("Failed to broadcast to agent: {}", e);
                    }
                }
                Ok(())
            }
            AgentAction::Queue {
                queue_name,
                event: _,
            } => {
                info!("Queueing event to {}", queue_name);
                // TODO: Implement queueing
                Ok(())
            }
        }
    }

    /// Log event to history
    async fn log_event(&self, event: &SystemEvent) {
        let mut history = self.event_history.write().await;
        history.push((chrono::Utc::now(), event.clone()));

        // Trim history if needed
        if history.len() > self.max_history {
            history.remove(0);
        }
    }

    /// Get recent event history
    pub async fn get_history(
        &self,
        limit: usize,
    ) -> Vec<(chrono::DateTime<chrono::Utc>, SystemEvent)> {
        let history = self.event_history.read().await;
        history.iter().rev().take(limit).cloned().collect()
    }

    /// Get registered handler types
    pub async fn get_handler_types(&self) -> Vec<String> {
        let handlers = self.handlers.read().await;
        handlers.keys().cloned().collect()
    }

    /// Get handler count for a specific type
    pub async fn get_handler_count(&self, event_type: &str) -> usize {
        let handlers = self.handlers.read().await;
        handlers.get(event_type).map(|h| h.len()).unwrap_or(0)
    }

    /// Execute invoke action (helper to avoid recursion)
    async fn execute_invoke(&self, agent_id: String, prompt: String) -> anyhow::Result<()> {
        info!("Invoking agent {} with prompt", agent_id);

        // Get the agent from the pool
        let manager = self.agent_manager.read().await;

        // Try to find by name first, then by DID
        let agent_handle = if let Some(agent) = manager.get_by_name(&agent_id).await {
            Some(agent)
        } else {
            manager.get(&agent_id).await
        };

        if let Some(_agent) = agent_handle {
            // TODO: Implement actual agent invocation
            // For now, just log that we would invoke
            info!(
                "Would invoke agent {} with prompt: {}",
                agent_id,
                prompt.chars().take(100).collect::<String>()
            );

            // Future implementation:
            // 1. Get or create session context for the agent
            // 2. Add the prompt as a user message
            // 3. Execute the agent loop
            // 4. Return result
        } else {
            warn!("Agent {} not found for invocation", agent_id);
            return Err(anyhow::anyhow!("Agent {} not found", agent_id));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Mock AgentManager for testing
    fn mock_agent_manager() -> Arc<RwLock<AgentManager>> {
        // This would need a real AgentManager in integration tests
        // For unit tests, we just verify the router structure
        unimplemented!("Mock AgentManager not implemented")
    }

    #[tokio::test]
    async fn test_handler_registration() {
        // This test would need a mock AgentManager
        // Skipping for now as it requires more infrastructure
    }

    #[test]
    fn test_agent_action_debug() {
        let action = AgentAction::Invoke {
            agent_id: "test-agent".to_string(),
            prompt: "Hello".to_string(),
            context: HashMap::new(),
        };
        assert!(format!("{:?}", action).contains("test-agent"));
    }

    #[test]
    fn test_agent_action_clone() {
        let action = AgentAction::Invoke {
            agent_id: "test-agent".to_string(),
            prompt: "Hello".to_string(),
            context: HashMap::new(),
        };
        let cloned = action.clone();
        match cloned {
            AgentAction::Invoke { agent_id, .. } => {
                assert_eq!(agent_id, "test-agent");
            }
            _ => panic!("Wrong action type"),
        }
    }
}
