//! Peko - Lightweight Multi-Agent Runtime
//!
//! A Rust-based agent runtime with unified extension support for multi-platform messaging.

//! ## Architecture
//!
//! Peko uses a minimal core (~500KB-1MB) with on-demand loaded extensions:
//!
//! - **Core**: Agent runtime, state machine, tool registry
//! - **Extensions**: Unified extension system (skills, tools, MCP, gateways)
//! - **Gateways**: Messaging platform adapters (Discord, Slack, etc.) as extensions
//!
//! ## Quick Start
//!
//! ```bash,ignore
//! # Install a gateway extension
//! peko ext install ./discord-gateway
//!
//! # Run single agent
//! peko agent
//!
//! # See all options
//! peko --help
//! ```
//!
//! ## Extension System
//!
//! Extensions use the Unified Extension Architecture (ADR-017):
//!
//! ```rust,ignore
//! use peko::extensions::framework::{
//!     ExtensionManager, ExtensionManifest,
//! };
//! use peko::extensions::gateway::adapter::GatewayAdapter;
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
// Silence overwhelmingly noisy/insignificant lints globally
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::return_self_not_must_use)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::unused_async)]
#![allow(clippy::unnecessary_debug_formatting)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::complexity)]
#![allow(clippy::unused_self)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::string_add_assign)]
#![allow(clippy::format_push_string)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::single_match_else)]
#![allow(clippy::single_match)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::missing_fields_in_debug)]
#![allow(clippy::new_without_default)]
#![allow(clippy::option_map_or_none)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_map)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::unwrap_or_default)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::needless_continue)]
#![allow(clippy::module_inception)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::assigning_clones)]
#![allow(clippy::doc_link_with_quotes)]
#![allow(clippy::if_not_else)]
#![allow(clippy::default_trait_access)]
#![allow(clippy::borrow_deref_ref)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::collapsible_str_replace)]
#![allow(clippy::doc_overindented_list_items)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::implicit_hasher)]
#![allow(clippy::io_other_error)]
#![allow(clippy::manual_unwrap_or_default)]
#![allow(clippy::map_clone)]
#![allow(clippy::no_effect_underscore_binding)]
#![allow(clippy::unnecessary_literal_bound)]
#![allow(clippy::wrong_self_convention)]
#![allow(clippy::iter_over_hash_type)]
#![allow(clippy::map_entry)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::match_like_matches_macro)]
#![allow(clippy::semicolon_if_nothing_returned)]
#![allow(clippy::needless_raw_string_hashes)]
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::map_flatten)]
#![allow(clippy::let_underscore_untyped)]
#![allow(clippy::option_map_unit_fn)]
#![allow(clippy::result_map_unit_fn)]
#![allow(clippy::filter_map_next)]
#![allow(clippy::manual_filter_map)]
#![allow(clippy::manual_find_map)]
#![allow(clippy::unnecessary_semicolon)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::match_wildcard_for_single_variants)]

// ============================================================================
// Common Utilities
// ============================================================================

/// Common utilities shared across CLI and API
pub mod common;

// ============================================================================
// Core Runtime
// ============================================================================

/// Agent runtime, lifecycle, and multi-agent management (absorbed `prompt/`)
pub mod agents;

/// Execution engine and state machine
pub mod engine;

/// Message queue with lane-aware processing
/// Session storage (JSONL)
pub mod session;

// ============================================================================
// External Interfaces
// ============================================================================

/// LLM provider integrations
pub mod providers;

/// Extension Framework and Type Implementations (MCP, Gateway, Skill,
/// Builtin, General, Universal).
///
/// Contains the generic extension framework (under `crate::extensions::framework`)
/// and the extension type implementations (sibling submodules). The framework
/// is dependency-free; type adapters depend on it.
pub mod extensions;

// ============================================================================
// Data & State
// ============================================================================

/// Configuration management
/// Agent identity and key management
pub mod identity;

/// Authentication and authorization (ADR-034)
pub mod auth;

/// Canonical actor type (ADR-041).
pub mod subject;

/// Principal container entity (ADR-041).
pub mod principal;

// ============================================================================
// Infrastructure
// ============================================================================

/// Cron job scheduling
pub(crate) mod cron;

/// Daemon mode for background execution (internal, exposed for integration tests)
#[cfg(not(feature = "test-utils"))]
pub(crate) mod daemon;

/// Daemon mode for background execution (exposed for integration tests with test-utils feature)
#[cfg(feature = "test-utils")]
pub mod daemon;

/// Re-exports for integration tests (only available with `test-utils` feature)
#[cfg(feature = "test-utils")]
pub mod test_utils {
    pub use crate::daemon::state::{AppState, DaemonConfigSnapshot};
}

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

// ============================================================================
// Utilities
// ============================================================================

/// Remote registry client (push/pull) and local packaging
/// (export/import/build/push/pull of `.agent` / `.team` archives).
pub mod registry;

/// Runtime-Pekohub tunnel protocol (ADR-035)
pub mod tunnel;

// ============================================================================
// Development / Experimental
// ============================================================================

/// Development and experimental features
// ============================================================================
// Public API
// ============================================================================
pub use agents::Agent;

// Re-export event types for tool monitoring and streaming
pub use engine::{AgenticEvent, LifecyclePhase};

/// Peko version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
