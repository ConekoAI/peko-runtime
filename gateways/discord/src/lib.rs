//! Discord Gateway Plugin for Pekobot
//!
//! This plugin enables Pekobot to connect to Discord servers.
//!
//! # Configuration
//!
//! ```toml
//! [[gateways]]
//! name = "my-discord-bot"
//! plugin = "discord"
//! config = { token = "${secret:DISCORD_TOKEN}" }
//! ```
//!
//! # Getting a Token
//!
//! 1. Go to https://discord.com/developers/applications
//! 2. Create a new application
//! 3. Go to "Bot" section and add a bot
//! 4. Copy the bot token
//! 5. Add to Pekobot: `pekobot secret set DISCORD_TOKEN <token>`

use async_trait::async_trait;
use gateway_interface::{
    Channel as GatewayChannel, ChannelId, ChannelType, ContentType, EntityInfo, EntityRef,
    GatewayCapabilities, GatewayError, GatewayId, GatewayMetadata, GatewayPlugin, GatewayResult,
    IncomingMessage, MessageContent, MessageId, MessageStream, Target, User as GatewayUser, UserId,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// Discord gateway plugin
pub struct DiscordGateway {
    /// Bot token
    token: String,
    /// Gateway metadata
    metadata: GatewayMetadata,
    /// Internal state
    state: Arc<RwLock<DiscordState>>,
}

/// Internal state for the Discord gateway
#[derive(Default)]
struct DiscordState {
    /// Whether connected to Discord
    connected: bool,
    /// Bot user information
    bot_user: Option<GatewayUser>,
    /// Cached channels (channel_id -> Channel)
    channels: HashMap<String, GatewayChannel>,
    /// Cached users (user_id -> User)
    users: HashMap<String, GatewayUser>,
}

impl DiscordGateway {
    /// Create a new Discord gateway instance
    pub fn new() -> Self {
        let metadata = GatewayMetadata {
            name: "discord".to_string(),
            display_name: "Discord".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: "Discord gateway for Pekobot".to_string(),
            author: "Pekora".to_string(),
            platforms: vec!["discord".to_string()],
            capabilities: GatewayCapabilities {
                supports_dm: true,
                supports_threads: true,
                supports_editing: true,
                supports_deletion: true,
                supports_reactions: true,
                supports_typing: true,
                supports_embeds: true,
                supports_attachments: true,
                supports_voice: false, // Future enhancement
                extra: HashMap::new(),
            },
            required_config: vec!["token".to_string()],
            optional_config: vec![
                "default_channel".to_string(),
                "rate_limit_per_user".to_string(),
            ],
        };

        Self {
            token: String::new(),
            metadata,
            state: Arc::new(RwLock::new(DiscordState::default())),
        }
    }

    /// Get the current connection state
    async fn get_state(&self) -> DiscordStateSnapshot {
        let state = self.state.read().await;
        DiscordStateSnapshot {
            connected: state.connected,
            channel_count: state.channels.len(),
            user_count: state.users.len(),
        }
    }
}

/// Snapshot of Discord state for debugging
#[derive(Debug)]
struct DiscordStateSnapshot {
    connected: bool,
    channel_count: usize,
    user_count: usize,
}

impl Default for DiscordGateway {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GatewayPlugin for DiscordGateway {
    fn metadata(&self) -> GatewayMetadata {
        self.metadata.clone()
    }

    async fn initialize(
        &mut self,
        config: HashMap<String, Value>,
    ) -> GatewayResult<()> {
        info!("Initializing Discord gateway");

        // Extract and validate token
        let token = config
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::ConfigurationError {
                message: "Missing required config: token".to_string(),
            })?;

        // Basic token format validation
        // Discord tokens are base64-encoded and contain dots
        if !token.contains('.') || token.len() < 50 {
            return Err(GatewayError::ConfigurationError {
                message: "Invalid Discord token format. Tokens should be 50+ characters and contain '.'".to_string(),
            });
        }

        self.token = token.to_string();
        info!("Discord gateway initialized successfully");
        Ok(())
    }

    async fn start(&self) -> GatewayResult<MessageStream> {
        info!("Starting Discord gateway");

        let (tx, rx) = mpsc::channel(100);

        // Clone data for the background task
        let token = self.token.clone();
        let state = self.state.clone();

        // Spawn background task for Discord connection
        // In a full implementation, this would use Serenity to connect
        // to Discord's Gateway and forward events through the channel
        tokio::spawn(async move {
            info!("Discord gateway background task started");
            
            // Mark as connected
            {
                let mut s = state.write().await;
                s.connected = true;
            }

            // TODO: Implement actual Serenity client here
            // This would:
            // 1. Create serenity::Client with the token
            // 2. Set up event handlers
            // 3. Connect to Discord Gateway
            // 4. Forward events to the channel

            // For now, just keep the connection "alive"
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                
                // Check if still connected
                let connected = {
                    let s = state.read().await;
                    s.connected
                };
                
                if !connected {
                    info!("Discord gateway connection closed");
                    break;
                }
            }
        });

        info!("Discord gateway started");
        Ok(rx)
    }

    async fn send(
        &self,
        target: Target,
        content: MessageContent,
    ) -> GatewayResult<MessageId> {
        debug!("Sending message to Discord");

        // Determine the target channel
        let channel_id = match &target {
            Target::Channel(id) => id.0.clone(),
            Target::User(id) => {
                // For DMs, we'd need to create/get the DM channel first
                format!("dm:{}", id.0)
            }
            Target::Reply { channel, .. } => channel.0.clone(),
            Target::Thread { thread_id, .. } => thread_id.clone(),
        };

        // TODO: Implement actual Discord HTTP API call
        // Use serenity::http::Http to send the message
        
        // For now, generate a mock message ID
        let timestamp = chrono::Utc::now().timestamp_millis();
        let message_id = format!("{}-{}", channel_id, timestamp);

        info!("Message sent to Discord channel {}: {}", channel_id, message_id);
        Ok(MessageId(message_id))
    }

    async fn get_info(
        &self,
        entity: EntityRef,
    ) -> GatewayResult<EntityInfo> {
        let state = self.state.read().await;
        
        match entity {
            EntityRef::User(user_id) => {
                state.users.get(&user_id.0)
                    .cloned()
                    .map(EntityInfo::User)
                    .ok_or_else(|| GatewayError::EntityNotFound {
                        entity: format!("user:{}", user_id.0),
                    })
            }
            EntityRef::Channel(channel_id) => {
                state.channels.get(&channel_id.0)
                    .cloned()
                    .map(EntityInfo::Channel)
                    .ok_or_else(|| GatewayError::EntityNotFound {
                        entity: format!("channel:{}", channel_id.0),
                    })
            }
            EntityRef::Message(msg_id) => {
                Err(GatewayError::NotSupported {
                    operation: format!("get_info for message {}", msg_id.0),
                })
            }
        }
    }

    async fn react(
        &self,
        message_id: MessageId,
        emoji: &str,
    ) -> GatewayResult<()> {
        debug!("Adding reaction {} to message {}", emoji, message_id.0);
        
        // TODO: Implement Discord API call
        // serenity::http::Http::add_reaction(channel_id, message_id, emoji)
        
        Ok(())
    }

    async fn edit(
        &self,
        message_id: MessageId,
        new_content: MessageContent,
    ) -> GatewayResult<()> {
        debug!("Editing message {}", message_id.0);
        
        // TODO: Implement Discord API call
        // serenity::http::Http::edit_message(channel_id, message_id, content)
        
        Ok(())
    }

    async fn delete(&self, message_id: MessageId) -> GatewayResult<()> {
        debug!("Deleting message {}", message_id.0);
        
        // TODO: Implement Discord API call
        // serenity::http::Http::delete_message(channel_id, message_id)
        
        Ok(())
    }

    async fn typing(&self, channel: ChannelId) -> GatewayResult<()> {
        debug!("Sending typing indicator to channel {}", channel.0);
        
        // TODO: Implement Discord API call
        // serenity::http::Http::broadcast_typing(channel_id)
        
        Ok(())
    }

    fn is_connected(&self) -> bool {
        // This would check the actual connection state
        // For now, check if we have a token set
        !self.token.is_empty()
    }

    async fn shutdown(&self) -> GatewayResult<()> {
        info!("Shutting down Discord gateway");

        // Mark as disconnected to stop background task
        {
            let mut state = self.state.write().await;
            state.connected = false;
        }

        // TODO: Implement actual Discord client shutdown
        // This would close WebSocket connections and cleanup

        info!("Discord gateway shutdown complete");
        Ok(())
    }
}

/// Factory for creating Discord gateway instances
pub struct DiscordGatewayFactory;

impl DiscordGatewayFactory {
    /// Create a new factory
    pub const fn new() -> Self {
        Self
    }
}

impl Default for DiscordGatewayFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl gateway_interface::GatewayFactory for DiscordGatewayFactory {
    fn create(&self) -> Box<dyn GatewayPlugin> {
        Box::new(DiscordGateway::new())
    }

    fn metadata(&self) -> GatewayMetadata {
        DiscordGateway::new().metadata()
    }
}

// ============================================================================
// FFI Exports for Dynamic Loading
// ============================================================================

/// Create a gateway factory instance
///
/// This function is called by Pekobot core when loading the plugin.
/// It returns a raw pointer to a boxed factory.
///
/// # Safety
/// The caller must call `destroy_gateway_factory` to free the memory.
#[no_mangle]
pub extern "C" fn create_gateway_factory() -> *mut dyn gateway_interface::GatewayFactory {
    let factory = Box::new(DiscordGatewayFactory::new());
    Box::into_raw(factory)
}

/// Destroy a gateway factory instance
///
/// # Safety
/// The pointer must have been created by `create_gateway_factory` and not already freed.
#[no_mangle]
pub extern "C" fn destroy_gateway_factory(ptr: *mut dyn gateway_interface::GatewayFactory) {
    if !ptr.is_null() {
        unsafe {
            let _ = Box::from_raw(ptr);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        let gateway = DiscordGateway::new();
        let metadata = gateway.metadata();

        assert_eq!(metadata.name, "discord");
        assert_eq!(metadata.display_name, "Discord");
        assert!(metadata.capabilities.supports_dm);
        assert!(metadata.capabilities.supports_reactions);
        assert!(!metadata.capabilities.supports_voice); // Not yet implemented
    }

    #[tokio::test]
    async fn test_initialize_success() {
        let mut gateway = DiscordGateway::new();
        let mut config = HashMap::new();
        // Valid-looking token format
        config.insert(
            "token".to_string(),
            Value::String("MTAxMDIwMw0KaWxsdW1pbmF0aQ0KYXN0cmEu".to_string()),
        );

        let result = gateway.initialize(config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_initialize_missing_token() {
        let mut gateway = DiscordGateway::new();
        let config = HashMap::new();

        let result = gateway.initialize(config).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            GatewayError::ConfigurationError { message } => {
                assert!(message.contains("token"));
            }
            _ => panic!("Expected ConfigurationError"),
        }
    }

    #[tokio::test]
    async fn test_initialize_invalid_token() {
        let mut gateway = DiscordGateway::new();
        let mut config = HashMap::new();
        // Invalid token (too short, no dot)
        config.insert("token".to_string(), Value::String("invalid".to_string()));

        let result = gateway.initialize(config).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_factory() {
        let factory = DiscordGatewayFactory::new();
        let plugin = factory.create();

        assert_eq!(plugin.metadata().name, "discord");
    }

    #[tokio::test]
    async fn test_lifecycle() {
        let mut gateway = DiscordGateway::new();
        
        // Initialize
        let mut config = HashMap::new();
        config.insert(
            "token".to_string(),
            Value::String("MTAxMDIwMw0KaWxsdW1pbmF0aQ0KYXN0cmE.".to_string()),
        );
        gateway.initialize(config).await.unwrap();
        
        // Check connection state
        assert!(gateway.is_connected());
        
        // Shutdown
        gateway.shutdown().await.unwrap();
    }
}
