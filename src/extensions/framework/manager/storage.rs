//! Phase 8c.1.D.5 shim. The real implementation lives in
//! `peko_extension_host::manager::storage` (lifted from this file).
//!
//! Kept as a `pub use` module so historical import paths like
//! `crate::extensions::framework::manager::storage::*` keep compiling.
//! Phase 8c.2 will delete this file after the path sweep.

pub use peko_extension_host::manager::storage::*;
