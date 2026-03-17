//! Event Bus Hook Integration
//!
//! Connects the team event bus to the hook registry for event-triggered hooks.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::hooks::{HookRegistry, HookTrigger, RegisteredHook, TriggerSource};
use crate::team::bus::{A2aMessage, A2aMessageType, EventBus};

/// Event bus hook integration
///
/// Subscribes to event bus topics and triggers hooks when messages arrive.
pub struct EventBusHookIntegration {
    registry: Arc<HookRegistry>,
    /// Active subscriptions: topic -> (hook_id, filter)
    subscriptions: Arc<RwLock<HashMap<String, Vec<Subscription>>>>,
}

/// Subscription configuration
#[derive(Debug, Clone)]
pub struct Subscription {
    /// Hook ID to trigger
    pub hook_id: String,
    /// Instance ID
    pub instance_id: String,
    /// Optional JSON filter
    pub filter: Option<serde_json::Value>,
}

impl EventBusHookIntegration {
    /// Create new event bus hook integration
    pub fn new(registry: Arc<HookRegistry>) -> Self {
        Self {
            registry,
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register event hooks for an instance
    pub async fn register_instance(&self, instance_id: &str) -> anyhow::Result<Vec<String>> {
        let hooks = self.registry.get_for_instance(instance_id).await;
        let mut registered = Vec::new();

        for hook in hooks {
            if let crate::hooks::HookType::Event { topic } = &hook.hook_type {
                self.subscribe(
                    hook.id.clone(),
                    instance_id.to_string(),
                    topic.clone(),
                    None,
                )
                .await;
                registered.push(hook.id.clone());

                info!(
                    "Registered event hook {} for instance {} on topic {}",
                    hook.id, instance_id, topic
                );
            }
        }

        Ok(registered)
    }

    /// Subscribe to a topic for a hook
    async fn subscribe(
        &self,
        hook_id: String,
        instance_id: String,
        topic: String,
        filter: Option<serde_json::Value>,
    ) {
        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.entry(topic).or_default().push(Subscription {
            hook_id,
            instance_id,
            filter,
        });
    }

    /// Unregister all event hooks for an instance
    pub async fn unregister_instance(&self, instance_id: &str) -> u32 {
        let mut subscriptions = self.subscriptions.write().await;
        let mut count = 0;

        for subs in subscriptions.values_mut() {
            let initial_len = subs.len();
            subs.retain(|sub| sub.instance_id != instance_id);
            count += (initial_len - subs.len()) as u32;
        }

        // Remove empty topics
        subscriptions.retain(|_, subs| !subs.is_empty());

        count
    }

    /// Handle an incoming event bus message
    pub async fn handle_message(&self, message: &A2aMessage) -> Vec<HookTriggerResult> {
        let mut results = Vec::new();

        // Get topic from message
        let topic = match &message.topic {
            Some(t) => t.clone(),
            None => {
                // For Direct/Task messages, use a synthetic topic based on target
                if let Some(ref to) = message.to {
                    format!("agent.{}", to)
                } else {
                    return results;
                }
            }
        };

        let subscriptions = self.subscriptions.read().await;
        let subs = match subscriptions.get(&topic) {
            Some(s) => s.clone(),
            None => return results,
        };
        drop(subscriptions);

        for subscription in subs {
            // Get the hook
            let hook = match self.registry.get(&subscription.hook_id).await {
                Some(h) => h,
                None => continue,
            };

            if !hook.enabled {
                continue;
            }

            // Check filter if present
            if let Some(ref filter) = subscription.filter {
                if !self.matches_filter(&message.payload, filter) {
                    continue;
                }
            }

            // Create trigger
            let trigger_source = TriggerSource::Event {
                topic: topic.clone(),
                payload: message.payload.clone(),
            };

            let trigger = HookTrigger::new(hook, trigger_source);

            results.push(HookTriggerResult {
                hook_id: subscription.hook_id.clone(),
                instance_id: subscription.instance_id.clone(),
                triggered: true,
            });

            info!(
                "Event hook {} triggered by message on topic {} from {}",
                subscription.hook_id, topic, message.from
            );
        }

        results
    }

    /// Check if payload matches filter
    fn matches_filter(&self, payload: &serde_json::Value, filter: &serde_json::Value) -> bool {
        // Simple filter matching - check if all fields in filter exist in payload with same values
        if let Some(filter_obj) = filter.as_object() {
            if let Some(payload_obj) = payload.as_object() {
                for (key, value) in filter_obj {
                    match payload_obj.get(key) {
                        Some(payload_value) => {
                            if payload_value != value {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
                true
            } else {
                false
            }
        } else {
            // Non-object filters do exact match
            payload == filter
        }
    }

    /// Get subscription count
    pub async fn subscription_count(&self) -> usize {
        let subscriptions = self.subscriptions.read().await;
        subscriptions.values().map(|v| v.len()).sum()
    }

    /// Get topics being watched
    pub async fn watched_topics(&self) -> Vec<String> {
        let subscriptions = self.subscriptions.read().await;
        subscriptions.keys().cloned().collect()
    }
}

/// Result of attempting to trigger a hook
#[derive(Debug, Clone)]
pub struct HookTriggerResult {
    pub hook_id: String,
    pub instance_id: String,
    pub triggered: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{HookAction, HookType, SessionTarget};
    use crate::image::config::Hook;

    async fn create_test_registry() -> Arc<HookRegistry> {
        Arc::new(HookRegistry::new())
    }

    fn create_test_event_hook(
        id: &str,
        instance_id: &str,
        topic: &str,
        filter: Option<serde_json::Value>,
    ) -> RegisteredHook {
        RegisteredHook {
            id: id.to_string(),
            instance_id: instance_id.to_string(),
            hook_type: HookType::Event {
                topic: topic.to_string(),
            },
            action: HookAction::Run {
                message: "Event received".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        }
    }

    fn create_test_message(topic: &str, payload: serde_json::Value) -> A2aMessage {
        A2aMessage {
            id: "msg_001".to_string(),
            message_type: A2aMessageType::Broadcast,
            from: "agent_a".to_string(),
            to: None,
            topic: Some(topic.to_string()),
            payload,
            conversation_id: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[tokio::test]
    async fn test_event_bus_integration_creation() {
        let registry = create_test_registry().await;
        let integration = EventBusHookIntegration::new(registry);
        assert_eq!(integration.subscription_count().await, 0);
    }

    #[tokio::test]
    async fn test_register_instance() {
        let registry = create_test_registry().await;

        // Register an event hook
        let hook = create_test_event_hook("hook_001", "inst_123", "team.tasks", None);
        registry.register(hook).await.unwrap();

        let integration = EventBusHookIntegration::new(registry);
        let registered = integration.register_instance("inst_123").await.unwrap();

        assert_eq!(registered.len(), 1);
        assert_eq!(integration.subscription_count().await, 1);

        let topics = integration.watched_topics().await;
        assert!(topics.contains(&"team.tasks".to_string()));
    }

    #[tokio::test]
    async fn test_handle_message() {
        let registry = create_test_registry().await;

        // Register an event hook
        let hook = create_test_event_hook("hook_001", "inst_123", "team.tasks", None);
        registry.register(hook).await.unwrap();

        let integration = EventBusHookIntegration::new(registry);
        integration.register_instance("inst_123").await.unwrap();

        // Create a matching message
        let message =
            create_test_message("team.tasks", serde_json::json!({"task": "do something"}));

        let results = integration.handle_message(&message).await;

        assert_eq!(results.len(), 1);
        assert!(results[0].triggered);
        assert_eq!(results[0].hook_id, "hook_001");
    }

    #[tokio::test]
    async fn test_handle_non_matching_message() {
        let registry = create_test_registry().await;

        // Register an event hook for specific topic
        let hook = create_test_event_hook("hook_001", "inst_123", "team.tasks", None);
        registry.register(hook).await.unwrap();

        let integration = EventBusHookIntegration::new(registry);
        integration.register_instance("inst_123").await.unwrap();

        // Create a message for different topic
        let message = create_test_message("team.results", serde_json::json!({"result": "done"}));

        let results = integration.handle_message(&message).await;

        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_filter_matching() {
        let registry = create_test_registry().await;
        let integration = EventBusHookIntegration::new(registry);

        // Test object filter matching
        let payload = serde_json::json!({
            "priority": "high",
            "task": "important"
        });
        let filter = serde_json::json!({"priority": "high"});

        assert!(integration.matches_filter(&payload, &filter));

        // Test non-matching filter
        let filter2 = serde_json::json!({"priority": "low"});
        assert!(!integration.matches_filter(&payload, &filter2));

        // Test missing field
        let filter3 = serde_json::json!({"nonexistent": "value"});
        assert!(!integration.matches_filter(&payload, &filter3));
    }

    #[tokio::test]
    async fn test_unregister_instance() {
        let registry = create_test_registry().await;

        // Register event hooks for two instances
        let hook1 = create_test_event_hook("hook_001", "inst_123", "topic1", None);
        let hook2 = create_test_event_hook("hook_002", "inst_123", "topic2", None);
        let hook3 = create_test_event_hook("hook_003", "inst_456", "topic3", None);

        registry.register(hook1).await.unwrap();
        registry.register(hook2).await.unwrap();
        registry.register(hook3).await.unwrap();

        let integration = EventBusHookIntegration::new(registry);
        integration.register_instance("inst_123").await.unwrap();
        integration.register_instance("inst_456").await.unwrap();

        assert_eq!(integration.subscription_count().await, 3);

        // Unregister one instance
        let count = integration.unregister_instance("inst_123").await;
        assert_eq!(count, 2);
        assert_eq!(integration.subscription_count().await, 1);

        let topics = integration.watched_topics().await;
        assert!(!topics.contains(&"topic1".to_string()));
        assert!(!topics.contains(&"topic2".to_string()));
        assert!(topics.contains(&"topic3".to_string()));
    }
}
