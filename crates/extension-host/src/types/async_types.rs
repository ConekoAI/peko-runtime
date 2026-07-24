//! Compatibility re-export for the `peko_extension_api::async_types` module.
//!
//! `AsyncReceipt` moved into the `peko-extension-api` workspace crate in
//! Phase 7. This shim keeps `peko::extensions::framework::types::AsyncReceipt`
//! paths working unchanged.

pub use peko_extension_api::async_types::*;
