//! Shared utilities for tool implementations
//!
//! This module provides common functionality used by both Universal Tools
//! and MCP tools to avoid code duplication and ensure consistent behavior.
//!
//! # Modules
//!
//! - `context_resolver`: Unified runtime context field resolution
//! - `validation`: Security and consistency validation
//! - `process_transport`: Unified process spawning and management
//! - `reserved_params`: Unified reserved parameter configuration format
//! - `proxy_utils`: Common utilities for tool proxy implementations
//!
//! Phase 8b.2 deleted the `schema_filter` root shim (byte-identical to
//! `peko_extension_host::protocols::shared::schema_filter`) and re-exports
//! its public surface here so the historical
//! `crate::extensions::framework::protocols::shared::filter_reserved_params`
//! path keeps resolving.

pub mod process_transport;
pub mod proxy_utils;
pub mod validation;

pub use process_transport::{ProcessConfig, ProcessTransport, ProcessTransportBuilder};
pub use proxy_utils::{estimate_tool_duration, execute_with_context_handling, format_status};
// Reserved params re-exported from extensions::services
pub use crate::extensions::framework::services::ParamSource as ReservedParamSource;
pub use peko_extension_host::protocols::shared::schema_filter::*;
pub use validation::{validate_no_reserved_params_leak, ValidationError};
