//! Compatibility re-exports for the `peko-tools-core` workspace crate.
//!
//! The tool execution API — `Tool`, `ToolContext`, `AbortSignal`,
//! `ToolResult`, `ToolError`, `ToolProgressEvent`,
//! `ToolWithContext`, `ToolInterruptNotice`, `ContextSource`,
//! `ContextResolver`, `ToolContextAdapter`, bridge helpers, and
//! `ToolExposure` (F34) — lives in the `peko-tools-core` crate as one
//! coherent domain. Internal consumers continue using the historical
//! `peko::tools::core::*` import paths through these re-exports.
//!
//! Both the submodule form (`peko::tools::core::traits::Tool`) and
//! the flat re-export form (`peko::tools::core::Tool`) keep
//! resolving after extraction.

// Re-export each submodule so existing
// `peko::tools::core::traits::Tool`,
// `peko::tools::core::exec::ToolContext`, etc. paths keep working.
pub use peko_tools_core::context_source;
pub use peko_tools_core::exec;
pub use peko_tools_core::interrupt;
pub use peko_tools_core::traits;

// Re-export the top-level types so `peko::tools::core::Tool`,
// `peko::tools::core::ToolContext`, …, and the
// `peko::tools::core::ToolExposure` fan-out all keep working
// through the historical facade.
pub use peko_tools_core::{
    bridge_from_cancellation_token, bridge_to_cancellation_token, AbortSignal,
    AbortSignalBridgeGuard, CancellationTokenBridgeGuard, ContextResolver, ContextSource, Tool,
    ToolContext, ToolContextAdapter, ToolError, ToolExposure, ToolInterruptNotice,
    ToolProgressEvent, ToolResult, ToolWithContext,
};
