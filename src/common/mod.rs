//! Common utilities shared across Pekobot
//!
//! This module provides shared functionality used by both CLI and API components,
//! ensuring consistency in path resolution, configuration handling, etc.

pub mod paths;

// Re-export commonly used items
pub use paths::{
    default_cache_dir, default_config_dir, default_data_dir, resolve_team_agent,
    resolve_team_agent_with_override, PathResolver, DEFAULT_TEAM,
};
