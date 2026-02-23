//! Core gateway plugin interface
//!
//! This module defines the `GatewayPlugin` trait that all gateway implementations must satisfy.
//! It enables Pekobot to communicate with any messaging platform through a unified abstraction.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::gateway::error::{GatewayError, GatewayResult};
use crate::gateway::types::{
    EntityInfo, EntityRef, GatewayCapabilities, GatewayMetadata, IncomingMessage,
    MessageContent, MessageId, MessageStream, OutgoingMessage, Target,
};

/// API version for gateway plugins
/// 
/// Plugins must declare compatibility with this version.
/// Bump this when making breaking changes to the trait interface.
pub const GATEWAY_API_VERSION: &str = "1.0.0";

/// Core trait that all gateway plugins must implement
///
/// This trait abstracts over all messaging platforms (Discord, WhatsApp, etc.)
/// allowing Pekobot to interact with them uniformly.
///
/// # Implementation Notes
///
/// - All methods are async to support network I/O
/// - Implementations must be Send + Sync for use across tasks
/// - Errors should be specific and actionable
#[async_trait]
pub trait GatewayPlugin: Send + Sync {
    /// Get metadata about this gateway
    ///
    /// This should return static information about the plugin:
    /// - Name and version
    /// - Supported capabilities
    /// - Configuration requirements
    fn metadata(&self) -> GatewayMetadata;

    /// Check if this plugin supports a given API version
    ///
    /// Default implementation checks exact match, but plugins
    /// can override to support multiple versions.
    fn supports_api_version(&self, version: &str) -> bool {
        version == GATEWAY_API_VERSION
    }

    /// Initialize the gateway with configuration
    ///
    /// Called once before the gateway is started. Use this to:
    /// - Validate configuration
    /// - Set up authentication
    /// - Connect to platform APIs
    /// - Initialize internal state
    ///
    /// # Arguments
    /// * `config` - Plugin-specific configuration values
    ///
    /// # Errors
    /// Returns `GatewayError::InitializationFailed` if setup fails
    async fn initialize(
        &mut self,
        config: HashMap<String, Value>,
    ) -> GatewayResult<()>;

    /// Start listening for incoming messages
    ///
    /// This method should:
    /// 1. Establish connection to the platform
    /// 2. Start any background tasks for receiving
    /// 3. Return a stream that yields incoming messages
    ///
    /// The returned stream should remain active until `shutdown` is called.
    ///
    /// # Errors
    /// Returns `GatewayError::ConnectionFailed` if connection cannot be established
    async fn start(&self,
    ) -> GatewayResult<MessageStream>;

    /// Send a message through this gateway
    ///
    /// # Arguments
    /// * `target` - Where to send the message (channel, user, or reply)
    /// * `content` - What to send
    ///
    /// # Returns
    /// The platform-specific message ID
    ///
    /// # Errors
    /// - `GatewayError::SendFailed` if message cannot be sent
    /// - `GatewayError::NotSupported` if target type not supported
    /// - `GatewayError::RateLimited` if rate limited
    async fn send(
        &self,
        target: Target,
        content: MessageContent,
    ) -> GatewayResult<MessageId>;

    /// Send a message and wait for a reply
    ///
    /// This is a convenience method for request-response patterns.
    /// Default implementation sends and sets up a one-time listener.
    ///
    /// # Arguments
    /// * `target` - Where to send
    /// * `content` - What to send
    /// * `timeout_secs` - How long to wait for reply
    ///
    /// # Returns
    /// The reply message, or None if timeout
    ///
    /// # Errors
    /// Same as `send`, plus `GatewayError::Timeout`
    async fn send_and_wait(
        &self,
        target: Target,
        content: MessageContent,
        timeout_secs: u64,
    ) -> GatewayResult<Option<IncomingMessage>> {
        // Default implementation
        let _ = self.send(target, content).await?;
        // In real implementation, would set up reply listener
        // For now, return None (subclasses can override)
        Ok(None)
    }

    /// Get information about an entity (user, channel, or message)
    ///
    /// # Arguments
    /// * `entity` - Reference to the entity
    ///
    /// # Errors
    /// - `GatewayError::EntityNotFound` if entity doesn't exist
    /// - `GatewayError::NotSupported` if operation not supported
    async fn get_info(
        &self,
        entity: EntityRef,
    ) -> GatewayResult<EntityInfo>;

    /// React to a message with an emoji
    ///
    /// # Arguments
    /// * `message_id` - Message to react to
    /// * `emoji` - Emoji to react with (Unicode or custom ID)
    ///
    /// # Errors
    /// - `GatewayError::NotSupported` if reactions not supported
    async fn react(
        &self,
        message_id: MessageId,
        emoji: &str,
    ) -> GatewayResult<()> {
        let _ = (message_id, emoji);
        Err(GatewayError::NotSupported {
            gateway: self.metadata().name,
            operation: "react".to_string(),
        })
    }

    /// Edit a previously sent message
    ///
    /// # Arguments
    /// * `message_id` - Message to edit
    /// * `new_content` - New content
    ///
    /// # Errors
    /// - `GatewayError::NotSupported` if editing not supported
    async fn edit(
        &self,
        message_id: MessageId,
        new_content: MessageContent,
    ) -> GatewayResult<()> {
        let _ = (message_id, new_content);
        Err(GatewayError::NotSupported {
            gateway: self.metadata().name,
            operation: "edit".to_string(),
        })
    }

    /// Delete a message
    ///
    /// # Arguments
    /// * `message_id` - Message to delete
    ///
    /// # Errors
    /// - `GatewayError::NotSupported` if deletion not supported
    async fn delete(&self, message_id: MessageId) -> GatewayResult<()> {
        let _ = message_id;
        Err(GatewayError::NotSupported {
            gateway: self.metadata().name,
            operation: "delete".to_string(),
        })
    }

    /// Send typing indicator
    ///
    /// # Arguments
    /// * `channel` - Channel to show typing in
    ///
    /// # Errors
    /// - `GatewayError::NotSupported` if typing indicators not supported
    async fn typing(
        &self,
        channel: crate::gateway::types::ChannelId,
    ) -> GatewayResult<()> {
        let _ = channel;
        Err(GatewayError::NotSupported {
            gateway: self.metadata().name,
            operation: "typing".to_string(),
        })
    }

    /// Get gateway capabilities
    ///
    /// Default implementation returns metadata capabilities,
    /// but dynamic gateways can override to report runtime capabilities.
    fn capabilities(&self) -> GatewayCapabilities {
        self.metadata().capabilities
    }

    /// Check if gateway is currently connected
    ///
    /// Default implementation assumes connected after successful `start`.
    fn is_connected(&self) -> bool {
        true
    }

    /// Shutdown the gateway cleanly
    ///
    /// This should:
    /// 1. Stop accepting new messages
    /// 2. Flush any pending sends
    /// 3. Close connections
    /// 4. Clean up resources
    ///
    /// # Errors
    /// Errors during shutdown are logged but generally ignored
    async fn shutdown(&self,
    ) -> GatewayResult<()>;
}

/// Factory for creating gateway instances
///
/// Each plugin exports a factory that can create gateway instances.
/// This allows the core to instantiate gateways without knowing concrete types.
pub trait GatewayFactory: Send + Sync {
    /// Create a new gateway instance
    fn create(&self) -> Box<dyn GatewayPlugin>;

    /// Get metadata without creating instance
    fn metadata(&self) -> GatewayMetadata;

    /// Get the API version this factory produces
    fn api_version(&self) -> &str {
        GATEWAY_API_VERSION
    }
}

/// Type alias for the factory export function
///
/// Plugins export a function with this signature:
/// ```rust
/// #[no_mangle]
/// pub extern "C" fn create_gateway_factory() -> *mut dyn GatewayFactory {
///     // ...
/// }
/// ```
pub type CreateGatewayFactoryFn = extern "C" fn() -> *mut dyn GatewayFactory;

/// Helper trait for downcasting gateway plugins
///
/// This allows concrete types to be retrieved when needed
/// for platform-specific operations.
pub trait GatewayExt {
    /// Attempt to downcast to a concrete type
    fn downcast_ref<T: GatewayPlugin>(&self,
    ) -> Option<&T>;
}

impl GatewayExt for dyn GatewayPlugin {
    fn downcast_ref<T: GatewayPlugin>(&self,
    ) -> Option<&T> {
        // This is a placeholder - in practice, use Any downcasting
        // or provide platform-specific accessor methods
        None
    }
}
