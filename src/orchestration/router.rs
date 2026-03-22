//! Event Router for orchestration layer
//!
//! Routes system events to appropriate agents based on registered handlers.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::agent::stateless_service::ExecutionRequest;
use crate::agent::StatelessAgentManager;
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
    agent_manager: Arc<RwLock<StatelessAgentManager>>,
    /// Event history for audit/debugging
    event_history: RwLock<Vec<(chrono::DateTime<chrono::Utc>, SystemEvent)>>,
    /// Maximum history size
    max_history: usize,
}

impl EventRouter {
    /// Create a new event router
    pub fn new(agent_manager: Arc<RwLock<StatelessAgentManager>>) -> Self {
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
        handlers.get(event_type).map_or(0, std::vec::Vec::len)
    }

    /// Execute invoke action (helper to avoid recursion)
    async fn execute_invoke(&self, agent_id: String, prompt: String) -> anyhow::Result<()> {
        info!("Invoking agent {} with prompt", agent_id);

        // Get the agent manager
        let manager = self.agent_manager.read().await;

        // Check if agent exists
        let agent_exists = manager.exists(&agent_id, None).await?;

        if !agent_exists {
            warn!("Agent {} not found for invocation", agent_id);
            return Err(anyhow::anyhow!("Agent {agent_id} not found"));
        }

        // Create execution request
        let session_id = format!("router_{}", uuid::Uuid::new_v4());
        let request = ExecutionRequest::new(&agent_id, &session_id, &prompt);

        // Execute the prompt on the agent
        match manager.execute(request).await {
            Ok(result) => {
                info!(
                    "Agent {} execution completed: success={}, duration={}ms",
                    agent_id, result.success, result.duration_ms
                );
            }
            Err(e) => {
                error!("Agent {} execution failed: {}", agent_id, e);
                return Err(e);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::events::{FileChangeType, SystemEvent};
    use std::collections::HashMap;

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

    #[test]
    fn test_agent_action_queue() {
        let event = SystemEvent::Internal {
            event_type: "test".to_string(),
            source: "test".to_string(),
            payload: serde_json::json!({}),
            timestamp: chrono::Utc::now(),
        };

        let action = AgentAction::Queue {
            queue_name: "test-queue".to_string(),
            event: event.clone(),
        };

        match &action {
            AgentAction::Queue { queue_name, .. } => {
                assert_eq!(queue_name, "test-queue");
            }
            _ => panic!("Wrong action type"),
        }
    }

    #[tokio::test]
    async fn test_handler_registration_and_routing() {
        // Create a real StatelessAgentManager for integration testing
        let (agent_manager, _events) = StatelessAgentManager::new()
            .await
            .expect("Failed to create StatelessAgentManager");
        let agent_manager = Arc::new(RwLock::new(agent_manager));

        let router = EventRouter::new(agent_manager);

        // Register a handler for webhook events
        router
            .register_handler("webhook", |event| {
                if let SystemEvent::Webhook { source, .. } = event {
                    Some(AgentAction::Invoke {
                        agent_id: format!("{}-handler", source),
                        prompt: "Process webhook".to_string(),
                        context: HashMap::new(),
                    })
                } else {
                    None
                }
            })
            .await;

        // Verify handlers are registered
        let handler_types = router.get_handler_types().await;
        assert!(handler_types.contains(&"webhook".to_string()));

        // Verify handler counts
        assert_eq!(router.get_handler_count("webhook").await, 1);
        assert_eq!(router.get_handler_count("unknown").await, 0);

        // Route a webhook event (agent won't exist, but we test the routing path)
        let webhook_event = SystemEvent::Webhook {
            source: "github".to_string(),
            route: "/webhook/github".to_string(),
            payload: serde_json::json!({"action": "push"}),
            headers: HashMap::new(),
            timestamp: chrono::Utc::now(),
        };

        // This will try to invoke a non-existent agent, but it tests the routing
        let result = router.route_event(webhook_event).await;
        // Should succeed in routing even if agent doesn't exist
        assert!(result.is_ok());

        // Verify event was logged to history
        let history = router.get_history(10).await;
        assert!(!history.is_empty());

        // Route a file event
        let file_event = SystemEvent::File {
            path: std::path::PathBuf::from("/tmp/test.txt"),
            change_type: FileChangeType::Modified,
            timestamp: chrono::Utc::now(),
        };

        let result = router.route_event(file_event).await;
        assert!(result.is_ok());

        // Verify both events in history
        let history = router.get_history(10).await;
        assert_eq!(history.len(), 2);
    }

    #[tokio::test]
    async fn test_event_routing_no_handler() {
        let (agent_manager, _events) = StatelessAgentManager::new()
            .await
            .expect("Failed to create StatelessAgentManager");
        let agent_manager = Arc::new(RwLock::new(agent_manager));

        let router = EventRouter::new(agent_manager);

        // Route an event with no handler registered
        let event = SystemEvent::Internal {
            event_type: "unknown".to_string(),
            source: "test".to_string(),
            payload: serde_json::json!({}),
            timestamp: chrono::Utc::now(),
        };

        // Should succeed but do nothing
        let result = router.route_event(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_multiple_handlers_for_event() {
        let (agent_manager, _events) = StatelessAgentManager::new()
            .await
            .expect("Failed to create StatelessAgentManager");
        let agent_manager = Arc::new(RwLock::new(agent_manager));

        let router = EventRouter::new(agent_manager);

        // Register multiple handlers for the same event type
        router
            .register_handler("timer", |_event| {
                Some(AgentAction::Invoke {
                    agent_id: "handler1".to_string(),
                    prompt: "First handler".to_string(),
                    context: HashMap::new(),
                })
            })
            .await;

        router
            .register_handler("timer", |_event| {
                Some(AgentAction::Invoke {
                    agent_id: "handler2".to_string(),
                    prompt: "Second handler".to_string(),
                    context: HashMap::new(),
                })
            })
            .await;

        assert_eq!(router.get_handler_count("timer").await, 2);

        // Route a timer event
        let event = SystemEvent::Timer {
            schedule_id: "schedule-1".to_string(),
            task_id: "task-1".to_string(),
            fired_at: chrono::Utc::now(),
        };

        let result = router.route_event(event).await;
        assert!(result.is_ok());
    }
}
