//! Extension manager backend modules
//!
//! The runtime-wide extension lifecycle is owned by
//! [`crate::extensions::framework::store::ExtensionStore`] (still in root).
//! This module hosts:
//!
//! - `discovery` (re-exported from `peko_extension_host`): directory scanning
//!   and extension detection — lifted in Phase 8b; kept here as a shim for
//!   backwards compat (the host itself doesn't depend on root-only
//!   `ExtensionStore::with_storage_dir`).
//!
//! `packaging` and `storage` were deleted from root in Phase 8c.2 — they
//! live in `peko_extension_host::manager::{packaging,storage}`.

pub mod discovery {
    //! Re-export of host-side discovery helpers (Phase 8b.2).
    pub use peko_extension_host::manager::discovery::{discovery_paths, DiscoveredExtension};
}
