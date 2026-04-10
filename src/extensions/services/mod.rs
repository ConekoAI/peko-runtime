//! Extension Services
//!
//! This module provides shared services for the Extension system.
//! These services handle cross-cutting concerns like parameter injection,
//! validation, and tool execution.
//!
//! # Architecture
//!
//! ```
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    EXTENSION SERVICES                           │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                                                                 │
//! │  ReservedParamsService    ToolExecutionService                  │
//! │  ├── Config parsing       ├── Parameter injection               │
//! │  ├── Validation           ├── Schema filtering                  │
//! │  └── Resolution           └── Execution pipeline                │
//! │                                                                 │
//! └─────────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    EXTENSION ADAPTERS                           │
//! │  (Universal Tool, MCP, etc. - thin wrappers around services)    │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

// Reserved parameters module
pub mod reserved_params;

// Tool execution module
pub mod tool_execution;

// Re-export main types
pub use reserved_params::{ParamSource, ReservedParamsConfig, ReservedParamsService};
pub use tool_execution::{ToolExecutionConfig, ToolExecutionService};

use std::sync::Arc;

/// Container for all extension services
#[derive(Debug, Clone)]
pub struct Services {
    /// Reserved parameters service
    reserved_params: Arc<reserved_params::ReservedParamsService>,
    /// Tool execution service
    tool_execution: Arc<tool_execution::ToolExecutionService>,
}

impl Services {
    /// Create new services container
    pub fn new() -> Self {
        Self {
            reserved_params: Arc::new(reserved_params::ReservedParamsService::new()),
            tool_execution: Arc::new(tool_execution::ToolExecutionService::new()),
        }
    }

    /// Get the reserved parameters service
    pub fn reserved_params(&self) -> &reserved_params::ReservedParamsService {
        &self.reserved_params
    }

    /// Get the tool execution service
    pub fn tool_execution(&self) -> &tool_execution::ToolExecutionService {
        &self.tool_execution
    }

    /// Get arc to reserved params service
    pub fn reserved_params_arc(&self) -> Arc<reserved_params::ReservedParamsService> {
        self.reserved_params.clone()
    }

    /// Get arc to tool execution service
    pub fn tool_execution_arc(&self) -> Arc<tool_execution::ToolExecutionService> {
        self.tool_execution.clone()
    }
}

impl Default for Services {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_services_creation() {
        let services = Services::new();
        assert!(Arc::strong_count(&services.reserved_params_arc()) == 2);
        assert!(Arc::strong_count(&services.tool_execution_arc()) == 2);
    }

    #[test]
    fn test_services_default() {
        let services: Services = Default::default();
        // Just verify it doesn't panic
        let _ = services.reserved_params();
        let _ = services.tool_execution();
    }
}
