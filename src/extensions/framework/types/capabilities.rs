//! Compatibility re-export for the `peko_extension_api::capabilities` module.
//!
//! `Capability`, `Capabilities`, and `ActiveExtensionSet` moved into the
//! `peko-extension-api` workspace crate in Phase 7. This shim keeps
//! `peko::extensions::framework::types::{Capability, Capabilities,
//! ActiveExtensionSet}` paths working unchanged.

pub use peko_extension_api::capabilities::*;
