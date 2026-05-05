//! Extension Protocols — Backward Compatibility Re-exports
//!
//! This module is preserved during Phase 2 migration as a compatibility layer.
//! Protocol implementations have moved to `src/extensions/<type>/protocol/` or
//! `src/extension/protocols/shared/` (framework).
//!
//! # New Locations
//! - `crate::extensions::gateway::protocol` — Gateway IPC Protocol
//! - `crate::extensions::universal::protocol` — Universal Tool Protocol
//! - `crate::extension::protocols::shared` — Shared framework utilities

// `shared` module lives in the framework (src/extension/protocols/shared/)
pub use crate::extension::protocols::shared as shared;

// Re-export shared utilities from the framework
pub use crate::extension::protocols::shared::{
    ContextResolver, ProcessConfig, ProcessTransport, ProcessTransportBuilder,
    filter_reserved_params, validate_no_reserved_params_leak, ValidationError,
    estimate_tool_duration, execute_with_context_handling, format_status,
};
