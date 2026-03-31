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

pub mod context_resolver;
pub mod schema_filter;
pub mod validation;

pub use context_resolver::ContextResolver;
pub use schema_filter::filter_reserved_params;
pub use validation::{validate_no_reserved_params_leak, ValidationError};
