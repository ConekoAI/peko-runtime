//! Extension Framework — Generic Extension Core (ADR-017)
//!
//! Phase 8a + 8b + 8c moved the bulk of this module into `peko_extension_host`:
//! `core`, `types`, `skill_catalog`, `integration`, `scaffold`, `manager/*`,
//! `services/*`, `transport/*`, and `protocols/shared/*` all live in host.
//!
//! The root module tree retains only the root-only pieces that need root
//! types:
//! - `adapters/` — extension type adapter trait + manifests (root-only)
//! - `async_exec/` — async task executor (references root's `ExtensionCore`;
//!   3,378 lines; deferred until `ExtensionCore` itself lifts)
//! - `core/async_bridge.rs` — root-only IPC bridge to daemon
//! - `store.rs` — concrete `ExtensionStore` impl (root because the trait
//!   port lives in host; root owns the actual struct)
//! - `types/` — re-export shim for `peko_extension_host::types` (still
//!   used by ~30 callers in `engine/agentic_loop_compat.rs` etc.)
//!
//! Each shim is `pub use peko_extension_host::*` so the historical
//! `crate::extensions::framework::core::*` (etc.) paths continue to
//! compile until Phase 15 deletes them.
//!
//! Extension type implementations (MCP, Gateway, Skill, etc.) live in
//! `crate::extensions` (plural), not here.
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
///
/// Lifts into `peko_extension_host` in Phase 8c. Until then, stays in root.
pub mod adapters;

/// Async task execution framework.
///
/// Lifts into `peko_extension_host` in Phase 8b. The executor submodule
/// remains as a backwards-compat shim until Phase 8c.2 deletes it.
pub mod async_exec;

/// Hook points, registry, handler traits — the core of the extension system.
///
/// Phase 8a: most of `core/` moved into `peko_extension_host::core`.
/// `core/async_bridge.rs` stays in root until Phase 8b. The root
/// `core/mod.rs` re-exports the host crate's `core` items plus
/// delegates `async_bridge` to the local file.
pub mod core;

/// Extension type definitions (ExtensionManifest, HookResult, etc.).
///
/// Phase 8a: moved into `peko_extension_host::types`. Re-exported here
/// so the historical path keeps compiling.
pub mod types;

/// Global, process-wide extension store.
///
/// Deferred — `store.rs` lifts with `core/store.rs` in Phase 8b after
/// its `framework/adapters` and `framework/manager` deps lift.
pub mod store;

/// Extension lifecycle management (install, enable, disable, discover, bundle).
///
/// Phase 8b lifted the bulk of `manager/` into `peko_extension_host::manager`;
/// Phase 8c adds `packaging` + `storage` (which depends on the ExtensionStore
/// trait port). `discovery` stays here as a backwards-compat shim.
pub mod manager;

// ============================================================================
// Re-exports
// ============================================================================

// Re-export services trait-port surface (lives in host for 8a so
// `framework::services::ToolExecutionConfig` etc. can be backed by
// host-crate types without host depending on root services/).
//
// Note: `AsyncExecutionRouter` resolves here to the **trait** port.
// The concrete router struct lives at
// `peko_extension_host::transport::async_router::AsyncExecutionRouter`
// — callers needing its `with_transport()` constructor import the
// concrete path; trait-port callers use this re-export.
pub use peko_extension_host::transport::AsyncExecutionRouter as AsyncExecutionRouterTrait;
pub use peko_extension_host::{ExecFn, PreprocessorFn, ToolExecConfig};

// Re-export core types at the framework root so callers using
// `crate::extensions::framework::HookPoint` (no submodule) keep
// resolving. Phase 15 deletes these once all callers switch to
// `peko_extension_host::HookPoint` directly.
pub use peko_extension_host::{
    common, global_core, init_global_core, AsyncReceipt, ExtensionCore, ExtensionId,
    ExtensionManifest, ExtensionServices, HookBinding, HookBindingBuilder, HookContext,
    HookHandler, HookHandlerFactory, HookId, HookInput, HookOutput, HookPoint, HookPointBuilder,
    HookPriority, HookResult, HookState, MessageEnvelope, PromptBuildState, RegisteredHook,
    SessionSnapshot, TelemetryService, ToolMetadata, ToolRegistryAccess, ToolSource,
    DEFAULT_HOOK_PRIORITY, FALLBACK_HOOK_PRIORITY, SYSTEM_HOOK_PRIORITY, USER_HOOK_PRIORITY,
};

// ============================================================================
// Prelude
// ============================================================================

/// Prelude for convenient imports
pub mod prelude {
    pub use peko_extension_host::core::{
        common, ExtensionCore, HookContext, HookHandler, HookPoint, HookPointBuilder,
    };
    pub use peko_extension_host::types::{
        ExtensionId, ExtensionManifest, HookId, HookInput, HookOutput, HookResult,
    };
}
