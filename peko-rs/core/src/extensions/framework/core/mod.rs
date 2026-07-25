//! Re-export shim for `peko_extension_host::core` plus the local
//! `async_bridge` module that stays in root until Phase 8b.
//!
//! Phase 8a moved 12 of the 13 files in `framework/core/` into
//! `peko_extension_host::core`. The remaining file, `async_bridge.rs`,
//! imports from `framework/async_exec` (Phase 8b) and is deferred.
//! This `mod.rs` re-exports every host-side name so historical
//! `crate::extensions::framework::core::*` paths keep resolving, and
//! declares the local `async_bridge` submodule.

// `async_bridge.rs` stays in root until Phase 8b lifts async_exec.
pub mod async_bridge;
pub use async_bridge::ExtensionAsyncAdapter;

// Re-export every other core item from the host crate. The host
// exposes each submodule as a `pub mod`, so the historical
// `crate::extensions::framework::core::registry::ExtensionCore` path
// continues to resolve through `peko_extension_host::core::registry::ExtensionCore`.
pub use peko_extension_host::core::{
    binding, common, config, context, global_core, handler, hook_points, hook_registry,
    init_global_core, registry, scoring, tool_registration, tool_registry, BuiltinExtensionInfo,
    ExtensionConfig, ExtensionCore, ExtensionServices, HookBinding, HookBindingBuilder,
    HookContext, HookHandler, HookHandlerFactory, HookPoint, HookPointBuilder, HookRegistry,
    HookState, RegisteredHook, TelemetryService, ToolMetadata, ToolSource,
};
// `ToolRegistryAccess` lives in `peko_extension_host::types` (it's a
// trait on `ExtensionCore`'s tool registry access surface). Re-export
// it here so historical `framework::core::ToolRegistryAccess` paths
// keep resolving.
pub use peko_extension_host::types::ToolRegistryAccess;

// `test_sync` is gated on `feature = "test-utils"` in the host crate.
// Re-export it conditionally so root's `framework::core::test_sync`
// path also resolves when the test-utils feature is enabled.
#[cfg(feature = "test-utils")]
pub use peko_extension_host::core::test_sync;
