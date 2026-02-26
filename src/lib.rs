//! Pekobot - Lightweight Multi-Agent Runtime
//!
//! A Rust-based agent runtime with pluggable gateway support for multi-platform messaging.

#![allow(
    dead_code,
    unused_async,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::unused_self,
    clippy::format_push_string,
    clippy::unnecessary_debug_formatting,
    clippy::pass_by_ref_mut
)]

//! ## Architecture
//!
//! Pekobot uses a minimal core (~500KB-1MB) with on-demand loaded plugins:
//!
//! - **Core**: Agent runtime, state machine, tool registry
//! - **Gateways**: Pluggable messaging platform adapters (Discord, etc.)
//! - **Tools**: On-demand tool plugins (same system as gateways)
//!
//! ## Quick Start
//!
//! ```bash
//! # Install a gateway plugin
//! pekobot gateway install discord
//!
//! # Run single agent
//! pekobot agent
//!
//! # See all options
//! pekobot --help
//! ```
//!
//! ## Gateway Plugin System
//!
//! Gateways are dynamic libraries that implement the `GatewayPlugin` trait:
//!
//! ```rust,ignore
//! use pekobot::gateway::{GatewayManager, GatewayConfig};
//!
//! async fn example() {
//!     let manager = GatewayManager::new(config).await.unwrap();
//!     
//!     // Load and start Discord gateway
//!     manager.registry().load("discord").await.unwrap();
//!     
//!     // Create instance
//!     let instance = manager.registry()
//!         .create_instance("discord", config)
//!         .await
//!         .unwrap();
//!     
//!     // Start receiving messages
//!     let stream = instance.start().await.unwrap();
//! }
//! ```

#![warn(clippy::all, clippy::pedantic)]

pub mod agent;
pub mod capability_registry;
pub mod channels;
pub mod commands;
pub mod compaction;
pub mod config;
pub mod cron;
pub mod daemon;
pub mod engine;
pub mod gateway;
pub mod identity;
pub mod manager;
pub mod memory;
pub mod observability;
pub mod portable;
pub mod providers;
pub mod security;
pub mod session;
pub mod skills;
pub mod tool_registry;
pub mod tools;
pub mod tunnel;
pub mod types;

pub use agent::Agent;
pub use config::Config;

/// Pekobot version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
