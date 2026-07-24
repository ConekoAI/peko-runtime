//! Compatibility re-export for the `peko_extension_api::session` module.
//!
//! `PromptBuildState`, `ToolRegistryAccess`, `SessionSnapshot`, and
//! `MessageEnvelope` moved into the `peko-extension-api` workspace
//! crate in Phase 7. This shim keeps
//! `peko::extensions::framework::types::{PromptBuildState,
//! ToolRegistryAccess, SessionSnapshot, MessageEnvelope}` paths
//! working unchanged.

pub use peko_extension_api::session::*;
