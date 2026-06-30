//! Common utilities shared across Peko
//!
//! This module provides shared functionality used by both CLI and API components,
//! ensuring consistency in path resolution, configuration handling, etc.

pub mod config_path;
pub mod identifiers;
pub mod json_utils;
pub mod paths;
pub mod process;
pub mod registry;
pub mod secret_store;
pub mod services;
pub mod time;
pub mod types;
pub mod vault;

// Re-export commonly used items
pub use identifiers::{
    parse_agent_identifier, parse_agent_identifier_with_override, validate_agent_name,
    IdentifierError, ValidationError,
};
pub use paths::{
    default_cache_dir, default_config_dir, default_data_dir, resolve_team_agent,
    resolve_team_agent_with_override, PathResolver,
};
pub use time::{format_timestamp, format_timestamp_ms, format_timestamp_rfc3339};
pub use types::{AgentInfo, AgentSummary};
