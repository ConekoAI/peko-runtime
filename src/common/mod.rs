//! Common utilities shared across Pekobot
//!
//! This module provides shared functionality used by both CLI and API components,
//! ensuring consistency in path resolution, configuration handling, etc.

pub mod identifiers;
pub mod paths;
pub mod services;
pub mod types;

// Re-export commonly used items
pub use identifiers::{
    parse_agent_identifier, parse_agent_identifier_with_override, validate_agent_name,
    validate_team_name, IdentifierError, ValidationError,
};
pub use paths::{
    default_cache_dir, default_config_dir, default_data_dir, resolve_team_agent,
    resolve_team_agent_with_override, PathResolver, DEFAULT_TEAM,
};
pub use types::{AgentInfo, AgentSummary, TeamInfo, TeamMetadata};
