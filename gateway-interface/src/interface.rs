//! Gateway plugin interface

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::error::{GatewayError, GatewayResult};
use crate::types::{
    ChannelId, EntityInfo, EntityRef, GatewayCapabilities, GatewayMetadata, IncomingMessage,
    MessageContent, MessageId, MessageStream, Target,
};

/// API version
pub const GATEWAY_API_VERSION: &str = "1.0.0";

/// Core trait for gateway plugins
#[async_trait]
pub trait GatewayPlugin: Send + Sync {
    /// Get metadata
    fn metadata(&self) -> GatewayMetadata;

    /// Check API version support
    fn supports_api_version(&self, version: &str) -> bool {
        version == GATEWAY_API_VERSION
    }

    /// Initialize with config
    async fn initialize(&mut self, config: HashMap<String, Value>) -> GatewayResult<()>;

    /// Start listening
    async fn start(&self) -> GatewayResult<MessageStream>;

    /// Send message
    async fn send(&self, target: Target, content: MessageContent) -> GatewayResult<MessageId>;

    /// Send and wait for reply
    async fn send_and_wait(
        &self,
        target: Target,
        content: MessageContent,
        _timeout_secs: u64,
    ) -> GatewayResult<Option<IncomingMessage>> {
        let _ = self.send(target, content).await?;
        Ok(None)
    }

    /// Get entity info
    async fn get_info(&self, entity: EntityRef) -> GatewayResult<EntityInfo>;

    /// React to message
    async fn react(&self, message_id: MessageId, emoji: &str) -> GatewayResult<()> {
        let _ = (message_id, emoji);
        Err(GatewayError::NotSupported {
            operation: "react".to_string(),
        })
    }

    /// Edit message
    async fn edit(&self, message_id: MessageId, new_content: MessageContent) -> GatewayResult<()> {
        let _ = (message_id, new_content);
        Err(GatewayError::NotSupported {
            operation: "edit".to_string(),
        })
    }

    /// Delete message
    async fn delete(&self, message_id: MessageId) -> GatewayResult<()> {
        let _ = message_id;
        Err(GatewayError::NotSupported {
            operation: "delete".to_string(),
        })
    }

    /// Send typing indicator
    async fn typing(&self, channel: ChannelId) -> GatewayResult<()> {
        let _ = channel;
        Err(GatewayError::NotSupported {
            operation: "typing".to_string(),
        })
    }

    /// Get capabilities
    fn capabilities(&self) -> GatewayCapabilities {
        self.metadata().capabilities
    }

    /// Check connection status
    fn is_connected(&self) -> bool {
        true
    }

    /// Shutdown
    async fn shutdown(&self) -> GatewayResult<()>;
}

/// Factory for creating gateway instances
pub trait GatewayFactory: Send + Sync {
    /// Create instance
    fn create(&self) -> Box<dyn GatewayPlugin>;

    /// Get metadata
    fn metadata(&self) -> GatewayMetadata;

    /// API version
    fn api_version(&self) -> &str {
        GATEWAY_API_VERSION
    }
}
