//! Extension manager backend modules (Phase 8b).
//!
//! The runtime-wide extension lifecycle is owned by
//! [`crate::store::ExtensionStore`] (still in root). This module hosts
//! the backend helpers that the store uses internally:
//!
//! - `discovery`: directory scanning and extension detection
//! - `storage`: on-disk persistence for installed extensions
//!
//! `packaging` (`.ext` package export/import) stays in root because it
//! depends on [`crate::extensions::framework::store::ExtensionStore`]
//! which is in turn coupled to `crate::extensions::framework::adapters::*`.
//! Phase 8c moves `adapters/` and the rest of `services/` and `protocols/`,
//! at which point `packaging` can lift into the host too.

pub mod discovery;
pub mod storage;
