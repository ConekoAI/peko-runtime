//! Extension manager backend modules (Phase 8b + Phase 8c.1.D.4).
//!
//! The runtime-wide extension lifecycle is owned by
//! [`crate::store::ExtensionStore`] (still in root). This module hosts
//! the backend helpers that the store uses internally:
//!
//! - `discovery`: directory scanning and extension detection
//! - `storage`: on-disk persistence for installed extensions
//! - `packaging`: `.ext` package export/import (Phase 8c.1.D.4 lift)

pub mod discovery;
pub mod packaging;
pub mod storage;
