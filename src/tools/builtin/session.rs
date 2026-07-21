//! Session introspection tools
//!
//! Phase 10d re-export shim. The canonical `SessionTool`, `SessionCache`,
//! and the six session DTOs (`SessionInfo`, `HistoryMessage`,
//! `ToolCallInfo`, `ToolResultInfo`, `UsageStats`, `SessionStatusResult`)
//! now live in [`peko_tools_builtin::session`]. The legacy
//! `crate::tools::builtin::session::{SessionTool, SessionIntrospector,
//! SessionCache, SessionInfo, …}` paths are preserved as re-exports so
//! existing callers and tests continue to compile.
//!
//! The legacy `SessionIntrospector` (root-side adapter for the
//! `SessionRegistry` trait that used to live here) has been replaced by
//! [`crate::session::session_runtime_impl::SessionManagerRuntime`], which
//! implements the new [`peko_tools_builtin::session::SessionRuntime`]
//! port trait. The type alias below keeps the legacy name valid; new
//! callers should construct `SessionManagerRuntime` directly.

/// Alias for the legacy `SessionIntrospector` name. Prefer constructing
/// `crate::session::session_runtime_impl::SessionManagerRuntime` directly.
pub use crate::session::session_runtime_impl::SessionManagerRuntime as SessionIntrospector;

pub use crate::session::session_runtime_impl::SessionManagerRuntime;

// Trait alias: the legacy `SessionRegistry` trait was the same shape as
// the new `peko_tools_builtin::session::SessionRuntime` port trait.
pub use peko_tools_builtin::session::SessionRuntime as SessionRegistry;

pub use peko_tools_builtin::session::{
    HistoryMessage, SessionCache, SessionInfo, SessionStatusResult, SessionTool,
    SharedSessionRuntime, ToolCallInfo, ToolResultInfo, UsageStats,
};
