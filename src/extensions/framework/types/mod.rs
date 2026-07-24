//! Re-export shim for `peko_extension_host::types`.
//!
//! Phase 8a moved all of `framework/types/` into `peko_extension_host`.
//! This file forwards every name so the historical
//! `crate::extensions::framework::types::*` path continues to
//! resolve until Phase 15 deletes it.
pub use peko_extension_host::types::*;
