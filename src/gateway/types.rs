//! Gateway plugin interface and types
//!
//! This module defines the core abstractions that all gateway plugins must implement.
//! It enables Pekobot to communicate with any messaging platform through a unified interface.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Unique identifier for a message
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub String);

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a channel/chat/conversation
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelId(pub String);

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a user/sender
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a gateway instance
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GatewayId(pub String);

impl fmt::Display for GatewayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Reference to an entity (user, channel, or message)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityRef {
    User(UserId),
    Channel(ChannelId),
    Message(MessageId),
}

/// Target for sending a message
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Target {
    /// Send to a specific channel/chat
    Channel(ChannelId),
    /// Send as a direct message to a user
    User(UserId),
    /// Reply to a specific message
    Reply { 
        channel: ChannelId, 
        message: MessageId 
    },
    /// Thread reply (for platforms supporting threads)
    Thread { 
        channel: ChannelId, 
        thread_id: String 
    },
}

/// Type of message content
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentType {
    /// Plain text
    Text,
    /// Markdown formatted text
    Markdown,
    /// HTML formatted text
    Html,
    /// Image attachment
    Image,
    /// File attachment
    File,
    /// Audio attachment
    Audio,
    /// Video attachment
    Video,
    /// Rich embed/card
    Embed,
    /// System/notification message
    System,
}

/// Attachment (file, image, etc.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// Unique identifier for this attachment
    pub id: String,
    /// Filename
    pub filename: String,
    /// MIME type
    pub content_type: String,
    /// Size in bytes
    pub size: usize,
    /// URL to download (if available)
    pub url: Option<String>,
    /// Base64-encoded data (for small attachments)
    pub data: Option<String>,
    /// Description/alt text
    pub description: Option<String>,
}

/// Message content for sending
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageContent {
    /// Content type
    pub content_type: ContentType,
    /// Main text content
    pub text: String,
    /// Optional attachments
    pub attachments: Vec<Attachment>,
    /// Platform-specific metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

impl MessageContent {
    /// Create simple text content
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content_type: ContentType::Text,
            text: text.into(),
            attachments: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Create markdown content
    pub fn markdown(text: impl Into<String>) -> Self {
        Self {
            content_type: ContentType::Markdown,
            text: text.into(),
            attachments: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add an attachment
    pub fn with_attachment(mut self, attachment: Attachment) -> Self {
        self.attachments.push(attachment);
        self
    }
}

/// Incoming message from a gateway
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingMessage {
    /// Unique message ID (platform-specific)
    pub id: MessageId,
    /// Which gateway received this
    pub gateway: GatewayId,
    /// Which channel/chat
    pub channel: ChannelId,
    /// Who sent it
    pub sender: User,
    /// When it was sent
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Message content
    pub content: MessageContent,
    /// If this is a reply, the original message
    pub reply_to: Option<MessageId>,
    /// For threaded platforms
    pub thread_id: Option<String>,
    /// Platform-specific metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Outgoing message to be sent
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingMessage {
    /// Target for this message
    pub target: Target,
    /// Content to send
    pub content: MessageContent,
    /// Platform-specific options
    pub options: HashMap<String, serde_json::Value>,
}

/// User information
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub is_bot: bool,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Channel/Chat information
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub channel_type: ChannelType,
    pub parent_id: Option<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Type of channel
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelType {
    DirectMessage,
    Group,
    Text,
    Voice,
    Thread,
    Category,
    /// Platform-specific type
    Other(String),
}

/// Entity information (returned by get_info)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntityInfo {
    User(User),
    Channel(Channel),
    Message(MessageInfo),
}

/// Message metadata (for get_info on messages)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageInfo {
    pub id: MessageId,
    pub channel: ChannelId,
    pub sender: User,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub content_preview: String,
    pub reactions: Vec<Reaction>,
}

/// Reaction/emoji on a message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub users: Vec<UserId>,
}

/// Capabilities that a gateway supports
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayCapabilities {
    /// Can send direct messages to users
    pub supports_dm: bool,
    /// Supports threaded conversations
    pub supports_threads: bool,
    /// Can edit sent messages
    pub supports_editing: bool,
    /// Can delete messages
    pub supports_deletion: bool,
    /// Supports reactions/emoji
    pub supports_reactions: bool,
    /// Supports typing indicators
    pub supports_typing: bool,
    /// Supports rich embeds/cards
    pub supports_embeds: bool,
    /// Supports file attachments
    pub supports_attachments: bool,
    /// Supports voice/video calls
    pub supports_voice: bool,
    /// Platform-specific capabilities
    pub extra: HashMap<String, bool>,
}

impl Default for GatewayCapabilities {
    fn default() -> Self {
        Self {
            supports_dm: true,
            supports_threads: false,
            supports_editing: false,
            supports_deletion: false,
            supports_reactions: false,
            supports_typing: false,
            supports_embeds: false,
            supports_attachments: false,
            supports_voice: false,
            extra: HashMap::new(),
        }
    }
}

/// Metadata about a gateway plugin
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayMetadata {
    /// Unique name (e.g., "discord", "whatsapp")
    pub name: String,
    /// Human-readable name
    pub display_name: String,
    /// Version (semver)
    pub version: String,
    /// Description
    pub description: String,
    /// Author
    pub author: String,
    /// Supported platforms
    pub platforms: Vec<String>,
    /// Capabilities
    pub capabilities: GatewayCapabilities,
    /// Required configuration fields
    pub required_config: Vec<String>,
    /// Optional configuration fields
    pub optional_config: Vec<String>,
}

/// Stream of incoming messages
pub type MessageStream = tokio::sync::mpsc::Receiver<IncomingMessage>;
