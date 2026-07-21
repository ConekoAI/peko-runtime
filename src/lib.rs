//! Peko - Principal-as-actor runtime
//!
//! A Rust-based runtime where each user-facing entity is a Principal
//! (identity, memory, governance, capabilities, root agent, and
//! a workspace of agent prompts). Agents are now thin Markdown
//! extensions (`AGENT.md`) managed by the extension framework.
//!
//! ## Architecture
//!
//! Peko uses a minimal core (~500KB-1MB) with on-demand loaded extensions:
//!
//! - **Core**: Principal runtime, root-agent routing, tool registry
//! - **Extensions**: Unified extension system (skills, tools, MCP, gateways,
//!   and the thin agent prompts that Principals delegate to)
//! - **Gateways**: Messaging platform adapters (Discord, Slack, etc.) as extensions
//!
//! ## Quick Start
//!
//! ```bash,ignore
//! # Install a gateway extension
//! peko ext install ./discord-gateway
//!
//! # Create a principal and send a message
//! peko principal create alice
//! peko send alice "hello"
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
//!     ExtensionStore, ExtensionManifest,
//! };
//! use peko::extensions::gateway::adapter::GatewayAdapter;
//!
//! async fn example() {
//!     let store = ExtensionStore::new();
//!     store.register_adapter(Box::new(GatewayAdapter::new(core))).await;
//!
//!     // Install a gateway extension
//!     store.install("./discord-gateway").await.unwrap();
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

/// Principal runtime, root-agent lifecycle, and the workspace-of-agent-prompts
/// model that replaced standalone multi-agent management.
pub mod agents;

/// Execution engine and state machine
pub mod engine;

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

/// Agent identity and key management
pub mod identity;

/// Authentication and authorization (ADR-034)
pub mod auth;

/// Canonical actor type (ADR-041).
pub mod subject;

/// Principal container entity (ADR-041).
pub mod principal;

/// Per-principal token quota (F18). Cross-cutting concern that
/// touches principal config, the engine loop, the compactor, and
/// the IPC layer.
pub mod quota;

// ============================================================================
// Append-only runtime surfaces
// ============================================================================

/// Runtime-owned, append-only chat-log storage (one shard per
/// `(principal_did, peer)` pair). Distinct from session JSONL — chat
/// logs record consumer-visible messages only and are external to
/// the principal's mutable working memory. See ADR-042.
pub mod chat_log;

// ============================================================================
// Infrastructure
// ============================================================================

/// Cron job scheduling
pub(crate) mod cron;

/// Daemon mode for background execution (long-running process).
///
/// Crate-internal since the F9 cleanup: the only consumers (`Daemon::run` in
/// `src/daemon/mod.rs`, the integration test that exercises it, and the
/// `#[cfg(test)]` mod in `src/tunnel/dispatcher.rs`) all live inside the
/// crate. The release binary never reaches into it.
pub(crate) mod daemon;

/// IPC layer (UDP/Unix socket) for CLI↔daemon communication
pub mod ipc;

/// Observability (metrics, tracing, audit, performance)
pub(crate) mod observability;

// ============================================================================
// Tools
// ============================================================================

/// Tool implementations (filesystem, http, browser, etc.)
pub mod tools;

// ============================================================================
// CLI & Commands
// ============================================================================
/// CLI command handlers
pub mod commands;

// ============================================================================
// Utilities
// ============================================================================

/// Remote registry client (push/pull) and local `.principal` packaging.
/// (the standalone agent packaging surface was retired in favor of Principal packages).
pub mod registry;

/// Runtime-Pekohub tunnel protocol (ADR-035)
pub mod tunnel;

// ============================================================================
// Public API
// ============================================================================
//
// `peko` is the root compatibility package (lib + bin) inside the Cargo
// workspace. The lib's public surface is driven by external integration tests
// under `tests/` and `tests/scenarios/` plus the binary at `src/main.rs` (which
// imports via `peko::...` because the bin is a separate crate root — a
// `crate::*` swap is not viable for the same reason). The remaining three dead
// re-exports that used to live here (`Agent`, `AgenticEvent`, `LifecyclePhase`)
// had zero consumers anywhere in the crate, in `tests/`, or in `src/main.rs`,
// and have been removed.
//
// `VERSION` is consumed internally by `commands::update`, `ipc::handlers::system`,
// and the registry packaging manifests. It is crate-internal — there is no
// reason for it to be part of the published surface.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
