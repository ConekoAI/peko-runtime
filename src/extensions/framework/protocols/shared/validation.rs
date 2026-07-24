//! Re-export shim (Phase 8c.1.C).
//!
//! Implementation lives in `peko_extension_host::protocols::shared::validation`.
//! This root-side file is kept so the historical
//! `crate::extensions::framework::protocols::shared::validation::*`
//! import paths keep compiling until the framework shim tree is fully
//! deleted in Phase 8c.2.

pub use peko_extension_host::protocols::shared::validation::*;
