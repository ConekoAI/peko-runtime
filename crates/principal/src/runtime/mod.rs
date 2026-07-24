//! Runtime contracts and shared data types used by the principal layer.
//!
//! Phase 14.c.2a (this PR) introduces two runtime types here:
//!
//! * [`OutputFormat`] — the human/JSON preference flag used by slash
//!   commands and IPC responses. Lifted from `crate::common::types::OutputFormat`.
//! * [`builtin_tools`] — the canonical list of built-in tool names
//!   (global + agent-specific). Lifted from
//!   `crate::extensions::framework::adapters::builtin_tools`. Used by
//!   [`super::extension_store::ExtensionCatalog::build`] to populate
//!   the catalog's "builtin" rows.

pub mod builtin_tools;
pub mod output_format;

pub use output_format::OutputFormat;
