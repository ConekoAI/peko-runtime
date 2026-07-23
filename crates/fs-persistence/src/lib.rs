//! Append-only file persistence utilities used by `peko-chat-log`
//! and `peko-session`. Hosts `FileLock` / `LockManager` /
//! `append_bytes_durable` + the default timeout constants.
//!
//! This crate replaces `src/common/persistence/` and
//! `src/session/lock.rs` (the latter was a `pub use` shim). Both
//! `peko-chat-log` (Phase 5) and `peko-session` (Phase 7) depend on
//! it; nothing else in the workspace. Keeping it leaf-sized avoids
//! pulling `peko-extension-host` in just for its `SimpleRegistry`
//! wrapper — `LockManager` rolls a plain `HashMap` instead.
//!
//! Phase 5 of the post-migration cleanup.

mod durable;
mod file_lock;

pub use durable::append_bytes_durable;
pub use file_lock::{FileLock, LockManager, DEFAULT_LOCK_TIMEOUT_MS, DEFAULT_STALE_LOCK_MS};
