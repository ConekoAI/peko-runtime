//! Path utilities for the extension host.
//!
//! Phase 8 commit 1 ships the small
//! [`default_async_tasks_dir`] helper that mirrors
//! `src/common/paths.rs::default_data_dir()` joined with
//! `async_tasks`. Production callers should inject a custom path
//! via `AsyncExecutor::with_task_file_writer(...)`; this helper is
//! the default for tests and standalone contexts.
//!
//! Phase 8 commit 2 will introduce a more general [`PathResolver`]
//! trait that lets the host's `store.rs` and other consumers
//! depend on root-side path resolution without naming
//! `crate::common::paths::*` types.
//!
//! [`PathResolver`]: crate::paths::PathResolver

use std::path::PathBuf;

/// Default directory for async-task file records (mirrors
/// `src/common/paths.rs::default_data_dir()` joined with
/// `async_tasks`).
///
/// **Must stay in sync with
/// `src/common::paths::default_data_dir()`.**
#[must_use]
pub fn default_async_tasks_dir() -> PathBuf {
    std::env::var_os("PEKO_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("peko")
        })
        .join("async_tasks")
}

// Placeholder for Phase 8 commit 2's trait definition. Listed here
// so the lib.rs doc-link resolves today.
#[doc(hidden)]
pub trait PathResolver {}
