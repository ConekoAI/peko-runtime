//! Compatibility re-export shim.
//!
//! The canonical location for `FileLock` / `LockManager` is
//! `crate::common::persistence` (shared by append-only runtime
//! stores). This module is preserved so existing imports of
//! `crate::session::lock::FileLock` continue to compile while callers
//! migrate at their own pace.

pub use crate::common::persistence::file_lock::{FileLock, LockManager};
