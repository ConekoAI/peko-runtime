//! Phase 8c.1.D.4 shim. The real implementation lives in
//! `peko_extension_host::manager::packaging` (lifted from this file).
//!
//! Production code lifted verbatim; tests rewritten to use a stub store
//! that implements the [`peko_extension_host::store::ExtensionStore`]
//! trait port (host has no root-only `ExtensionStore::new()` constructor).
//!
//! Kept as a `pub use` module so historical import paths like
//! `crate::extensions::framework::manager::packaging::*` keep compiling.
//! Phase 8c.2 will delete this file after the path sweep.

pub use peko_extension_host::manager::packaging::*;
