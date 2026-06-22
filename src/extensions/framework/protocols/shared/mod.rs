//! Shared utilities for tool implementations
//!
//! This module provides common functionality used by both Universal Tools
//! and MCP tools to avoid code duplication and ensure consistent behavior.
//!
//! # Modules
//!
//! - `context_resolver`: Unified runtime context field resolution
//! - `schema_filter`: Schema manipulation utilities (filtering reserved params)
//! - `validation`: Security and consistency validation
//! - `process_transport`: Unified process spawning and management
//! - `reserved_params`: Unified reserved parameter configuration format
//! - `proxy_utils`: Common utilities for tool proxy implementations

pub mod context_resolver;
pub mod process_transport;
pub mod proxy_utils;
pub mod schema_filter;
pub mod validation;

pub use context_resolver::ContextResolver;
pub use process_transport::{ProcessConfig, ProcessTransport, ProcessTransportBuilder};
pub use proxy_utils::{estimate_tool_duration, execute_with_context_handling, format_status};
// Reserved params re-exported from extensions::services
pub use crate::extensions::framework::services::ParamSource as ReservedParamSource;
pub use schema_filter::filter_reserved_params;
pub use validation::{validate_no_reserved_params_leak, ValidationError};

// In-flight compat: `ContextSource` was moved to `tools::core::context_source`.
// Re-exported from the new location for one commit while consumers migrate.
pub use crate::tools::core::context_source::ContextSource;
