//! Compatibility re-export for the `peko_extension_api::manifest` module.
//!
//! `ExtensionDependency` and `ExtensionManifest` moved into the
//! `peko-extension-api` workspace crate in Phase 7. This shim keeps
//! `peko::extensions::framework::types::{ExtensionDependency,
//! ExtensionManifest}` paths working unchanged.

pub use peko_extension_api::manifest::*;
