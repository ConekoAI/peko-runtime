//! Pekobot - Lightweight Multi-Agent Runtime
//!
//! A Rust-based agent runtime with pluggable gateway support for multi-platform messaging.

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
// Common Utilities
// ============================================================================

/// Common utilities shared across CLI and API
pub mod common;

// ============================================================================
// Core Runtime
// ============================================================================

/// Agent runtime, lifecycle, and multi-agent management
pub mod agent;

/// Execution engine and state machine
pub mod engine;

/// Message queue with lane-aware processing
pub(crate) mod queue;

/// Session storage (JSONL)
pub mod session;

/// Orchestration layer (event router, file watcher, webhooks)
pub mod orchestration;

/// Team runtime (multi-agent teams, event bus, shared services)
pub mod team;

/// File watcher for development mode
pub mod watcher;

// ============================================================================
// External Interfaces
// ============================================================================

/// Gateway plugin system and built-in channel implementations
pub mod gateway;

/// Built-in communication channels (CLI, HTTP, etc.)
pub mod channels;

/// LLM provider integrations
pub mod providers;

/// Tool registry and management
pub mod tool_registry;

/// Unified capability framework (tools, MCP, skills)
pub mod cap;

/// MCP (Model Context Protocol) support
pub mod mcp;

/// Unified Extension Architecture (ADR-017)
pub mod extensions;

// ============================================================================
// Data & State
// ============================================================================

/// Type definitions
pub mod types;

/// Configuration management
pub mod config;

/// Agent identity and key management
pub mod identity;

// ============================================================================
// Infrastructure
// ============================================================================

/// Cron job scheduling
pub(crate) mod cron;

/// Hook registry and management (Milestone 8)
pub mod hooks;

/// Daemon mode for background execution
pub(crate) mod daemon;

/// HTTP API server
pub mod api;

/// Web UI (embedded HTML)
pub mod web_ui;

/// Security policies and sandboxing
pub mod security;

/// Observability (metrics, tracing, audit, performance)
pub mod observability;

// ============================================================================
// Tools & Skills
// ============================================================================

/// Tool implementations (filesystem, http, browser, etc.)
pub mod tools;

/// Skill system
pub(crate) mod skills;

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

/// Agent image management (images, instances, registry)
pub mod image;

/// Remote registry client (push/pull)
pub mod registry;

/// Compaction and transcript management
pub(crate) mod compaction;

// ============================================================================
// Development / Experimental
// ============================================================================

/// Development and experimental features
pub mod dev;

// ============================================================================
// Public API
// ============================================================================

pub use agent::Agent;
pub use config::Config;

// Re-export event types for tool monitoring and streaming
pub use engine::{AgenticEvent, LifecyclePhase};

/// Pekobot version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
