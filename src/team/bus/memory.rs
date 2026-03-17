//! In-memory event bus backend
//!
//! Single-process implementation with agent inboxes.
//! All messages are delivered immediately to registered agents.

use super::{A2aMessage, A2aMessageType, AgentId, DeliveryResult, EventBus, MessageId, Topic};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

/// In-memory event bus implementation
pub struct InMemoryBus {
    /// Registered agent inboxes
    agents: Arc<RwLock<HashMap<AgentId, mpsc::UnboundedSender<A2aMessage>>>>,

    /// Topic subscriptions: topic -> set of agent IDs
    subscriptions: Arc<RwLock<HashMap<Topic, HashSet<AgentId>>>>,
}

impl InMemoryBus {
    /// Create a new in-memory bus
    pub fn new() -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Generate a unique message ID
    fn generate_message_id() -> MessageId {
        format!("msg_{}", Uuid::new_v4().simple())
    }

    /// Get current timestamp in ISO 8601 format
    fn now() -> String {
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    /// Send a message to a specific agent
    async fn deliver_to_agent(&self, agent_id: &AgentId, message: A2aMessage) -> DeliveryResult {
        let agents = self.agents.read().await;

        match agents.get(agent_id) {
            Some(inbox) => {
                // Clone the sender to avoid holding the lock during send
                let inbox = inbox.clone();
                drop(agents);

                match inbox.send(message) {
                    Ok(_) => DeliveryResult::Delivered,
                    Err(_) => {
                        // Channel closed, agent disconnected
                        DeliveryResult::AgentNotFound(agent_id.clone())
                    }
                }
            }
            None => DeliveryResult::AgentNotFound(agent_id.clone()),
        }
    }

    /// Internal helper to send a direct message
    async fn do_send_direct(
        &self,
        from: AgentId,
        to: AgentId,
        message_type: A2aMessageType,
        payload: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<DeliveryResult> {
        let message = A2aMessage {
            id: Self::generate_message_id(),
            message_type,
            from,
            to: Some(to.clone()),
            topic: None,
            payload,
            conversation_id,
            timestamp: Self::now(),
        };

        Ok(self.deliver_to_agent(&to, message).await)
    }
}

impl Default for InMemoryBus {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventBus for InMemoryBus {
    async fn send_direct(
        &self,
        from: AgentId,
        to: AgentId,
        payload: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<DeliveryResult> {
        self.do_send_direct(from, to, A2aMessageType::Direct, payload, conversation_id)
            .await
    }

    async fn broadcast(
        &self,
        from: AgentId,
        payload: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<Vec<DeliveryResult>> {
        let agents = self.agents.read().await;
        let agent_ids: Vec<AgentId> = agents
            .keys()
            .filter(|id| **id != from) // Don't send to self
            .cloned()
            .collect();
        drop(agents);

        let mut results = Vec::new();

        for agent_id in agent_ids {
            let message = A2aMessage {
                id: Self::generate_message_id(),
                message_type: A2aMessageType::Broadcast,
                from: from.clone(),
                to: Some(agent_id.clone()),
                topic: None,
                payload: payload.clone(),
                conversation_id: conversation_id.clone(),
                timestamp: Self::now(),
            };

            let result = self.deliver_to_agent(&agent_id, message).await;
            results.push(result);
        }

        Ok(results)
    }

    async fn send_task(
        &self,
        from: AgentId,
        to: AgentId,
        task: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<DeliveryResult> {
        self.do_send_direct(from, to, A2aMessageType::Task, task, conversation_id)
            .await
    }

    async fn send_task_result(
        &self,
        from: AgentId,
        to: AgentId,
        result: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<DeliveryResult> {
        self.do_send_direct(
            from,
            to,
            A2aMessageType::TaskResult,
            result,
            conversation_id,
        )
        .await
    }

    async fn subscribe(&self, agent_id: AgentId, topic: Topic) -> anyhow::Result<()> {
        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.entry(topic).or_default().insert(agent_id);
        Ok(())
    }

    async fn unsubscribe(&self, agent_id: AgentId, topic: Topic) -> anyhow::Result<()> {
        let mut subscriptions = self.subscriptions.write().await;
        if let Some(subscribers) = subscriptions.get_mut(&topic) {
            subscribers.remove(&agent_id);
            if subscribers.is_empty() {
                subscriptions.remove(&topic);
            }
        }
        Ok(())
    }

    async fn publish(
        &self,
        from: AgentId,
        topic: Topic,
        payload: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<Vec<DeliveryResult>> {
        let subscribers = {
            let subscriptions = self.subscriptions.read().await;
            subscriptions
                .get(&topic)
                .map(|set| set.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };

        if subscribers.is_empty() {
            return Ok(vec![DeliveryResult::TopicNotFound(topic)]);
        }

        let mut results = Vec::new();

        for agent_id in subscribers {
            if agent_id == from {
                continue; // Don't send to self
            }

            let message = A2aMessage {
                id: Self::generate_message_id(),
                message_type: A2aMessageType::Broadcast,
                from: from.clone(),
                to: Some(agent_id.clone()),
                topic: Some(topic.clone()),
                payload: payload.clone(),
                conversation_id: conversation_id.clone(),
                timestamp: Self::now(),
            };

            let result = self.deliver_to_agent(&agent_id, message).await;
            results.push(result);
        }

        Ok(results)
    }

    async fn register_agent(
        &self,
        agent_id: AgentId,
        inbox: mpsc::UnboundedSender<A2aMessage>,
    ) -> anyhow::Result<()> {
        let mut agents = self.agents.write().await;
        agents.insert(agent_id, inbox);
        Ok(())
    }

    async fn unregister_agent(&self, agent_id: AgentId) -> anyhow::Result<()> {
        // Remove from agents
        {
            let mut agents = self.agents.write().await;
            agents.remove(&agent_id);
        }

        // Remove from all subscriptions
        {
            let mut subscriptions = self.subscriptions.write().await;
            for subscribers in subscriptions.values_mut() {
                subscribers.remove(&agent_id);
            }
            // Clean up empty topics
            subscriptions.retain(|_, subscribers| !subscribers.is_empty());
        }

        Ok(())
    }

    async fn connected_agents(&self) -> anyhow::Result<Vec<AgentId>> {
        let agents = self.agents.read().await;
        Ok(agents.keys().cloned().collect())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let mut agents = self.agents.write().await;
        agents.clear();

        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.clear();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_send_direct() {
        let bus = InMemoryBus::new();

        // Create receiver
        let (tx, mut rx) = mpsc::unbounded_channel();
        bus.register_agent("agent_b".to_string(), tx).await.unwrap();

        // Send message
        let result = bus
            .send_direct(
                "agent_a".to_string(),
                "agent_b".to_string(),
                serde_json::json!({"content": "hello"}),
                None,
            )
            .await
            .unwrap();

        assert!(matches!(result, DeliveryResult::Delivered));

        // Verify received
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.message_type, A2aMessageType::Direct);
        assert_eq!(msg.from, "agent_a");
        assert_eq!(msg.to, Some("agent_b".to_string()));
    }

    #[tokio::test]
    async fn test_send_to_unknown_agent() {
        let bus = InMemoryBus::new();

        let result = bus
            .send_direct(
                "agent_a".to_string(),
                "unknown".to_string(),
                serde_json::json!({}),
                None,
            )
            .await
            .unwrap();

        assert!(matches!(result, DeliveryResult::AgentNotFound(_)));
    }

    #[tokio::test]
    async fn test_broadcast() {
        let bus = InMemoryBus::new();

        // Create receivers
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        bus.register_agent("agent_1".to_string(), tx1)
            .await
            .unwrap();
        bus.register_agent("agent_2".to_string(), tx2)
            .await
            .unwrap();

        // Broadcast from agent_0
        let results = bus
            .broadcast(
                "agent_0".to_string(),
                serde_json::json!({"content": "hello all"}),
                None,
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| matches!(r, DeliveryResult::Delivered)));

        // Both agents should receive
        let msg1 = rx1.recv().await.unwrap();
        let msg2 = rx2.recv().await.unwrap();
        assert_eq!(msg1.message_type, A2aMessageType::Broadcast);
        assert_eq!(msg2.message_type, A2aMessageType::Broadcast);
    }

    #[tokio::test]
    async fn test_publish_subscribe() {
        let bus = InMemoryBus::new();

        // Create receivers
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        bus.register_agent("agent_1".to_string(), tx1)
            .await
            .unwrap();
        bus.register_agent("agent_2".to_string(), tx2)
            .await
            .unwrap();

        // Subscribe agent_1 to topic
        bus.subscribe("agent_1".to_string(), "tasks".to_string())
            .await
            .unwrap();

        // Publish to topic
        let results = bus
            .publish(
                "agent_0".to_string(),
                "tasks".to_string(),
                serde_json::json!({"task": "do something"}),
                None,
            )
            .await
            .unwrap();

        assert!(results
            .iter()
            .all(|r| matches!(r, DeliveryResult::Delivered)));

        // agent_1 should receive, agent_2 should not
        let msg = rx1.recv().await.unwrap();
        assert_eq!(msg.topic, Some("tasks".to_string()));

        // agent_2 should timeout
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_unregister() {
        let bus = InMemoryBus::new();

        let (tx, _rx) = mpsc::unbounded_channel();
        bus.register_agent("agent_1".to_string(), tx).await.unwrap();

        assert_eq!(bus.connected_agents().await.unwrap().len(), 1);

        bus.unregister_agent("agent_1".to_string()).await.unwrap();

        assert_eq!(bus.connected_agents().await.unwrap().len(), 0);
    }
}
