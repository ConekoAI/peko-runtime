//! Gateway Interface
//!
//! Shared interface definitions for Pekobot gateway plugins.
//! This crate must remain stable to ensure compatibility between
//! Pekobot core and gateway plugins.

pub mod error;
pub mod interface;
pub mod types;

pub use error::{GatewayError, GatewayResult};
pub use interface::{GatewayFactory, GatewayPlugin, GATEWAY_API_VERSION};
pub use types::{
    Attachment, Channel, ChannelId, ChannelType, ContentType, EntityInfo, EntityRef,
    GatewayCapabilities, GatewayId, GatewayMetadata, IncomingMessage, MessageContent, MessageId,
    MessageStream, OutgoingMessage, Target, User, UserId,
};

/// Re-export async_trait for plugin implementations
pub use async_trait::async_trait;
