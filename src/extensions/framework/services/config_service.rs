//! Phase 8c.1.D.3 shim. The real implementation lives in
//! `peko_extension_host::services::config_service` (lifted from this file).
//!
//! Kept as a `pub use` module so historical import paths like
//! `crate::extensions::framework::services::config_service::*` keep compiling.
//! Phase 8c.2 will delete this file after the path sweep.

pub use peko_extension_host::services::config_service::*;
