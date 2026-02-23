//! Gateway plugin system
//!
//! This module provides a unified interface for messaging platform integration.
//! All channels (Discord, WhatsApp, etc.) are implemented as plugins that
//! implement the `GatewayPlugin` trait.
//!
//! # Architecture
//!
//! ```
//! ┌─────────────────────────────────────────┐
//! │           Pekobot Core                  │
//! │  ┌─────────────────────────────────┐    │
//! │  │      GatewayManager             │    │
//! │  │  ┌───────────────────────────┐  │    │
//! │  │  │   GatewayRegistry         │  │    │
//! │  │  │  ┌─────────────────────┐  │  │    │
//! │  │  │  │ GatewayPlugin trait │  │  │    │
//! │  │  │  └─────────────────────┘  │  │    │
//! │  │  └───────────────────────────┘  │    │
//! │  └─────────────────────────────────┘    │
//! └──────────────────┬──────────────────────┘
//!                    │ loads
//! ┌──────────────────▼──────────────────────┐
//! │         Gateway Plugins (.gateway)      │
//! │  ┌──────────┐ ┌──────────┐ ┌─────────┐ │
//! │  │ discord  │ │ whatsapp │ │  slack  │ │
//! │  └──────────┘ └──────────┘ └─────────┘ │
//! └─────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ## Loading a gateway
//!
//! ```rust,no_run
//! use pekobot::gateway::{GatewayRegistry, GatewayConfig};
//!
//! # async fn example() {
//! let mut registry = GatewayRegistry::new();
//!
//! // Load from Pekohub or local cache
//! registry.load("discord").await.unwrap();
//!
//! // Create and initialize instance
//! let config = GatewayConfig {
//!     name: "my-bot".to_string(),
//!     plugin: "discord".to_string(),
//!     config: std::collections::HashMap::new(),
//!     enabled: true,
//!     ..Default::default()
//! };
//! # }
//! ```

// Re-export from gateway-interface crate
pub use gateway_interface::{
    error, interface, types, async_trait, GatewayCapabilities, GatewayError,
    GatewayFactory, GatewayId, GatewayMetadata, GatewayPlugin, GatewayResult, Target,
    MessageId, ChannelId, UserId, EntityRef, EntityInfo, IncomingMessage, OutgoingMessage,
    MessageContent, MessageStream, ContentType, Attachment, User, Channel, ChannelType,
    GATEWAY_API_VERSION,
};

// Local modules
pub mod config;
pub mod loader;
pub mod manager;
pub mod registry;

// Re-export config types
pub use config::{
    BinaryDownloads, ConfigField, ConfigSchema, FilterAction, FilterCondition, FilterRule,
    GatewayConfig, GatewayInfo, GatewaysConfig, PluginInfo, PluginManifest, RateLimitConfig,
    RetryConfig,
};

// Re-export loader types
pub use loader::{platform, PluginHandle, PluginLoader};

// Re-export registry types
pub use registry::GatewayRegistry;

// Re-export manager types
pub use manager::{GatewayEvent, GatewayManager, InstanceHandle};
