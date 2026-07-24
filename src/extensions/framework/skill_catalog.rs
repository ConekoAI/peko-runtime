//! Re-export shim for `peko_extension_host::skill_catalog`.
//!
//! Phase 8a moved the global skill location catalog into the host
//! crate. Root keeps this `mod` declaration so callers using
//! `crate::extensions::framework::skill_catalog::*` continue to
//! compile. Phase 15 deletes this shim.
pub use peko_extension_host::skill_catalog::*;
