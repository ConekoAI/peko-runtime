//! Common utilities shared across Peko
//!
//! This module provides shared functionality used by both CLI and API components,
//! ensuring consistency in path resolution, configuration handling, etc.

pub mod config_path;
pub mod identifiers;
pub mod json_utils;
pub mod paths;
pub mod process;
// `registry` was moved to `peko-extension-host` in Phase 8. The
// shim keeps `crate::common::registry::*` paths working until
// Phase 10 deletes it (the only non-framework consumer is
// `tools/builtin/session.rs`, which moves to `peko-tools-builtin`).
pub mod registry {
    pub use peko_extension_host::registry::*;
}
pub mod services;
pub mod time;
pub mod types;
pub mod vault;
pub mod vault_credential_provider;
pub mod vault_secret_store;

// Re-export commonly used items
pub use identifiers::{parse_agent_name, validate_agent_name, IdentifierError, ValidationError};
pub use paths::{default_cache_dir, default_config_dir, default_data_dir, PathResolver};
pub use time::{format_timestamp, format_timestamp_ms, format_timestamp_rfc3339};
