//! Shared helpers for the Task* planning-todo tool family.
//!
//! Phase 10d re-export shim. The canonical implementation lives in
//! [`peko_tools_builtin::tasks`]; this file preserves the legacy
//! `crate::tools::builtin::task_common::{require_session_id,
//! parse_status_param, missing_session_error}` paths for any
//! out-of-tree callers.

pub use peko_tools_builtin::tasks::{
    missing_session_error, parse_status_param, require_session_id,
};
