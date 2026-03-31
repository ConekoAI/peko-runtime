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

pub mod context_resolver;
pub mod process_transport;
pub mod reserved_params;
pub mod schema_filter;
pub mod validation;

pub use context_resolver::ContextResolver;
pub use process_transport::{ProcessConfig, ProcessTransport, ProcessTransportBuilder};
pub use reserved_params::{ReservedParam, ReservedParamSource, ReservedParams, resolve_all};
pub use schema_filter::filter_reserved_params;
pub use validation::{validate_no_reserved_params_leak, ValidationError};
