//! Extension manager backend modules
//!
//! The runtime-wide extension lifecycle is now owned by
//! [`crate::extensions::framework::store::ExtensionStore`]. This module keeps
//! the backend helpers that the store uses internally:
//!
//! - `discovery`: directory scanning and extension detection
//! - `packaging`: `.ext` package export/import
//! - `storage`: on-disk persistence for installed extensions

pub mod discovery;
pub mod packaging;
pub mod storage;
