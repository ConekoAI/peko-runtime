//! Re-export shim for `peko_extension_host::scaffold`.
//!
//! Phase 8a moved `framework/scaffold/` (templates + engine) into
//! `peko_extension_host`. The `templates/` directory was moved as a
//! tree and the host crate's `include_str!` macros resolve to the
//! new location.
pub use peko_extension_host::scaffold::*;
