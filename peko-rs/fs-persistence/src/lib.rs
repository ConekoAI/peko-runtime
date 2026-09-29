//! Append-only file persistence utilities used by `peko-session`,
//! `peko-channel`, and `peko-plan`. Hosts `FileLock` /
//! `append_bytes_durable` + the default timeout constants, and — since
//! ADR-065 — `WorkspaceFileLock` for cross-agent same-file write
//! serialization in agent workspaces.
//!
//! This crate replaces `src/common/persistence/` and
//! `src/session/lock.rs` (the latter was a `pub use` shim). Nothing
//! else in the workspace depends on it. Keeping it leaf-sized avoids
//! pulling the extension framework in just for its path helpers —
//! `default_workspace_lock_dir` mirrors
//! `peko_extension_api::paths::default_data_dir` locally.
//!
//! Phase 5 of the post-migration cleanup; ADR-065 added the workspace
//! lock and removed the (dead + broken) `LockManager`.

mod durable;
mod file_lock;
mod workspace_lock;

pub use durable::append_bytes_durable;
pub use file_lock::{FileLock, DEFAULT_LOCK_TIMEOUT_MS, DEFAULT_STALE_LOCK_MS};
pub use workspace_lock::{
    default_workspace_lock_dir, lock_file_path, WorkspaceFileLock,
    DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS,
};
