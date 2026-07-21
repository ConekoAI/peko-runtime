//! Compatibility re-export for the `peko-provider-api` workspace
//! crate.
//!
//! The full provider contract types — `ChatOptions`, `ChatResponse`,
//! `StreamEvent`, `ContentDelta`, `StopReason`, `BlockType`,
//! `ToolDefinition`, `ContentBlockId`, `ThinkingEffort`,
//! `ThinkingFormat`, `ThinkingKeep`, `ToolChoice`, `ServiceTier`,
//! `ProviderCompat`, `DeferredToolsMode` — live in the
//! `peko-provider-api` crate as one cohesive domain. Internal
//! consumers keep the historical `peko::providers::traits::*` import
//! paths through this shim so existing call sites don't churn.

pub use peko_provider_api::traits::*;
