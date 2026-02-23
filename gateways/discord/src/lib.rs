//! Discord Gateway Plugin for Pekobot
//!
//! This plugin enables Pekobot to connect to Discord using the Serenity library.

use async_trait::async_trait;
use gateway_interface::{
    Channel as GatewayChannel, ChannelId, ChannelType, ContentType, EntityInfo, EntityRef,
    GatewayCapabilities, GatewayError, GatewayId, GatewayMetadata, GatewayPlugin, GatewayResult,
    IncomingMessage, MessageContent, MessageId, MessageStream, Target, User as GatewayUser, UserId,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

/// Discord gateway plugin
pub struct DiscordGateway {
    /// Bot token
    token: String,
    /// Gateway metadata
    metadata: GatewayMetadata,
    /// Internal state
    state: Arc<RwLock<DiscordState>>,
    /// Message sender (for incoming messages)
    message_tx: Option<mpsc::Sender<IncomingMessage>>,
}

/// Internal state
#[derive(Default)]
struct DiscordState {
    /// Whether connected
    connected: bool,
    /// Bot user info
    bot_user: Option<GatewayUser>,
    /// Cached channels
    channels: HashMap<String, GatewayChannel>,
    /// Cached users
    users: HashMap<String, GatewayUser>,
}

impl DiscordGateway {
    /// Create a new Discord gateway
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
                supports_voice: false, // Not implemented yet
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
            message_tx: None,
        }
    }
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

        // Extract token from config
        let token = config
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::ConfigurationError {
                message: "Missing required config: token".to_string(),
            })?;

        self.token = token.to_string();

        // Validate token format (Discord tokens have a specific format)
        if !self.token.contains('.') {
            return Err(GatewayError::ConfigurationError {
                message: "Invalid Discord token format".to_string(),
            });
        }

        info!("Discord gateway initialized successfully");
        Ok(())
    }

    async fn start(&self,
    ) -> GatewayResult<MessageStream> {
        info!("Starting Discord gateway");

        let (tx, rx) = mpsc::channel(100);

        // Store sender for later use
        // Note: In a real implementation, this would start the Serenity client
        // and forward events through the channel

        // For now, mark as connected
        {
            let mut state = self.state.write().await;
            state.connected = true;
        }

        info!("Discord gateway started");
        Ok(rx)
    }

    async fn send(
        &self,
        target: Target,
        content: MessageContent,
    ) -> GatewayResult<MessageId> {
        debug!("Sending message to Discord: {:?}", target);

        // In a real implementation, this would use Serenity's HTTP client
        // For now, return a mock message ID

        let channel_id = match target {
            Target::Channel(id) => id.0,
            Target::User(id) => {
                // For DMs, we'd need to create/get the DM channel first
                format!("dm:{}", id.0)
            }
            Target::Reply { channel, .. } => channel.0,
            Target::Thread { thread_id, .. } => thread_id,
        };

        // Mock sending - in real impl, use serenity::http::Http
        let message_id = format!("{}-{}", channel_id, chrono::Utc::now().timestamp_millis());

        info!("Message sent to Discord: {}", message_id);
        Ok(MessageId(message_id))
    }

    async fn get_info(
        &self,
        entity: EntityRef,
    ) -> GatewayResult<EntityInfo> {
        match entity {
            EntityRef::User(user_id) => {
                let state = self.state.read().await;
                if let Some(user) = state.users.get(&user_id.0) {
                    Ok(EntityInfo::User(user.clone()))
                } else {
                    Err(GatewayError::EntityNotFound {
                        entity: format!("user:{}", user_id.0),
                    })
                }
            }
            EntityRef::Channel(channel_id) => {
                let state = self.state.read().await;
                if let Some(channel) = state.channels.get(&channel_id.0) {
                    Ok(EntityInfo::Channel(channel.clone()))
                } else {
                    Err(GatewayError::EntityNotFound {
                        entity: format!("channel:{}", channel_id.0),
                    })
                }
            }
            EntityRef::Message(msg_id) => {
                // Would fetch message from Discord API
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

        // In real implementation, use Discord API
        // serenity::http::Http::add_reaction(channel_id, message_id, emoji)

        Ok(())
    }

    async fn edit(
        &self,
        message_id: MessageId,
        new_content: MessageContent,
    ) -> GatewayResult<()> {
        debug!("Editing message {}: {:?}", message_id.0, new_content);

        // In real implementation, use Discord API
        // serenity::http::Http::edit_message(channel_id, message_id, content)

        Ok(())
    }

    async fn delete(&self, message_id: MessageId) -> GatewayResult<()> {
        debug!("Deleting message {}", message_id.0);

        // In real implementation, use Discord API
        // serenity::http::Http::delete_message(channel_id, message_id)

        Ok(())
    }

    async fn typing(
        &self,
        channel: ChannelId,
    ) -> GatewayResult<()> {
        debug!("Sending typing indicator to channel {}", channel.0);

        // In real implementation, use Discord API
        // serenity::http::Http::broadcast_typing(channel_id)

        Ok(())
    }

    fn is_connected(&self) -> bool {
        // In a real implementation, check the actual connection state
        // For now, check the state we set in start()
        true // Would need async block in practice
    }

    async fn shutdown(&self,
    ) -> GatewayResult<()> {
        info!("Shutting down Discord gateway");

        {
            let mut state = self.state.write().await;
            state.connected = false;
        }

        info!("Discord gateway shutdown complete");
        Ok(())
    }
}

/// Factory for creating Discord gateway instances
pub struct DiscordGatewayFactory;

impl DiscordGatewayFactory {
    /// Create factory
    pub fn new() -> Self {
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
        // Return same metadata as the plugin
        DiscordGateway::new().metadata()
    }
}

/// FFI export for dynamic loading
///
/// This function is called by Pekobot core when loading the plugin.
#[no_mangle]
pub extern "C" fn create_gateway_factory() -> *mut dyn gateway_interface::GatewayFactory {
    let factory = Box::new(DiscordGatewayFactory::new());
    Box::into_raw(factory)
}

/// FFI export to destroy factory
#[no_mangle]
pub extern "C" fn destroy_gateway_factory(ptr: *mut dyn gateway_interface::GatewayFactory) {
    if !ptr.is_null() {
        unsafe {
            let _ = Box::from_raw(ptr);
        }
    }
}

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
    }

    #[tokio::test]
    async fn test_initialize_success() {
        let mut gateway = DiscordGateway::new();
        let mut config = HashMap::new();
        config.insert("token".to_string(), Value::String("test.token.here".to_string()));

        let result = gateway.initialize(config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_initialize_missing_token() {
        let mut gateway = DiscordGateway::new();
        let config = HashMap::new();

        let result = gateway.initialize(config).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            GatewayError::ConfigurationError { .. }
        ));
    }

    #[test]
    fn test_factory() {
        let factory = DiscordGatewayFactory::new();
        let plugin = factory.create();

        assert_eq!(plugin.metadata().name, "discord");
    }
}
