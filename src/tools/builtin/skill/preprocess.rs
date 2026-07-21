//! Dynamic context preprocessor for SKILL.md bodies.
//!
//! Phase 10d re-export shim. The canonical implementation lives in
//! [`peko_tools_builtin::skill::body`]. Re-exported here to keep the
//! legacy `crate::tools::builtin::skill::preprocess::preprocess_dynamic_context`
//! path working for any external callers.

pub use peko_tools_builtin::skill::body::{preprocess_dynamic_context, SHELL_TIMEOUT_MS};
