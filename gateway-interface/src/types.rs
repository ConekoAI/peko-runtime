//! Gateway interface types
//!
//! This crate defines the shared interface between Pekobot core
//! and gateway plugins. It must remain stable to ensure compatibility.

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

/// Unique identifier for a channel/chat
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelId(pub String);

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a user
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Gateway instance ID
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GatewayId(pub String);

/// Reference to an entity
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityRef {
    User(UserId),
    Channel(ChannelId),
    Message(MessageId),
}

/// Target for sending a message
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Target {
    Channel(ChannelId),
    User(UserId),
    Reply {
        channel: ChannelId,
        message: MessageId,
    },
    Thread {
        channel: ChannelId,
        thread_id: String,
    },
}

/// Content type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentType {
    Text,
    Markdown,
    Html,
    Image,
    File,
    Audio,
    Video,
    Embed,
    System,
}

/// Attachment
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    pub url: Option<String>,
    pub data: Option<String>,
    pub description: Option<String>,
}

/// Message content
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageContent {
    pub content_type: ContentType,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl MessageContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content_type: ContentType::Text,
            text: text.into(),
            attachments: Vec::new(),
            metadata: HashMap::new(),
        }
    }
}

/// Incoming message
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncomingMessage {
    pub id: MessageId,
    pub gateway: GatewayId,
    pub channel: ChannelId,
    pub sender: User,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub content: MessageContent,
    pub reply_to: Option<MessageId>,
    pub thread_id: Option<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Outgoing message
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutgoingMessage {
    pub target: Target,
    pub content: MessageContent,
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

/// Channel information
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub channel_type: ChannelType,
    pub parent_id: Option<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Channel type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelType {
    DirectMessage,
    Group,
    Text,
    Voice,
    Thread,
    Category,
    Other(String),
}

/// Entity info
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntityInfo {
    User(User),
    Channel(Channel),
}

/// Gateway capabilities
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayCapabilities {
    pub supports_dm: bool,
    pub supports_threads: bool,
    pub supports_editing: bool,
    pub supports_deletion: bool,
    pub supports_reactions: bool,
    pub supports_typing: bool,
    pub supports_embeds: bool,
    pub supports_attachments: bool,
    pub supports_voice: bool,
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

/// Gateway metadata
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayMetadata {
    pub name: String,
    pub display_name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub platforms: Vec<String>,
    pub capabilities: GatewayCapabilities,
    pub required_config: Vec<String>,
    pub optional_config: Vec<String>,
}

/// Message stream
pub type MessageStream = tokio::sync::mpsc::Receiver<IncomingMessage>;
