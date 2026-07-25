//! Principal-flavored extension row.
//!
//! Subset of the IPC wire [`ExtensionSummary`] containing only the fields
//! the principal slash dispatcher (`/help`) actually reads. Keeps the
//! `peko-principal` crate free of any `peko-protocol`/`src::ipc` dependency
//! while preserving the data the slash command needs.
//!
//! [`ExtensionSummary`]: ../../src/ipc/packet.rs (root-side wire DTO)

use serde::{Deserialize, Serialize};

/// One row of an extension as the principal slash dispatcher sees it.
///
/// Pure data; intentionally minimal. The daemon's IPC handler maps the
/// wire [`ExtensionSummary`] to this struct at the boundary so the lifted
/// `slash/help.rs` can construct rows from extension-store data without
/// reaching into the IPC packet module.
///
/// [`ExtensionSummary`]: ../../src/ipc/packet.rs (root-side wire DTO)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalExtensionRow {
    pub id: String,
    pub name: String,
    pub ext_type: String,
    pub version: String,
    pub source: String,
    pub enabled: bool,
    pub description: String,
    pub provides: Vec<String>,
    pub requires: Vec<String>,
}
