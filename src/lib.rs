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
//! ```bash,ignore
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

// ============================================================================
// Core Runtime
// ============================================================================

/// Agent runtime and lifecycle
pub mod agent;

/// Execution engine and state machine
pub mod engine;

/// Message queue with lane-aware processing
pub(crate) mod queue;

/// Session management and lifecycle
pub(crate) mod session;

// ============================================================================
// External Interfaces
// ============================================================================

/// Communication channels (Discord, Slack, Telegram, etc.)
pub mod channels;

/// Gateway plugin system
pub mod gateway;

/// LLM provider integrations
pub mod providers;

/// Tool registry and management
pub mod tool_registry;

// ============================================================================
// Data & State
// ============================================================================

/// Type definitions
pub mod types;

/// Configuration management
pub mod config;

/// Memory systems (SQLite, vector, hybrid)
pub mod memory;

/// Agent identity and key management
pub mod identity;

// ============================================================================
// Infrastructure
// ============================================================================

/// Agent manager (lifecycle, pool, registry)
pub mod manager;

/// Cron job scheduling
pub(crate) mod cron;

/// Daemon mode for background execution
pub(crate) mod daemon;

/// Security policies and sandboxing
pub mod security;

/// Observability (metrics, tracing, audit)
pub(crate) mod observability;

// ============================================================================
// Tools & Skills
// ============================================================================

/// Tool implementations (filesystem, http, browser, etc.)
pub mod tools;

/// Skill system
pub(crate) mod skills;

/// Capability registry for agent discovery
pub(crate) mod capability_registry;

// ============================================================================
// CLI & Commands
// ============================================================================

/// CLI command handlers
pub mod commands;

/// Prompt generation and bootstrap
pub(crate) mod prompt;

// ============================================================================
// Utilities
// ============================================================================

/// Portable agent packaging (export/import)
pub mod portable;

/// Compaction and transcript management
pub(crate) mod compaction;

/// Tunnel for remote access
pub(crate) mod tunnel;

// ============================================================================
// Public API
// ============================================================================

pub use agent::Agent;
pub use config::Config;

// Re-export event types for tool monitoring and streaming
pub use engine::{AgenticEvent, LifecyclePhase};

/// Pekobot version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
