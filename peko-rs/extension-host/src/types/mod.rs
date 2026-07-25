//! Extension framework types
//!
//! Phase 7 extracted the framework-facing contracts into the
//! `peko-extension-api` workspace crate. This module is a thin
//! compatibility facade: each sub-shim re-exports the API crate's
//! types so existing `peko::extensions::framework::types::*` paths
//! keep resolving unchanged. The `hook_io` shim additionally exposes
//! the `CompactionPreparationPayload` / `CompactionResultPayload`
//! helpers that bridge the typed engine-side data into the
//! `serde_json::Value` fields the API crate's `HookInput::*` variants
//! carry.

pub use peko_extension_api::types::{
    ExtensionId, HookId, HookPriority, ToolRuntimeContext, DEFAULT_HOOK_PRIORITY,
    FALLBACK_HOOK_PRIORITY, SYSTEM_HOOK_PRIORITY, USER_HOOK_PRIORITY,
};

pub use peko_extension_api::{
    tool_result_from_hook, ActiveExtensionSet, AsyncReceipt, AsyncTaskId, AsyncTaskResult,
    AsyncTaskStatus, Capabilities, Capability, ExtensionDependency, ExtensionManifest, HookInput,
    HookOutput, HookResult, MessageEnvelope, ParamSource, PromptBuildState, ReservedParamsConfig,
    ReservedParamsService, SessionSnapshot, ToolMetadata, ToolRegistryAccess, ToolSource,
};

// `ToolExposure` migrated to `peko-tools-core` in Phase 5. Re-export
// from here so existing `crate::types::ToolExposure`
// paths keep resolving unchanged.
pub use peko_tools_core::ToolExposure;

// Per-file shims preserved for backwards compatibility — the actual
// type definitions now live in the `peko-extension-api` workspace
// crate. Each sub-shim re-exports the corresponding module; `hook_io`
// additionally provides payload helpers for the engine.
pub mod async_types;
pub mod capabilities;
pub mod hook_io;
pub mod manifest;
pub mod session;
pub mod tool;
