//! Extension Framework API contracts (Phase 7)
//!
//! This crate owns the stable framework-facing contracts of the
//! extension system. The concrete framework host (registry, async
//! executor, transport, store, manager, scaffold) lives in the root
//! `peko` crate under `src/extensions/framework/` and re-exports
//! these types as a compatibility facade.
//!
//! # Boundary
//!
//! Allowed dependencies: `peko-message`, `peko-tools-core`,
//! `peko-provider-api`, plus `serde`, `serde_json`, `tokio::sync`,
//! and `uuid` for the value types. The API crate must NOT import:
//!
//! - `peko-engine` (Phase 9 will move the loop)
//! - `peko-engine`-side types (session, principal, agents, daemon,
//!   built-in tools, concrete adapters)
//! - any root-only type from the host
//!
//! # Notable Phase 7 tradeoffs
//!
//! - `HookInput::CompactionPreparation` and `HookInput::CompactionResult`
//!   carry their payloads as `serde_json::Value` blobs because the
//!   pre-Phase-7 versions embedded `crate::session::compaction::*`
//!   types that live in the root crate. The engine-side helper
//!   `compaction_preparation_payload(...)` and friends (in the host
//!   re-export module) provide ergonomic encode/decode.
//! - `AsyncTaskStatus` moved with the `HookOutput::TaskStatus` variant
//!   it tags; the surrounding executor stays in the host.
//! - `ReservedParamsConfig` and `ParamSource` are pure data types
//!   here; the resolution methods that depend on `ToolContext` and
//!   `Vault` (root-only types) live in the host as free functions
//!   `resolve_reserved_params` / `resolve_param_source_with_vault`.

pub mod async_status;
pub mod async_types;
pub mod capabilities;
pub mod hook_io;
pub mod manifest;
pub mod reserved_params;
pub mod session;
pub mod tool;
pub mod types;

pub use async_status::{AsyncTaskId, AsyncTaskResult, AsyncTaskStatus};
pub use async_types::AsyncReceipt;
pub use capabilities::{ActiveExtensionSet, Capabilities, Capability};
pub use hook_io::{tool_result_from_hook, HookInput, HookOutput, HookResult};
pub use manifest::{ExtensionDependency, ExtensionManifest};
pub use reserved_params::{ConfigFormat, ParamSource, ReservedParamsConfig, ReservedParamsService};
pub use session::{MessageEnvelope, PromptBuildState, SessionSnapshot, ToolRegistryAccess};
pub use tool::{ToolMetadata, ToolSource};
pub use types::{
    ExtensionId, HookId, HookPriority, ToolRuntimeContext, DEFAULT_HOOK_PRIORITY,
    FALLBACK_HOOK_PRIORITY, SYSTEM_HOOK_PRIORITY, USER_HOOK_PRIORITY,
};
