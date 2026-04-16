//! ConfigAuthority - Central Agent Configuration System
//!
//! This module provides the single central authority for agent configuration
//! management, replacing the previous dual-system (AgentConfigService + ConfigRegistry).
//!
//! ## Architecture
//!
//! - `authority_trait.rs`: Defines the `ConfigAuthority` async trait
//! - `implementation.rs`: Main implementation (`ConfigAuthorityImpl`)
//! - `entry.rs`: Unified `AgentConfigEntry` and `ConfigSource` types
//! - `cache.rs`: In-memory caching wrapper
//! - `io.rs`: TOML file I/O and `ApiKeyResolver`
//! - `migration.rs`: JSON to TOML migration utilities
//!
//! ## Usage
//!
//! ```ignore
//! use crate::common::services::config_authority::{ConfigAuthority, ConfigAuthorityImpl};
//!
//! let authority = ConfigAuthorityImpl::new(path_resolver);
//! let entry = authority.get("my-agent", Some("default")).await?;
//! ```

pub mod authority_trait;
pub mod cache;
pub mod entry;
pub mod implementation;
pub mod io;
pub mod migration;

// Re-export types for convenience
pub use authority_trait::{ConfigAuthority, ConfigError, ConfigResult};
pub use entry::{AgentConfigEntry, ConfigSource};
pub use implementation::ConfigAuthorityImpl;
