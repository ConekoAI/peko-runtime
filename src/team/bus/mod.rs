//! Event Bus for A2A Communication
//!
//! Implements REQ-BUS-001 through REQ-BUS-003:
//! - Bus-mediated A2A communication
//! - Pluggable backends (in-memory, redis, nats)
//! - Message observability via session events

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Unique message ID
pub type MessageId = String;

/// Topic/channel identifier
pub type Topic = String;

/// Agent instance identifier
pub type AgentId = String;

/// A2A message envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aMessage {
    /// Unique message ID
    pub id: MessageId,

    /// Message type for routing
    #[serde(rename = "type")]
    pub message_type: A2aMessageType,

    /// Source agent ID
    pub from: AgentId,

    /// Target agent ID (for Direct messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<AgentId>,

    /// Topic/channel (for Broadcast, Subscribe messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<Topic>,

    /// Message payload
    pub payload: serde_json::Value,

    /// Conversation/correlation ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,

    /// Timestamp (ISO 8601)
    pub timestamp: String,
}

/// A2A message types per REQ-BUS-001
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum A2aMessageType {
    /// Direct message to specific agent
    Direct,
    /// Task assignment
    Task,
    /// Task result
    TaskResult,
    /// Broadcast to all agents
    Broadcast,
    /// Subscribe to topic
    Subscribe,
}

impl fmt::Display for A2aMessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            A2aMessageType::Direct => write!(f, "Direct"),
            A2aMessageType::Task => write!(f, "Task"),
            A2aMessageType::TaskResult => write!(f, "TaskResult"),
            A2aMessageType::Broadcast => write!(f, "Broadcast"),
            A2aMessageType::Subscribe => write!(f, "Subscribe"),
        }
    }
}

/// Delivery result for a message
#[derive(Debug, Clone)]
pub enum DeliveryResult {
    /// Delivered successfully
    Delivered,
    /// Agent not found or not connected
    AgentNotFound(AgentId),
    /// Topic not found
    TopicNotFound(Topic),
    /// Backend error
    Error(String),
}

/// Event bus trait - pluggable backend interface
#[async_trait]
pub trait EventBus: Send + Sync {
    /// Send a direct message to a specific agent
    async fn send_direct(
        &self,
        from: AgentId,
        to: AgentId,
        payload: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<DeliveryResult>;

    /// Broadcast a message to all agents
    async fn broadcast(
        &self,
        from: AgentId,
        payload: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<Vec<DeliveryResult>>;

    /// Send a task assignment
    async fn send_task(
        &self,
        from: AgentId,
        to: AgentId,
        task: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<DeliveryResult>;

    /// Send a task result
    async fn send_task_result(
        &self,
        from: AgentId,
        to: AgentId,
        result: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<DeliveryResult>;

    /// Subscribe an agent to a topic
    async fn subscribe(&self, agent_id: AgentId, topic: Topic) -> anyhow::Result<()>;

    /// Unsubscribe an agent from a topic
    async fn unsubscribe(&self, agent_id: AgentId, topic: Topic) -> anyhow::Result<()>;

    /// Publish a message to a topic
    async fn publish(
        &self,
        from: AgentId,
        topic: Topic,
        payload: serde_json::Value,
        conversation_id: Option<String>,
    ) -> anyhow::Result<Vec<DeliveryResult>>;

    /// Register an agent with the bus
    async fn register_agent(
        &self,
        agent_id: AgentId,
        inbox: mpsc::UnboundedSender<A2aMessage>,
    ) -> anyhow::Result<()>;

    /// Unregister an agent from the bus
    async fn unregister_agent(&self, agent_id: AgentId) -> anyhow::Result<()>;

    /// Get list of connected agents
    async fn connected_agents(&self) -> anyhow::Result<Vec<AgentId>>;

    /// Shutdown the bus
    async fn shutdown(&self) -> anyhow::Result<()>;
}

/// Event bus factory - creates appropriate backend
pub async fn create_bus(
    backend: crate::team::config::BusBackend,
    url: Option<String>,
) -> anyhow::Result<Arc<dyn EventBus>> {
    match backend {
        crate::team::config::BusBackend::InMemory => Ok(Arc::new(InMemoryBus::new())),
        crate::team::config::BusBackend::Redis => {
            // For Phase 1, we only support in-memory
            // Redis/NATS backends are SHOULD items that can be deferred
            tracing::warn!("Redis backend not yet implemented, falling back to in-memory");
            Ok(Arc::new(InMemoryBus::new()))
        }
        crate::team::config::BusBackend::Nats => {
            tracing::warn!("NATS backend not yet implemented, falling back to in-memory");
            Ok(Arc::new(InMemoryBus::new()))
        }
    }
}

// In-memory bus implementation
mod memory;
pub use memory::InMemoryBus;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = A2aMessage {
            id: "msg_123".to_string(),
            message_type: A2aMessageType::Direct,
            from: "agent_a".to_string(),
            to: Some("agent_b".to_string()),
            topic: None,
            payload: serde_json::json!({"content": "hello"}),
            conversation_id: Some("conv_456".to_string()),
            timestamp: "2026-03-17T10:00:00.000Z".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: A2aMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, msg.id);
        assert_eq!(deserialized.message_type, A2aMessageType::Direct);
    }
}
