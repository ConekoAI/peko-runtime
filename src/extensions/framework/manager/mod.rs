//! Extension manager backend modules
//!
//! The runtime-wide extension lifecycle is now owned by
//! [`crate::extensions::framework::store::ExtensionStore`]. This module keeps
//! the backend helpers that the store uses internally:
//!
//! - `discovery` (re-exported from `peko_extension_host`): directory scanning
//!   and extension detection — lifted in Phase 8b; root shim deleted in
//!   Phase 8b.2 once `store.rs` (Phase 8c blocker) consumes the host path.
//! - `packaging`: `.ext` package export/import (Phase 8c blocker).
//! - `storage`: on-disk persistence for installed extensions (Phase 8c blocker).

pub mod discovery {
    //! Re-export of host-side discovery helpers (Phase 8b.2).
    pub use peko_extension_host::manager::discovery::{discovery_paths, DiscoveredExtension};
}
pub mod packaging;
pub mod storage;
