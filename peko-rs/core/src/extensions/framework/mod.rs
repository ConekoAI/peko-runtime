//! Extension Framework — Generic Extension Core (ADR-017)
//!
//! Phase F2 (foldback) rolled back the Phase 8a/8b/8c bulk-extraction
//! of this module into `peko-extension-host`. The sat is deleted; all
//! its files live directly under `src/extensions/framework/` again.
//! The trait ports that needed to leave the sat (because `peko-engine`
//! imports them without depending on root) moved into
//! `peko-extension-api` instead:
//!
//! - `ToolFunnel` (engine-facing `ExtensionCore` surface) — was in sat,
//!   now in `peko_extension_api::ToolFunnel`
//! - `CompletionEvent` / `SteeringMessage` / `InboxItem` data types —
//!   was in sat, now in `peko_extension_api::completion_event`
//! - `default_data_dir` / `default_agent_workspace` path helpers —
//!   was in sat, now in `peko_extension_api::paths`
//! - `ExtensionStoreTrait` (the trait port) — was in sat, now in
//!   `crate::extensions::framework::store_trait`
//!
//! Everything else (concrete `ExtensionStore`, hook dispatcher,
//! capability gate, async executor, transport, manager, scaffold,
//! skill catalog, integration, framework services, protocol
//! shared subtrees) stays in root.
//!
//! Extension type implementations (MCP, Gateway, Skill, etc.) live
//! in `crate::extensions` (plural), not here.
//!
//! # Module Boundaries
//!
//! This module (`src/extensions/framework/`) must NOT import from:
//! - `crate::extensions` (extension type implementations)
//! - `crate::mcp` (absorbed into `crate::extensions::mcp`)
//! - `crate::daemon` (daemon-specific code)
//! - `crate::tools` (tool implementations)
//!
//! Dependency direction: `extension::core` → `extension::types` → `extension::manager|async_exec`

// ============================================================================
// Submodules
// ============================================================================

/// Extension type adapter trait, manifest formats, and built-in adapter provider.
pub mod adapters;

/// Async task execution framework — owns the canonical `AsyncExecutor`,
/// `CompletionQueue`, and the spawned-task bookkeeping that engine
/// flows events into. Type-port helpers (`CompletionEvent`,
/// `SteeringMessage`) live in `peko_extension_api::completion_event`.
pub mod async_exec;

/// Hook points, registry, handler traits, executor integration —
/// the core of the extension system. The `ExtensionCore` impl is
/// the canonical entry point for `peko_engine::funnel` (F37
/// funnel).
pub mod core;

/// ExtensionTypeAdapter ↔ daeomon envelope conversion
/// (port-trait seam for peers without an ExtensionCore).
pub mod integration;

/// Cross-boundary async-task inbox + the `InboxItem` / `SessionInbox`
/// concrete types. The trait-port data types live in
/// `peko_extension_api::completion_event` so engine can reach them.
pub mod inbox;

/// Extension lifecycle management (install, enable, disable,
/// discover, package, bundle). Sub-modules: `discovery`,
/// `packaging`, `storage`.
pub mod manager;

/// Default-agent-workspace path resolver + principal-messaging
/// port traits. The path helpers `default_data_dir` /
/// `default_agent_workspace` were lifted to
/// `peko_extension_api::paths` (engine needs them without depending
/// on root).
pub mod paths;

// Sprint 9 Commit 4: the `principal_message` module was retired
// along with `StatelessAgentService`. Its only consumer was the
// chat-gateway adapter framework (deleted in Commit 3). The
// agent-session paradigm owns principal dispatch via
// `PrincipalManager::receive_streaming` directly.

/// Shared protocol wire formats (request/response packet bodies)
/// shared by the framework's IPC bridge.
pub mod protocols;

/// `crate::extensions::framework::registry` — the simple
/// `SimpleRegistry` / `SharedRegistry` utilities.
pub mod registry;

/// Scaffold generation engine for new extensions (the
/// `ScaffoldEngine` / `ScaffoldLang` / `ScaffoldOptions` triple).
pub mod scaffold;

/// Framework services — config scoping, reserved-params
/// resolution, extension-host wiring layer.
pub mod services;

/// Extension catalog (skills/agents/commands indexed by type).
pub mod skill_catalog;

/// Process-wide `ExtensionStore` trait port + concrete impl.
/// Two files: `store_trait.rs` (the trait port) + `store.rs` (the
/// impl). The trait port lifts into `peko-session` later when the
/// packaging path is ready.
pub mod store;
pub mod store_trait;

/// Engine-facing surface of root's `ExtensionCore`. The trait port
/// lives in `peko_extension_api::ToolFunnel`; the concrete impl lives
/// in `tool_funnel_impl.rs` at this path. The trait-and-impl pair
/// is split to break a sat→root dep cycle.
pub mod tool_funnel_impl;

/// Async-task transport sub-module (router + transport adapters
/// + shim module).
pub mod transport;

/// Stable API contracts for the framework (error enums, enums,
/// DTOs). The bulk of these types live in `peko_extension_api`;
/// this is a re-export shim for backwards compatibility.
pub mod types;

/// Vault access port trait (extension-host facing).
pub mod vault;

// ============================================================================
// Prelude
// ============================================================================

/// Prelude for convenient imports
pub mod prelude {
    pub use crate::extensions::framework::core::{
        common, ExtensionCore, HookContext, HookHandler, HookPoint, HookPointBuilder,
    };
    pub use crate::extensions::framework::types::{
        ExtensionId, ExtensionManifest, HookId, HookInput, HookOutput, HookResult,
    };
}
