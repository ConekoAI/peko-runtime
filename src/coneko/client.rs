//! Coneko HTTP client for network integration
//!
//! Provides HTTP client for communicating with the Coneko coordination network:
//! - Agent registration and discovery
//! - Cross-network message routing
//! - Health checks and connection management

use super::ConekoAdapter;
use crate::a2a::A2AMessage;
use crate::types::agent::AgentCapability;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// HTTP client for Coneko network
pub struct ConekoClient {
    http: reqwest::Client,
    endpoint: String,
    auth_token: Option<String>,
}

/// Agent information from Coneko registry
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentInfo {
    pub did: String,
    pub name: String,
    pub endpoint: String,
    pub capabilities: Vec<AgentCapability>,
    pub scope: String,
    pub tenant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Registration request payload
#[derive(Debug, Serialize)]
struct RegisterRequest {
    did: String,
    name: String,
    endpoint: String,
    capabilities: Vec<AgentCapability>,
    scope: String,
    tenant: String,
}

/// Registration response
#[derive(Debug, Deserialize)]
struct RegisterResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Discovery request payload
#[derive(Debug, Serialize)]
struct DiscoverRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
}

/// Discovery response
#[derive(Debug, Deserialize)]
struct DiscoverResponse {
    agents: Vec<AgentInfo>,
}

/// Message send response
#[derive(Debug, Deserialize)]
struct SendResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ConekoClient {
    /// Create a new Coneko client
    pub fn new(endpoint: &str, auth_token: Option<&str>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            http,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            auth_token: auth_token.map(String::from),
        })
    }

    /// Check if the Coneko server is reachable
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/health", self.endpoint);
        
        match self.http.get(&url).send().await {
            Ok(response) => {
                let healthy = response.status().is_success();
                if healthy {
                    debug!("Coneko health check: OK");
                } else {
                    warn!("Coneko health check failed: {}", response.status());
                }
                Ok(healthy)
            }
            Err(e) => {
                warn!("Coneko health check error: {}", e);
                Ok(false)
            }
        }
    }

    /// Register an agent with Coneko
    pub async fn register_agent(
        &self,
        did: &str,
        name: &str,
        endpoint: &str,
        capabilities: Vec<AgentCapability>,
        scope: &str,
        tenant: &str,
    ) -> Result<()> {
        let url = format!("{}/api/v1/agents/register", self.endpoint);
        
        let request = RegisterRequest {
            did: did.to_string(),
            name: name.to_string(),
            endpoint: endpoint.to_string(),
            capabilities,
            scope: scope.to_string(),
            tenant: tenant.to_string(),
        };

        debug!("Registering agent {} with Coneko", did);

        let mut req_builder = self.http.post(&url).json(&request);
        
        if let Some(token) = &self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let response = req_builder
            .send()
            .await
            .context("Failed to send registration request")?;

        let status = response.status();
        let body: RegisterResponse = response
            .json()
            .await
            .context("Failed to parse registration response")?;

        if status.is_success() && body.success {
            info!("Agent {} registered successfully with Coneko", did);
            Ok(())
        } else {
            let error_msg = body.error.unwrap_or_else(|| "Unknown error".to_string());
            anyhow::bail!("Registration failed: {}", error_msg)
        }
    }

    /// Unregister an agent from Coneko
    pub async fn unregister_agent(&self, did: &str) -> Result<()> {
        let url = format!("{}/api/v1/agents/{}/unregister", self.endpoint, did);
        
        debug!("Unregistering agent {} from Coneko", did);

        let mut req_builder = self.http.post(&url);
        
        if let Some(token) = &self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let response = req_builder
            .send()
            .await
            .context("Failed to send unregistration request")?;

        if response.status().is_success() {
            info!("Agent {} unregistered from Coneko", did);
            Ok(())
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Unregistration failed: {} - {}", status, text)
        }
    }

    /// Discover agents by capability
    pub async fn discover_agents(
        &self,
        capabilities: Option<Vec<String>>,
        scope: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<Vec<AgentInfo>> {
        let url = format!("{}/api/v1/agents/discover", self.endpoint);
        
        let request = DiscoverRequest {
            capabilities,
            scope: scope.map(String::from),
            tenant: tenant.map(String::from),
        };

        debug!("Discovering agents with filters: {:?}", request);

        let mut req_builder = self.http.post(&url).json(&request);
        
        if let Some(token) = &self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let response = req_builder
            .send()
            .await
            .context("Failed to send discovery request")?;

        let status = response.status();
        let body: DiscoverResponse = response
            .json()
            .await
            .context("Failed to parse discovery response")?;

        if status.is_success() {
            info!("Discovered {} agents", body.agents.len());
            Ok(body.agents)
        } else {
            anyhow::bail!("Discovery failed: {}", status)
        }
    }

    /// Send a message to an agent through Coneko
    pub async fn send_message(&self, message: &A2AMessage) -> Result<String> {
        let url = format!("{}/api/v1/messages", self.endpoint);
        
        debug!(
            "Sending message {} to {} via Coneko",
            message.message_id, message.recipient.did
        );

        let mut req_builder = self.http.post(&url).json(message);
        
        if let Some(token) = &self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let response = req_builder
            .send()
            .await
            .context("Failed to send message")?;

        let status = response.status();
        let body: SendResponse = response
            .json()
            .await
            .context("Failed to parse send response")?;

        if status.is_success() && body.success {
            let message_id = body.message_id.unwrap_or_else(|| message.message_id.clone());
            debug!("Message sent successfully: {}", message_id);
            Ok(message_id)
        } else {
            let error_msg = body.error.unwrap_or_else(|| "Unknown error".to_string());
            anyhow::bail!("Failed to send message: {}", error_msg)
        }
    }

    /// Poll for messages for a specific agent
    pub async fn poll_messages(&self, did: &str) -> Result<Vec<A2AMessage>> {
        let url = format!("{}/api/v1/agents/{}/messages", self.endpoint, did);
        
        debug!("Polling messages for agent {}", did);

        let mut req_builder = self.http.get(&url);
        
        if let Some(token) = &self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let response = req_builder
            .send()
            .await
            .context("Failed to poll messages")?;

        if response.status().is_success() {
            let messages: Vec<A2AMessage> = response
                .json()
                .await
                .context("Failed to parse messages")?;
            
            if !messages.is_empty() {
                debug!("Received {} messages", messages.len());
            }
            
            Ok(messages)
        } else {
            anyhow::bail!("Failed to poll messages: {}", response.status())
        }
    }
}

impl ConekoAdapter {
    /// Register an agent with Coneko
    pub async fn register_agent(
        &self,
        did: &str,
        name: &str,
        endpoint: &str,
        capabilities: Vec<AgentCapability>,
        scope: &str,
        tenant: &str,
    ) -> anyhow::Result<()> {
        if !self.enabled {
            debug!("Coneko adapter disabled, skipping registration");
            return Ok(());
        }

        let client = ConekoClient::new(&self.endpoint, self.auth_token.as_deref())?;
        client
            .register_agent(did, name, endpoint, capabilities, scope, tenant)
            .await
    }

    /// Unregister an agent from Coneko
    pub async fn unregister_agent(&self, did: &str) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let client = ConekoClient::new(&self.endpoint, self.auth_token.as_deref())?;
        client.unregister_agent(did).await
    }

    /// Discover agents by capability
    pub async fn discover_agents(
        &self,
        capabilities: Option<Vec<String>>,
        scope: Option<&str>,
        tenant: Option<&str>,
    ) -> anyhow::Result<Vec<AgentInfo>> {
        if !self.enabled {
            return Ok(vec![]);
        }

        let client = ConekoClient::new(&self.endpoint, self.auth_token.as_deref())?;
        client.discover_agents(capabilities, scope, tenant).await
    }

    /// Send a message through Coneko
    pub async fn send_message(&self, message: &A2AMessage) -> anyhow::Result<String> {
        if !self.enabled {
            anyhow::bail!("Coneko adapter is disabled");
        }

        let client = ConekoClient::new(&self.endpoint, self.auth_token.as_deref())?;
        client.send_message(message).await
    }

    /// Poll for messages
    pub async fn poll_messages(&self, did: &str) -> anyhow::Result<Vec<A2AMessage>> {
        if !self.enabled {
            return Ok(vec![]);
        }

        let client = ConekoClient::new(&self.endpoint, self.auth_token.as_deref())?;
        client.poll_messages(did).await
    }

    /// Health check
    pub async fn health_check(&self) -> anyhow::Result<bool> {
        if !self.enabled {
            return Ok(false);
        }

        let client = ConekoClient::new(&self.endpoint, self.auth_token.as_deref())?;
        client.health_check().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = ConekoClient::new("http://localhost:8080", Some("test-token"));
        assert!(client.is_ok());
    }

    #[test]
    fn test_client_creation_no_auth() {
        let client = ConekoClient::new("http://localhost:8080", None);
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_health_check_disabled() {
        let adapter = ConekoAdapter::disabled();
        assert!(!adapter.is_enabled());
        
        // Health check on disabled adapter returns false
        let result = adapter.health_check().await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }
}
