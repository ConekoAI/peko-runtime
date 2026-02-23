//! Gateway manager
//!
//! The GatewayManager is the central coordinator for all gateway connections.
//! It manages gateway instances, routes messages between agents and gateways,
//! and handles lifecycle events.
//!
//! # Architecture
//!
//! ```
//! ┌─────────────────────────────────────────┐
//! │         GatewayManager                  │
//! │  ┌─────────────────────────────────┐    │
//! │  │      GatewayRegistry            │    │
//! │  │  ┌───────────────────────────┐  │    │
//! │  │  │  PluginHandle (discord)   │  │    │
//! │  │  │  PluginHandle (whatsapp)  │  │    │
//! │  │  └───────────────────────────┘  │    │
//! │  └─────────────────────────────────┘    │
//! │                                         │
//! │  ┌─────────────────────────────────┐    │
//! │  │      Active Instances           │    │
//! │  │  ┌─────────┐ ┌─────────┐       │    │
//! │  │  │discord-1│ │slack-1  │ ...   │    │
//! │  │  └────┬────┘ └────┬────┘       │    │
//! │  └───────┼───────────┼────────────┘    │
//! │          │           │                 │
//! └──────────┼───────────┼─────────────────┘
//!            │           │
//!     ┌──────▼───────────▼──────┐
//!     │    Message Router       │
//!     └───────────┬─────────────┘
//!                 │
//!         ┌───────▼────────┐
//!         │  Agent System  │
//!         └────────────────┘
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::gateway::config::{GatewayConfig, GatewaysConfig};
use crate::gateway::error::{GatewayError, GatewayResult};
use crate::gateway::interface::{GatewayPlugin, IncomingMessage, MessageContent, Target};
use crate::gateway::registry::GatewayRegistry;
use crate::gateway::types::{ChannelId, GatewayId, MessageId, UserId};

/// Event from a gateway
#[derive(Debug, Clone)]
pub enum GatewayEvent {
    /// Incoming message
    Message {
        instance_id: String,
        message: IncomingMessage,
    },
    /// Gateway connected
    Connected { instance_id: String },
    /// Gateway disconnected
    Disconnected { instance_id: String, reason: String },
    /// Error from gateway
    Error { instance_id: String, error: String },
}

/// Handle to a running gateway instance
#[derive(Debug, Clone)]
pub struct InstanceHandle {
    /// Unique instance ID
    pub id: String,
    /// Gateway name (plugin name)
    pub gateway: String,
    /// Instance name from config
    pub name: String,
    /// Event sender
    pub event_tx: mpsc::Sender<GatewayEvent>,
}

/// Gateway manager - central coordinator
pub struct GatewayManager {
    /// Plugin registry
    registry: GatewayRegistry,
    /// Active instances
    instances: Arc<RwLock<HashMap<String, Box<dyn GatewayPlugin>>>>,
    /// Instance handles (for events)
    handles: Arc<RwLock<HashMap<String, InstanceHandle>>>,
    /// Event broadcaster
    event_tx: mpsc::Sender<GatewayEvent>,
    /// Event receiver (for internal use)
    event_rx: Arc<RwLock<mpsc::Receiver<GatewayEvent>>>,
}

impl GatewayManager {
    /// Create a new gateway manager
    pub async fn new(config: GatewaysConfig) -> GatewayResult<Self> {
        let (event_tx, event_rx) = mpsc::channel(1000);
        
        let registry = GatewayRegistry::new(
            &config.cache_dir,
            &config.pekohub_url
        );

        Ok(Self {
            registry,
            instances: Arc::new(RwLock::new(HashMap::new())),
            handles: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            event_rx: Arc::new(RwLock::new(event_rx)),
        })
    }

    /// Initialize gateways from configuration
    pub async fn init_from_config(
        &mut self,
        config: &GatewaysConfig,
    ) -> GatewayResult<()> {
        info!("Initializing gateways from configuration");

        for gateway_config in &config.gateways {
            if !gateway_config.enabled {
                debug!("Skipping disabled gateway: {}", gateway_config.name);
                continue;
            }

            match self.start_gateway(gateway_config).await {
                Ok(_) => {
                    info!("Started gateway: {}", gateway_config.name);
                }
                Err(e) => {
                    error!(
                        "Failed to start gateway '{}': {}",
                        gateway_config.name, e
                    );
                    // Continue with other gateways
                }
            }
        }

        info!("Gateway initialization complete");
        Ok(())
    }

    /// Start a gateway instance
    pub async fn start_gateway(
        &self,
        config: &GatewayConfig,
    ) -> GatewayResult<InstanceHandle> {
        info!(
            "Starting gateway instance: {} (plugin: {})",
            config.name, config.plugin
        );

        // Ensure plugin is loaded
        self.registry.load(&config.plugin).await.map_err(|e| {
            GatewayError::Plugin(Box::new(e))
        })?;

        // Create instance
        let instance_id = format!("{}-{}", config.plugin, config.name);
        let mut instance = self.registry
            .create_instance(&config.plugin,
                config.config.clone()
            )
            .await?;

        // Start the gateway
        let mut stream = instance.start().await?;

        // Create handle
        let handle = InstanceHandle {
            id: instance_id.clone(),
            gateway: config.plugin.clone(),
            name: config.name.clone(),
            event_tx: self.event_tx.clone(),
        };

        // Spawn message handler
        let event_tx = self.event_tx.clone();
        let instance_id_clone = instance_id.clone();
        
        tokio::spawn(async move {
            while let Some(message) = stream.recv().await {
                let event = GatewayEvent::Message {
                    instance_id: instance_id_clone.clone(),
                    message,
                };
                if event_tx.send(event).await.is_err() {
                    break;
                }
            }
        });

        // Store instance and handle
        {
            let mut instances = self.instances.write().await;
            instances.insert(instance_id.clone(), instance);
        }
        {
            let mut handles = self.handles.write().await;
            handles.insert(instance_id.clone(), handle.clone());
        }

        // Emit connected event
        let _ = self.event_tx.send(GatewayEvent::Connected {
            instance_id: instance_id.clone(),
        }).await;

        info!("Gateway instance started: {}", instance_id);
        Ok(handle)
    }

    /// Stop a gateway instance
    pub async fn stop_gateway(
        &self,
        instance_id: &str,
    ) -> GatewayResult<()> {
        info!("Stopping gateway instance: {}", instance_id);

        // Get and remove instance
        let instance = {
            let mut instances = self.instances.write().await;
            instances.remove(instance_id)
        }.ok_or_else(|| GatewayError::PluginNotFound(
            instance_id.to_string()
        ))?;

        // Shutdown
        instance.shutdown().await?;

        // Remove handle
        {
            let mut handles = self.handles.write().await;
            handles.remove(instance_id);
        }

        // Emit disconnected event
        let _ = self.event_tx.send(GatewayEvent::Disconnected {
            instance_id: instance_id.to_string(),
            reason: "Stopped by user".to_string(),
        }).await;

        info!("Gateway instance stopped: {}", instance_id);
        Ok(())
    }

    /// Send a message through a gateway instance
    pub async fn send(
        &self,
        instance_id: &str,
        target: Target,
        content: MessageContent,
    ) -> GatewayResult<MessageId> {
        let instances = self.instances.read().await;
        let instance = instances.get(instance_id).ok_or_else(|| {
            GatewayError::PluginNotFound(instance_id.to_string())
        })?;

        instance.send(target, content).await
    }

    /// Send a simple text message
    pub async fn send_text(
        &self,
        instance_id: &str,
        target: Target,
        text: impl Into<String>,
    ) -> GatewayResult<MessageId> {
        self.send(
            instance_id,
            target,
            MessageContent::text(text)
        ).await
    }

    /// Get next event
    pub async fn next_event(&self,
    ) -> Option<GatewayEvent> {
        let mut rx = self.event_rx.write().await;
        rx.recv().await
    }

    /// Get list of active instances
    pub async fn list_instances(&self,
    ) -> Vec<InstanceHandle> {
        let handles = self.handles.read().await;
        handles.values().cloned().collect()
    }

    /// Get instance handle
    pub async fn get_instance(
        &self,
        instance_id: &str,
    ) -> Option<InstanceHandle> {
        let handles = self.handles.read().await;
        handles.get(instance_id).cloned()
    }

    /// Get registry reference
    pub fn registry(&self) -> &GatewayRegistry {
        &self.registry
    }

    /// Shutdown all gateways
    pub async fn shutdown_all(&self,
    ) -> GatewayResult<()> {
        info!("Shutting down all gateways");

        let instance_ids: Vec<String> = {
            let handles = self.handles.read().await;
            handles.keys().cloned().collect()
        };

        for instance_id in instance_ids {
            if let Err(e) = self.stop_gateway(&instance_id).await {
                error!("Error shutting down gateway '{}': {}", instance_id, e);
            }
        }

        info!("All gateways shutdown");
        Ok(())
    }
}

/// Adapter that implements the old Channel trait using the new gateway system
///
/// This allows gradual migration from channels to gateways.
pub mod adapter {
    use super::*;
    use crate::channels::Channel;
    use anyhow::Result;
    use async_trait::async_trait;

    /// Adapter from GatewayPlugin to Channel trait
    pub struct GatewayChannelAdapter {
        manager: Arc<GatewayManager>,
        instance_id: String,
        default_target: Option<Target>,
    }

    impl GatewayChannelAdapter {
        /// Create a new adapter
        pub fn new(
            manager: Arc<GatewayManager>,
            instance_id: String,
            default_target: Option<Target>,
        ) -> Self {
            Self {
                manager,
                instance_id,
                default_target,
            }
        }
    }

    #[async_trait]
    impl Channel for GatewayChannelAdapter {
        fn name(&self) -> &str {
            &self.instance_id
        }

        async fn send(&mut self,
            message: &str,
        ) -> Result<()> {
            let target = self.default_target.clone()
                .ok_or_else(|| anyhow::anyhow!("No default target set"))?;

            self.manager.send_text(
                &self.instance_id,
                target,
                message
            ).await.map_err(|e| anyhow::anyhow!("{}", e))?;

            Ok(())
        }

        async fn receive(&mut self,
        ) -> Result<Option<String>> {
            // Check for events
            // In a real implementation, this would poll the event stream
            // For now, return None (non-blocking)
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = GatewaysConfig {
            cache_dir: temp_dir.path().to_string_lossy().to_string(),
            ..Default::default()
        };

        let manager = GatewayManager::new(config).await.unwrap();
        let instances = manager.list_instances().await;
        assert!(instances.is_empty());
    }
}
