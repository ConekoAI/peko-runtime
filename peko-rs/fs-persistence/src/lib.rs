//! Append-only file persistence utilities used by `peko-session`,
//! `peko-channel`, and `peko-plan`. Hosts `FileLock` / `LockManager` /
//! `append_bytes_durable` + the default timeout constants.
//!
//! This crate replaces `src/common/persistence/` and
//! `src/session/lock.rs` (the latter was a `pub use` shim). Nothing
//! else in the workspace depends on it. Keeping it leaf-sized avoids
//! pulling `peko-extension-host` in just for its `SimpleRegistry`
//! wrapper — `LockManager` rolls a plain `HashMap` instead.
//!
//! Phase 5 of the post-migration cleanup.

mod durable;
mod file_lock;

pub use durable::append_bytes_durable;
pub use file_lock::{FileLock, LockManager, DEFAULT_LOCK_TIMEOUT_MS, DEFAULT_STALE_LOCK_MS};
