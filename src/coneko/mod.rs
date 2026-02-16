//! Coneko network integration (optional)
//!
//! This module provides integration with the Coneko agent coordination network.
//! It is completely optional - Pekobot works perfectly standalone without Coneko.
//!
//! # Features
//!
//! - **Agent Registration**: Register your agent with the Coneko network
//! - **Discovery**: Find other agents by capability
//! - **Message Routing**: Send/receive messages through Coneko
//! - **Cross-Network**: Connect local agents with remote ones
//!
//! # Example
//!
//! ```rust,ignore
//! use pekobot::coneko::{ConekoAdapter, UnifiedRegistry};
//!
//! // Create adapter (disabled by default)
//! let adapter = ConekoAdapter::disabled();
//!
//! // Or enable with Coneko endpoint
//! let adapter = ConekoAdapter::enabled(
//!     "https://coneko.example.com",
//!     Some("your-api-token")
//! );
//!
//! // Register your agent
//! adapter.register_agent(
//!     "did:pekobot:local:myorg:myagent",
//!     "My Agent",
//!     "http://localhost:8080",
//!     vec![/* capabilities */],
//!     "local",
//!     "myorg",
//! ).await?;
//!
//! // Discover other agents
//! let agents = adapter.discover_agents(
//!     Some(vec!["messaging".to_string()]),
//!     None,
//!     None
//! ).await?;
//! ```

pub mod client;
pub mod registry;

pub use client::{AgentInfo, ConekoClient};
pub use registry::{LocalRegistry, UnifiedRegistry};

use crate::a2a::A2AMessage;
use crate::types::agent::AgentCapability;
use serde::{Deserialize, Serialize};

/// Coneko adapter configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConekoConfig {
    /// Whether Coneko integration is enabled
    pub enabled: bool,
    /// Coneko server endpoint URL
    pub endpoint: String,
    /// Authentication token (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    /// Polling interval for messages (in milliseconds)
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    /// Auto-register on startup
    #[serde(default = "default_auto_register")]
    pub auto_register: bool,
}

fn default_poll_interval() -> u64 {
    5000 // 5 seconds
}

fn default_auto_register() -> bool {
    true
}

impl Default for ConekoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://localhost:8080".to_string(),
            auth_token: None,
            poll_interval_ms: default_poll_interval(),
            auto_register: default_auto_register(),
        }
    }
}

/// Coneko adapter for network integration
#[derive(Debug, Clone)]
pub struct ConekoAdapter {
    enabled: bool,
    endpoint: String,
    auth_token: Option<String>,
    poll_interval_ms: u64,
}

impl ConekoAdapter {
    /// Create a disabled adapter
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            endpoint: "".to_string(),
            auth_token: None,
            poll_interval_ms: 5000,
        }
    }

    /// Create an enabled adapter
    pub fn enabled(endpoint: &str, auth_token: Option<&str>) -> Self {
        Self {
            enabled: true,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            auth_token: auth_token.map(String::from),
            poll_interval_ms: 5000,
        }
    }

    /// Create from configuration
    pub fn from_config(config: &ConekoConfig) -> Self {
        if config.enabled {
            Self::enabled(&config.endpoint,
                config.auth_token.as_deref()
            )
        } else {
            Self::disabled()
        }
    }

    /// Check if Coneko is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get the endpoint URL
    pub fn endpoint(&self) -> Option<&str> {
        if self.enabled {
            Some(&self.endpoint)
        } else {
            None
        }
    }

    /// Get auth token
    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    /// Get poll interval
    pub fn poll_interval_ms(&self) -> u64 {
        self.poll_interval_ms
    }
}

/// Coneko service that handles background tasks
pub struct ConekoService {
    adapter: ConekoAdapter,
    did: String,
    name: String,
    endpoint: String,
    capabilities: Vec<AgentCapability>,
    scope: String,
    tenant: String,
}

impl ConekoService {
    /// Create a new Coneko service
    pub fn new(
        adapter: ConekoAdapter,
        did: String,
        name: String,
        endpoint: String,
        capabilities: Vec<AgentCapability>,
        scope: String,
        tenant: String,
    ) -> Self {
        Self {
            adapter,
            did,
            name,
            endpoint,
            capabilities,
            scope,
            tenant,
        }
    }

    /// Start the service (register and begin polling)
    pub async fn start(&self,
        message_handler: impl Fn(A2AMessage) + Send + Sync + 'static,
    ) -> anyhow::Result<()> {
        if !self.adapter.is_enabled() {
            tracing::info!("Coneko service disabled, skipping start");
            return Ok(());
        }

        // Register agent
        self.adapter
            .register_agent(
                &self.did,
                &self.name,
                &self.endpoint,
                self.capabilities.clone(),
                &self.scope,
                &self.tenant,
            )
            .await?;

        // Start polling in background
        let adapter = self.adapter.clone();
        let did = self.did.clone();
        let interval = self.adapter.poll_interval_ms();

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(
                std::time::Duration::from_millis(interval)
            );

            loop {
                ticker.tick().await;

                match adapter.poll_messages(&did).await {
                    Ok(messages) => {
                        for message in messages {
                            message_handler(message);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to poll messages: {}", e);
                    }
                }
            }
        });

        tracing::info!("Coneko service started, polling every {}ms", interval);
        Ok(())
    }

    /// Stop the service (unregister)
    pub async fn stop(&self,
    ) -> anyhow::Result<()> {
        if !self.adapter.is_enabled() {
            return Ok(());
        }

        self.adapter.unregister_agent(&self.did).await?;
        tracing::info!("Coneko service stopped");
        Ok(())
    }

    /// Send a message through Coneko
    pub async fn send_message(
        &self,
        message: &A2AMessage,
    ) -> anyhow::Result<String> {
        self.adapter.send_message(message).await
    }

    /// Discover agents
    pub async fn discover(
        &self,
        capabilities: Option<Vec<String>>,
    ) -> anyhow::Result<Vec<AgentInfo>> {
        self.adapter.discover_agents(capabilities, None, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_disabled() {
        let adapter = ConekoAdapter::disabled();
        assert!(!adapter.is_enabled());
        assert!(adapter.endpoint().is_none());
    }

    #[test]
    fn test_adapter_enabled() {
        let adapter = ConekoAdapter::enabled("http://localhost:8080", Some("token"));
        assert!(adapter.is_enabled());
        assert_eq!(adapter.endpoint(), Some("http://localhost:8080"));
        assert_eq!(adapter.auth_token(), Some("token"));
    }

    #[test]
    fn test_config_defaults() {
        let config = ConekoConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.poll_interval_ms, 5000);
        assert!(config.auto_register);
    }

    #[test]
    fn test_adapter_from_config() {
        let config = ConekoConfig {
            enabled: true,
            endpoint: "https://coneko.example.com".to_string(),
            auth_token: Some("secret".to_string()),
            poll_interval_ms: 10000,
            auto_register: true,
        };

        let adapter = ConekoAdapter::from_config(&config);
        assert!(adapter.is_enabled());
        assert_eq!(adapter.endpoint(), Some("https://coneko.example.com"));
    }
}
