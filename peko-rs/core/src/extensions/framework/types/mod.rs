//! Extension framework types
//!
//! Phase 7 extracted the framework-facing contracts into the
//! `peko-extension-api` workspace crate. This module is a flat
//! re-export facade — every public type lives in the API crate and
//! is surfaced here as `peko::extensions::framework::types::T` for
//! backwards compatibility.
//!
//! PR-F #1: deleted the 5 per-file shims (`async_types`, `capabilities`,
//! `manifest`, `session`, `tool`) — each was a one-line
//! `pub use peko_extension_api::X::*` whose types were already
//! flat-exported above (lines 13-22). Zero callers of the
//! `types::async_types::*` / `types::capabilities::*` etc. module
//! paths in the entire repo; all consumers reach the types via the
//! flat re-export.
//!
//! `hook_io` remains as a non-shim module because it hosts the
//! `CompactionPreparationPayload` / `CompactionResultPayload` helpers
//! that bridge the typed engine-side data into the `serde_json::Value`
//! fields the API crate's `HookInput::*` variants carry.

pub use peko_extension_api::types::{
    ExtensionId, HookId, HookPriority, ToolRuntimeContext, DEFAULT_HOOK_PRIORITY,
};

pub use peko_extension_api::{
    tool_result_from_hook, ActiveExtensionSet, AsyncReceipt, AsyncTaskId, AsyncTaskResult,
    AsyncTaskStatus, Capabilities, Capability, ExtensionDependency, ExtensionManifest, HookInput,
    HookOutput, HookResult, MessageEnvelope, ParamSource, PromptBuildState, ReservedParamsConfig,
    ReservedParamsService, SessionSnapshot, ToolMetadata, ToolRegistryAccess, ToolSource,
};

// `ToolExposure` migrated to `peko-tools-core` in Phase 5. Re-export
// from here so existing `crate::extensions::framework::types::ToolExposure`
// paths keep resolving unchanged.
pub use peko_tools_core::ToolExposure;

pub mod hook_io;
