//! Extension Services
//!
//! This module provides shared services for the Extension system.
//! These services handle cross-cutting concerns like parameter injection,
//! validation, and tool execution.
//!
//! # Architecture
//!
//! ```text
//! +--------------------- EXTENSION SERVICES ------------------------+
//! |                                                                 |
//! |  ReservedParamsService    ToolExecutionService                  |
//! |  - Config parsing         - Parameter injection                 |
//! |  - Validation             - Schema filtering                    |
//! |  - Resolution             - Execution pipeline                  |
//! |                                                                 |
//! +---------------------------+-------------------------------------+
//!                             |
//!                             v
//! +--------------------- EXTENSION ADAPTERS ------------------------+
//! |  (Universal Tool, MCP, etc. - thin wrappers around services)    |
//! +-----------------------------------------------------------------+
//! ```

// Reserved parameters module
pub mod reserved_params;

// Tool execution module
pub mod tool_execution;

// Async execution router module
pub mod async_router;

// Async task transport abstraction (ADR-020)
pub mod async_transport;

// Re-export main types
pub use async_router::{AsyncExecutionRouter, AsyncReservedParams, ToolExecutionContext};
pub use async_transport::{AsyncTaskTransport, DaemonHttpTransport, LocalAsyncTransport, UnavailableAsyncTransport};
pub use reserved_params::{ParamSource, ReservedParamsConfig, ReservedParamsService};
pub use tool_execution::{ToolExecutionConfig, ToolExecutionService};

use std::sync::Arc;

/// Container for all extension services
#[derive(Debug, Clone)]
pub struct Services {
    /// Reserved parameters service
    reserved_params: Arc<reserved_params::ReservedParamsService>,
    /// Tool execution service (with panic isolation and timeout)
    tool_execution: Arc<tool_execution::ToolExecutionService>,
    /// Async execution router (for _async parameter handling)
    async_router: Arc<async_router::AsyncExecutionRouter>,
}

impl Services {
    /// Create new services container with default local transport
    #[must_use]
    pub fn new() -> Self {
        Self::with_transport(async_transport::create_local_transport())
    }

    /// Create services with a custom async task transport
    #[must_use]
    pub fn with_transport(transport: Arc<dyn AsyncTaskTransport>) -> Self {
        Self {
            reserved_params: Arc::new(reserved_params::ReservedParamsService::new()),
            tool_execution: Arc::new(tool_execution::ToolExecutionService::new()),
            async_router: Arc::new(async_router::AsyncExecutionRouter::with_transport(transport)),
        }
    }

    /// Create services by auto-detecting the best transport
    ///
    /// - If daemon is reachable, uses `DaemonHttpTransport`
    /// - Otherwise, returns an error — async tool execution requires the daemon
    pub async fn new_auto() -> anyhow::Result<Self> {
        let transport = async_transport::create_transport().await?;
        Ok(Self::with_transport(transport))
    }

    /// Get the reserved parameters service
    #[must_use] 
    pub fn reserved_params(&self) -> &reserved_params::ReservedParamsService {
        &self.reserved_params
    }

    /// Get the tool execution service
    #[must_use] 
    pub fn tool_execution(&self) -> &tool_execution::ToolExecutionService {
        &self.tool_execution
    }

    /// Get arc to reserved params service
    #[must_use] 
    pub fn reserved_params_arc(&self) -> Arc<reserved_params::ReservedParamsService> {
        self.reserved_params.clone()
    }

    /// Get arc to tool execution service
    #[must_use] 
    pub fn tool_execution_arc(&self) -> Arc<tool_execution::ToolExecutionService> {
        self.tool_execution.clone()
    }

    /// Get the async execution router
    #[must_use] 
    pub fn async_router(&self) -> &async_router::AsyncExecutionRouter {
        &self.async_router
    }

    /// Get arc to async execution router
    #[must_use] 
    pub fn async_router_arc(&self) -> Arc<async_router::AsyncExecutionRouter> {
        self.async_router.clone()
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
