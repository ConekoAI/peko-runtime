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
//!
//! Everything else (hook dispatcher, capability gate, transport,
//! framework services, protocol shared subtrees) stays in root. The
//! background-task runtime and the session inbox live at
//! `crate::async_exec` (ADR-066 P1); the extension store / discovery /
//! storage / adapter-trait stack was deleted in the same pass (zero
//! registered adapters).
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
//! Dependency direction: `extension::core` → `extension::types`

// ============================================================================
// Submodules
// ============================================================================

/// Hook points, registry, handler traits, executor integration —
/// the core of the extension system. The `ExtensionCore` impl is
/// the canonical entry point for `peko_engine::funnel` (F37
/// funnel).
pub mod core;

// PR-A: the `integration` module was an 11-line doc-only stub with
// zero callers in the repo. Its sole purpose was to host the
// `ExtensionAsyncTool` wrapper (itself deleted in ADR-063 as dead
// code). Pure removal.

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

// PR-A: the `scaffold` module hosted `peko ext init` (the
// `ScaffoldEngine` / `ScaffoldLang` / `ScaffoldOptions` triple plus
// the embedded extension templates). With `peko ext *` retired in
// Phase 5 (ADR-047 §2.1) there are no callers left in the repo; the
// directory and its 399 lines are pure removal.

/// Framework services — reserved-params resolution.
pub mod services;

// 2026-09-18 (skills-as-files cleanup): the `skill_catalog` module
// (global `SkillCatalog` populated from extension manifests) was
// deleted. Skills are workspace files resolved by
// `extensions::skill::reader::WorkspaceSkillRuntime`; nothing in
// production read the catalog any more.

// ADR-066 P1: `async_exec/` + `inbox.rs` re-homed to
// `crate::async_exec`; `store.rs` / `store_trait.rs` / `discovery.rs` /
// `extension_storage.rs` / `adapters/` / `services/config_service.rs` /
// `services/tool_execution.rs` deleted (zero production consumers).

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
