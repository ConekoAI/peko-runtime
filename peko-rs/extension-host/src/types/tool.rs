//! Compatibility re-export for the `peko_extension_api::tool` module.
//!
//! `ToolSource` and `ToolMetadata` moved into the `peko-extension-api`
//! workspace crate in Phase 7. This shim keeps
//! `peko::extensions::framework::types::{ToolSource, ToolMetadata}`
//! paths working unchanged.

pub use peko_extension_api::tool::*;
