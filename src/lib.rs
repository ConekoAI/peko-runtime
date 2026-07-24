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
//!
//! ## Cargo Workspace
//!
//! The `peko` crate is the **compatibility facade** and binary entry
//! point. As of Phase 12, the runtime's pure-dependency contracts and
//! implementation crates have been lifted into a 13-member Cargo
//! workspace under `crates/` (see the workspace member list and per-crate
//! dependency rules in [`AGENTS.md`](../AGENTS.md#architecture-overview)).
//! The root `peko` package preserves every public path used by
//! integration tests, the CLI binary (`src/main.rs`), and external
//! consumers — it does **not** implement new behavior, only re-exports
//! intentional surfaces from the workspace members.
//!
//! Two CI gates police the boundary:
//!
//! - `scripts/check_module_boundaries.sh` — path-grep for in-`src/`
//!   boundary rules (framework-vs-implementation, principal-vs-tunnel,
//!   etc.). Catches regressions in the legacy module structure.
//! - `scripts/check_workspace_deps.py` — Phase 12b Cargo.toml parser
//!   that asserts the 71-entry forbidden-edge table from the
//!   workspace-migration plan. Catches forbidden crate-to-crate edges
//!   (e.g. providers → engine ban, peko-protocol wire-only contract,
//!   leaf-crate purity rules) before a PR can merge.
//!
//! Adding a new crate to the workspace, lifting more code into an
//! existing crate, or wiring a new dep edge between workspace members
//! MUST keep both checks green.

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
// Module inventory (Phase 1 — root facade boundary)
// ============================================================================
//
// Every `pub mod` below carries a `[kept]` or `[extract:phase-N]` tag so
// reviewers can validate Phase-1 scope. The rule (codified in
// AGENTS.md §Cleanup invariant):
//
//   [kept]      Thin binary-composition module. Stays `pub` after cleanup.
//               Used by the CLI binary or holds root-only wiring glue.
//   [extract]   Domain module. Migrates to a workspace crate in Phase N
//               and the root entry is removed or narrowed to `pub(crate)`.
//
// `pub use peko_*::*` re-export shims are forbidden in this tree — they
// already exist in 4 files (`src/subject.rs`, `src/quota/mod.rs`,
// `src/tools/core/mod.rs`, `src/common/types/message.rs`) and are
// scheduled for deletion in Phase 15.
//
// Common utilities shared across CLI and API
// [kept] — Phase 14 trims to binary-composition wiring only (sub-pub(crate)).
pub mod common;

// ============================================================================
// Core Runtime
// ============================================================================

// [extract:phase-9] peko-agents
pub mod agents;

// [extract:phase-16] thin re-export of peko-engine after *_compat.rs removed
pub mod engine;

// [extract:phase-7] peko-session
pub mod session;

// ============================================================================
// External Interfaces
// ============================================================================

// [extract:phase-6] peko-providers
// src/providers/ was deleted in Phase 6; all provider types now live
// in the `peko-providers` workspace crate (`peko_providers::*`).
// Root composition wires Vault-backed adapters into the trait ports
// declared in `peko_provider_api::credentials` (see
// `crate::common::vault_credential_provider` and
// `crate::common::vault_secret_store`).

// [extract:phase-8] bulk-moved into peko-extension-host
pub mod extensions;

// ============================================================================
// Data & State
// ============================================================================

// [extract:phase-3] peko-identity
// src/identity/ was deleted in Phase 3; all identity types now live in
// the peko-identity workspace crate (`peko_identity::*`).
// `identity_compat` is the host-side adapter that wires root's
// `PathResolver` + `Vault` into the peko_identity trait ports
// (RuntimePaths / IdentityVault / IdentityDataDir).
pub mod identity_compat;

// [extract:phase-4] peko-auth
// src/auth/ was deleted in Phase 4; all auth types now live in the
// peko-auth workspace crate (`peko_auth::*`). `auth_compat` is the
// host-side adapter that wires root's `PathResolver` +
// `PrincipalConfig` into the peko_auth trait ports (RuntimePaths /
// PrincipalResourceView).
pub mod auth_compat;

// [extract:phase-14] peko-principal
pub mod principal;

// (Phase 15: peko-quota shim deleted; callers use peko_quota::* directly)

// ============================================================================
// Append-only runtime surfaces
// ============================================================================

// [extract:phase-5] peko-chat-log — moved to crates/chat-log/

// ============================================================================
// Infrastructure
// ============================================================================

// [extract:phase-14] peko-cron — DONE (PR #301, 2026-07-24):
// 4 root files moved to `crates/cron/src/{lib,events,idle,event_trigger}.rs`.
// `daemon_adapter.rs` relocated to `src/daemon/cron_runtime.rs` because it
// depends on root-only `crate::ipc::{DaemonClient, ResponsePacket}`.
// Callers in commands/cron, ipc/handlers/cron, daemon/state, and
// daemon/mod import from `peko_cron::*` and `crate::daemon::cron_runtime::*`
// directly.

// [extract:phase-13] peko-daemon (impl crate)
//
// `pub` since Phase 11b/12 because:
// 1. The `peko-daemon` workspace member crate (`crates/peko-daemon/`)
//    needs `Daemon::new`/`DaemonConfig`/`Daemon::run` to construct
//    and run a daemon.
// 2. `LaunchMode` is part of the public IPC wire envelope
//    (`ipc::packet::Status::mode`); `ipc` is `pub`.
//
// The daemon's internals (`background_runtime`, `cron_engine`,
// `state`, `DaemonStatus`) stay `pub(crate)`. Only the entry
// surface is widened.
pub mod daemon;

// [extract:phase-12b] peko-ipc
pub mod ipc;

// [extract:phase-14] peko-observability — DONE (PR #300, 2026-07-24):
// 4 root files moved to `crates/observability/src/{lib,audit,metrics,tracer}.rs`.
// Callers in daemon/cron_engine, principal/{context,manager,router}, tunnel/host,
// agents/subagent_executor import from `peko_observability::*` directly.

// ============================================================================
// Tools
// ============================================================================

// [extract:phase-10+phase-18] peko-tools-builtin; bash/tool_search/agent_catalog
// are deferred to Phase 18, src/tools/ tree deleted in Phase 18.
pub mod tools;

// ============================================================================
// CLI & Commands
// ============================================================================

// [kept] — CLI handlers. Stays pub after cleanup.
pub mod commands;

// ============================================================================
// Utilities
// ============================================================================

// [extract:phase-11] peko-registry
pub mod registry;

// [extract:phase-12a] peko-tunnel
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
