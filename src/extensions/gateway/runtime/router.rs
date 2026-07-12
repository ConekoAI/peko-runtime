//! GatewayRouter — routes incoming messages from gateways to agents

use crate::agents::stateless_service::StatelessAgentService;
use crate::common::types::principal_message::PrincipalMessageRequest;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// A message queued while a gateway is offline
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub channel_id: String,
    pub user_id: String,
    pub message: String,
    pub metadata: serde_json::Value,
    pub queued_at: chrono::DateTime<chrono::Utc>,
}

/// Routing configuration for a gateway
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct GatewayRoutingConfig {
    /// Default agent for unmapped channels
    pub default_agent: String,
    /// channel_id → agent_name
    #[serde(default)]
    pub channel_map: HashMap<String, String>,
    /// user_id → agent_name (for DMs)
    #[serde(default)]
    pub dm_agents: HashMap<String, String>,
}

/// Routes incoming messages from gateways to the correct agent and session
#[derive(Clone)]
pub struct GatewayRouter {
    /// gateway_id → routing config
    routing_table: Arc<RwLock<HashMap<String, GatewayRoutingConfig>>>,
    /// Per-gateway offline queues
    offline_queues: Arc<RwLock<HashMap<String, Vec<QueuedMessage>>>>,
    /// Principal message service for execution (handles principal-to-principal
    /// message dispatch routed via gateway channels).
    principal_service: Arc<StatelessAgentService>,
}

impl GatewayRouter {
    /// Create a new gateway router
    pub fn new(principal_service: Arc<StatelessAgentService>) -> Self {
        Self {
            routing_table: Arc::new(RwLock::new(HashMap::new())),
            offline_queues: Arc::new(RwLock::new(HashMap::new())),
            principal_service,
        }
    }

    /// Register a gateway's routing configuration
    pub async fn register_gateway(
        &self,
        gateway_id: &str,
        config: GatewayRoutingConfig,
    ) -> Result<()> {
        let mut table = self.routing_table.write().await;
        table.insert(gateway_id.to_string(), config);
        debug!("Registered routing config for gateway '{}'", gateway_id);
        Ok(())
    }

    /// Unregister a gateway
    pub async fn unregister_gateway(&self, gateway_id: &str) {
        let mut table = self.routing_table.write().await;
        table.remove(gateway_id);
        let mut queues = self.offline_queues.write().await;
        queues.remove(gateway_id);
        debug!("Unregistered gateway '{}'", gateway_id);
    }

    /// Mark gateway offline (messages will be queued)
    pub async fn mark_offline(&self, gateway_id: &str) {
        warn!(
            "Gateway '{}' marked offline — messages will be queued",
            gateway_id
        );
        let mut queues = self.offline_queues.write().await;
        queues.entry(gateway_id.to_string()).or_default();
    }

    /// Mark gateway online (drain queued messages)
    pub async fn mark_online(&self, gateway_id: &str) {
        info!("Gateway '{}' marked online", gateway_id);
        let queued = {
            let mut queues = self.offline_queues.write().await;
            queues.remove(gateway_id)
        };

        if let Some(messages) = queued {
            if !messages.is_empty() {
                info!(
                    "Draining {} queued messages for gateway '{}'",
                    messages.len(),
                    gateway_id
                );
                for msg in messages {
                    if let Err(e) = self
                        .route_incoming(
                            gateway_id,
                            &msg.channel_id,
                            &msg.user_id,
                            &msg.message,
                            msg.metadata,
                        )
                        .await
                    {
                        warn!("Failed to deliver queued message: {}", e);
                    }
                }
            }
        }
    }

    /// Resolve the agent name for a given channel/DM
    async fn resolve_agent(
        &self,
        gateway_id: &str,
        channel_id: &str,
        user_id: &str,
    ) -> Option<String> {
        let table = self.routing_table.read().await;
        let config = table.get(gateway_id)?;

        // Check DM mapping first
        if let Some(agent) = config.dm_agents.get(user_id) {
            return Some(agent.clone());
        }

        // Check channel mapping
        if let Some(agent) = config.channel_map.get(channel_id) {
            return Some(agent.clone());
        }

        // Fall back to default
        Some(config.default_agent.clone())
    }

    /// Route an incoming message to the appropriate agent
    pub async fn route_incoming(
        &self,
        gateway_id: &str,
        channel_id: &str,
        user_id: &str,
        message: &str,
        metadata: serde_json::Value,
    ) -> Result<String> {
        warn!(
            "Routing incoming message for gateway '{}' channel '{}' user '{}'",
            gateway_id, channel_id, user_id
        );

        // Check if gateway is offline
        {
            let queues = self.offline_queues.read().await;
            if queues.contains_key(gateway_id) {
                let mut queues = self.offline_queues.write().await;
                queues.get_mut(gateway_id).unwrap().push(QueuedMessage {
                    channel_id: channel_id.to_string(),
                    user_id: user_id.to_string(),
                    message: message.to_string(),
                    metadata,
                    queued_at: chrono::Utc::now(),
                });
                return Ok("Message queued — gateway offline".to_string());
            }
        }

        let agent_name = self
            .resolve_agent(gateway_id, channel_id, user_id)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No agent configured for gateway '{}', channel '{}'",
                    gateway_id,
                    channel_id
                )
            })?;

        warn!(
            "Resolved agent '{}' for gateway '{}' channel '{}'",
            agent_name, gateway_id, channel_id
        );

        debug!(
            "Routing message from gateway '{}' channel '{}' to agent '{}'",
            gateway_id, channel_id, agent_name
        );

        // Generate a session ID based on gateway + channel + user
        // Use '__' as separator instead of ':' for Windows filename compatibility
        let session_id = format!("{}__{}__{}", gateway_id, channel_id, user_id);

        let request = PrincipalMessageRequest::new(&agent_name, message)
            .with_session(&session_id)
            .with_user(user_id);

        warn!(
            "Executing message for agent '{}' via gateway '{}'",
            agent_name, gateway_id
        );

        match self.principal_service.execute_message(request).await {
            Ok(result) => {
                info!(
                    "Agent '{}' executed message successfully, response length: {}",
                    agent_name,
                    result.content.len()
                );
                Ok(result.content)
            }
            Err(e) => {
                error!("Agent execution failed for gateway '{}': {}", gateway_id, e);
                Err(anyhow::anyhow!("Agent execution failed: {}", e))
            }
        }
    }

    /// Deliver an agent response back to the gateway
    ///
    /// This is called by the gateway adapter after receiving a response from the agent.
    /// The actual delivery mechanism (stdio, HTTP, etc.) is handled by the adapter.
    pub async fn deliver_outgoing(
        &self,
        gateway_id: &str,
        channel_id: &str,
        _message: &str,
        session_id: &str,
    ) -> Result<()> {
        debug!(
            "Delivering response to gateway '{}' channel '{}' (session: {})",
            gateway_id, channel_id, session_id
        );
        // The actual delivery is handled by the GatewayRuntimeAdapter
        // This method exists for logging, metrics, and future queueing
        Ok(())
    }

    /// Get the routing configuration for a gateway
    pub async fn get_routing(&self, gateway_id: &str) -> Option<GatewayRoutingConfig> {
        let table = self.routing_table.read().await;
        table.get(gateway_id).cloned()
    }

    /// Update the routing configuration for a gateway
    pub async fn update_routing(
        &self,
        gateway_id: &str,
        config: GatewayRoutingConfig,
    ) -> Result<()> {
        let mut table = self.routing_table.write().await;
        table.insert(gateway_id.to_string(), config);
        Ok(())
    }
}

impl std::fmt::Debug for GatewayRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayRouter")
            .field("routing_table", &"<HashMap<String, GatewayRoutingConfig>>")
            .field("offline_queues", &"<HashMap<String, Vec<QueuedMessage>>>")
            .field("principal_service", &"<StatelessAgentService>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_routing_config_default() {
        let config = GatewayRoutingConfig {
            default_agent: "assistant".to_string(),
            channel_map: [("#general".to_string(), "assistant".to_string())]
                .into_iter()
                .collect(),
            dm_agents: [("user_123".to_string(), "personal".to_string())]
                .into_iter()
                .collect(),
        };

        assert_eq!(config.default_agent, "assistant");
        assert_eq!(
            config.channel_map.get("#general"),
            Some(&"assistant".to_string())
        );
        assert_eq!(
            config.dm_agents.get("user_123"),
            Some(&"personal".to_string())
        );
    }
}
