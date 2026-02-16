//! Agent registry for local multi-agent orchestration

use crate::a2a::message::{A2AMessage, MessageType, Payload, IntentPayload, QuotePayload, AcceptPayload, ContractPayload};
use crate::agent::Agent;
use anyhow::{Context, Result};
use std::collections::HashMap;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// Message bus for inter-agent communication
#[derive(Debug)]
pub struct MessageBus {
    /// Channel sender for broadcasting messages
    sender: mpsc::Sender<A2AMessage>,
}

impl MessageBus {
    /// Create a new message bus
    pub fn new() -> (Self, mpsc::Receiver<A2AMessage>) {
        let (sender, receiver) = mpsc::channel(100);
        (Self { sender }, receiver)
    }

    /// Send a message to the bus
    pub async fn send(&self, message: A2AMessage) -> Result<()> {
        self.sender
            .send(message)
            .await
            .context("Failed to send message to bus")?;
        Ok(())
    }
}

impl Clone for MessageBus {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

/// Local agent registry for managing multiple agents
pub struct AgentRegistry {
    /// DID -> Agent mapping
    agents: RwLock<HashMap<String, ArcAgent>>,
    /// Message bus for routing
    message_bus: MessageBus,
}

/// Arc-wrapped agent for shared ownership
pub type ArcAgent = std::sync::Arc<tokio::sync::Mutex<Agent>>;

impl AgentRegistry {
    /// Create a new agent registry
    pub fn new(message_bus: MessageBus) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            message_bus,
        }
    }

    /// Register an agent
    pub async fn register(&self, agent: Agent) -> Result<()> {
        let did = agent.did().to_string();
        let name = agent.name().to_string();
        
        let mut agents = self.agents.write().await;
        
        if agents.contains_key(&did) {
            return Err(anyhow::anyhow!("Agent with DID {} already registered", did));
        }

        agents.insert(did.clone(), std::sync::Arc::new(tokio::sync::Mutex::new(agent)));
        info!("Registered agent: {} ({})", name, did);
        
        Ok(())
    }

    /// Unregister an agent
    pub async fn unregister(&self, did: &str) -> Result<Option<ArcAgent>> {
        let mut agents = self.agents.write().await;
        let removed = agents.remove(did);
        
        if removed.is_some() {
            info!("Unregistered agent: {}", did);
        }
        
        Ok(removed)
    }

    /// Get an agent by DID
    pub async fn get_by_did(&self, did: &str) -> Option<ArcAgent> {
        let agents = self.agents.read().await;
        agents.get(did).cloned()
    }

    /// Get an agent by name
    pub async fn get_by_name(&self, name: &str) -> Option<ArcAgent> {
        let agents = self.agents.read().await;
        agents
            .values()
            .find(|agent| {
                // This is a bit inefficient but works for small registries
                // In production, maintain a name->DID index
                if let Ok(agent) = agent.try_lock() {
                    agent.name() == name
                } else {
                    false
                }
            })
            .cloned()
    }

    /// List all registered agents
    pub async fn list_agents(&self) -> Vec<(String, String)> {
        let agents = self.agents.read().await;
        let mut result = Vec::new();
        
        for (did, agent) in agents.iter() {
            if let Ok(agent) = agent.try_lock() {
                result.push((did.clone(), agent.name().to_string()));
            }
        }
        
        result
    }

    /// Get message bus
    pub fn message_bus(&self) -> &MessageBus {
        &self.message_bus
    }

    /// Route a message to its recipient
    pub async fn route_message(&self, message: A2AMessage) -> Result<()> {
        let recipient_did = &message.recipient.did;
        
        debug!("Routing message {} to {}", message.message_id, recipient_did);
        
        // Find the recipient agent
        let agent = self.get_by_did(recipient_did).await;
        
        if let Some(agent) = agent {
            // Process the message in the agent
            let mut agent = agent.lock().await;
            
            // Store the message in agent memory
            if let Err(e) = agent.store_memory(
                &format!("Received A2A message: {:?}", message.message_type),
                Some(serde_json::json!({
                    "message_type": format!("{:?}", message.message_type),
                    "sender": message.sender.did,
                    "message_id": message.message_id,
                    "thread_id": message.thread_id,
                })),
            ) {
                warn!("Failed to store message in memory: {}", e);
            }
            
            info!(
                "Delivered message {} to agent {} (type: {:?})",
                message.message_id,
                agent.name(),
                message.message_type
            );
            
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Recipient agent not found: {}",
                recipient_did
            ))
        }
    }

    /// Start all registered agents
    pub async fn start_all(&self) -> Result<()> {
        let agents = self.agents.read().await;
        
        for (did, agent) in agents.iter() {
            let mut agent = agent.lock().await;
            if let Err(e) = agent.start().await {
                error!("Failed to start agent {}: {}", did, e);
            }
        }
        
        info!("Started {} agents", agents.len());
        Ok(())
    }

    /// Stop all registered agents
    pub async fn stop_all(&self) -> Result<()> {
        let agents = self.agents.read().await;
        
        for (did, agent) in agents.iter() {
            let mut agent = agent.lock().await;
            if let Err(e) = agent.stop().await {
                error!("Failed to stop agent {}: {}", did, e);
            }
        }
        
        info!("Stopped {} agents", agents.len());
        Ok(())
    }
}

/// Thread-safe shared registry
pub type SharedRegistry = std::sync::Arc<AgentRegistry>;

/// Create a new shared registry with message bus
pub fn create_registry() -> (SharedRegistry, mpsc::Receiver<A2AMessage>) {
    let (bus, receiver) = MessageBus::new();
    let registry = std::sync::Arc::new(AgentRegistry::new(bus));
    (registry, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::agent::AgentConfig;

    #[tokio::test]
    async fn test_registry() {
        let (bus, _receiver) = MessageBus::new();
        let registry = AgentRegistry::new(bus);

        // Create a test agent
        let config = AgentConfig {
            name: "test-agent".to_string(),
            ..Default::default()
        };

        let agent = Agent::new(config).await.unwrap();
        let did = agent.did().to_string();

        // Register
        registry.register(agent).await.unwrap();
        assert!(registry.get_by_did(&did).await.is_some());

        // List
        let agents = registry.list_agents().await;
        assert_eq!(agents.len(), 1);

        // Unregister
        registry.unregister(&did).await.unwrap();
        assert!(registry.get_by_did(&did).await.is_none());
    }
}
