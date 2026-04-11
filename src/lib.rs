//! Pekobot - Lightweight Multi-Agent Runtime
//!
//! A Rust-based agent runtime with unified extension support for multi-platform messaging.

//! ## Architecture
//!
//! Pekobot uses a minimal core (~500KB-1MB) with on-demand loaded extensions:
//!
//! - **Core**: Agent runtime, state machine, tool registry
//! - **Extensions**: Unified extension system (skills, tools, MCP, gateways)
//! - **Gateways**: Messaging platform adapters (Discord, Slack, etc.) as extensions
//!
//! ## Quick Start
//!
//! ```bash,ignore
//! # Install a gateway extension
//! pekobot ext install ./discord-gateway
//!
//! # Run single agent
//! pekobot agent
//!
//! # See all options
//! pekobot --help
//! ```
//!
//! ## Extension System
//!
//! Extensions use the Unified Extension Architecture (ADR-017):
//!
//! ```rust,ignore
//! use pekobot::extensions::{
//!     ExtensionManager, ExtensionManifest,
//!     adapters::gateway_adapter::GatewayAdapter
//! };
//!
//! async fn example() {
//!     let manager = ExtensionManager::new();
//!     manager.register_adapter(Box::new(GatewayAdapter::new(core)));
//!     
//!     // Install and enable gateway extension
//!     manager.install("./discord-gateway").await.unwrap();
//!     manager.enable("discord").await.unwrap();
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
