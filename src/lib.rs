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
//! use pekobot::extension::{
//!     ExtensionManager, ExtensionManifest,
//! };
//! use pekobot::extensions::gateway::adapter::GatewayAdapter;
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

/// Shared runtime components (tool runtime, etc.)
pub mod runtime;

/// Execution engine and state machine
pub mod engine;

/// Message queue with lane-aware processing
/// Session storage (JSONL)
pub mod session;

/// Orchestration layer (event router, file watcher, webhooks)
/// Team runtime (multi-agent teams, event bus, shared services)
pub mod team;

// ============================================================================
// External Interfaces
// ============================================================================

/// LLM provider integrations
pub mod providers;

/// Extension Framework (generic, no external deps)
///
/// Contains the unified extension architecture: hook points, registries,
/// types, managers, and shared services. Zero dependencies on extension
/// type implementations.
pub mod extension;

/// Extension Type Implementations (MCP, Gateway, Skill, Builtin, General, Universal)
///
/// Each extension type lives in its own directory with its adapter,
/// runtime, and protocol code. Depends on `crate::extension` for the
/// generic framework.
pub mod extensions;

// ============================================================================
// Data & State
// ============================================================================

/// Type definitions
pub mod types;

/// Configuration management
/// Agent identity and key management
pub mod identity;

// ============================================================================
// Infrastructure
// ============================================================================

/// Cron job scheduling
pub(crate) mod cron;

// / Hook registry and management (Milestone 8 — deprecated, see Issue 001)
// pub mod hooks; // Removed per Issue 001 — use extensions::core instead
/// Daemon mode for background execution
pub(crate) mod daemon;

/// IPC layer (UDP/Unix socket) for CLI↔daemon communication
pub mod ipc;

/// Observability (metrics, tracing, audit, performance)
pub mod observability;

// ============================================================================
// Tools & Skills
// ============================================================================

/// Tool implementations (filesystem, http, browser, etc.)
pub mod tools;

/// Skill system
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

/// Portable agent packaging (export/import/build/push/pull)
pub mod portable;

/// Remote registry client (push/pull)
pub mod registry;

/// Compaction and transcript management
pub(crate) mod compaction;

// ============================================================================
// Development / Experimental
// ============================================================================

/// Development and experimental features
// ============================================================================
// Public API
// ============================================================================
pub use agent::Agent;

// Re-export event types for tool monitoring and streaming
pub use engine::{AgenticEvent, LifecyclePhase};

/// Pekobot version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
