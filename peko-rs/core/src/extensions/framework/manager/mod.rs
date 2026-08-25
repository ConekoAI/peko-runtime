//! Extension manager backend modules (Phase 8b + Phase 8c.1.D.4).
//!
//! The runtime-wide extension lifecycle is owned by
//! [`crate::extensions::framework::store::ExtensionStore`] (still in root). This module hosts
//! the backend helpers that the store uses internally:
//!
//! - `discovery`: directory scanning and extension detection
//! - `storage`: on-disk persistence for installed extensions
//!
//! **Phase 5 (ADR-047 §2.1):** the `packaging` backend (the `.ext`
//! archive format) was deleted. Extensions are workspace-resident
//! and no longer ship as portable archives; workspace scans are the
//! single source of truth.

pub mod discovery;
pub mod storage;
