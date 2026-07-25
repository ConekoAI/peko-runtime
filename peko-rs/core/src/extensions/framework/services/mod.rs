//! Extension services (Phase 8b + Phase 8c.1.D.3 completion).
//!
//! Phase 8b moved `reserved_params.rs` + `tool_execution.rs` because
//! `transport::async_router` depends on them. Phase 8c.1.D.3 adds
//! `config_service.rs` and the `Services` orchestrator struct (with
//! `new`/`with_transport`/`with_core`; `new_auto` deleted because it
//! had no callers — Agent 1 finding). The `core: Option<Arc<ExtensionCore>>`
//! field references the host's `ExtensionCore` (lifted in 8a, no rename).

pub mod config_service;
pub mod reserved_params;
pub mod tool_execution;

pub use config_service::{ConfigScope, ExtensionConfigData, ExtensionConfigService};
pub use reserved_params::{ParamSource, ReservedParamsConfig, ReservedParamsService};
pub use tool_execution::{ToolExecutionConfig, ToolExecutionService};
// `ToolExecutionContext` was promoted to live alongside the router in
// `transport::async_router` (mirrors how the trait port calls it);
// re-exported here for backwards compat with the historical
// `services::ToolExecutionContext` import path.
pub use crate::transport::async_router::ToolExecutionContext;

use crate::core::BuiltinExtensionInfo;
use crate::transport::async_router::AsyncExecutionRouter;
use crate::transport::async_transport::{create_local_transport, AsyncTaskTransport};
use std::sync::Arc;

/// Container for all extension services
#[derive(Debug, Clone)]
pub struct Services {
    /// Reserved parameters service
    reserved_params: Arc<reserved_params::ReservedParamsService>,
    /// Tool execution service (with panic isolation and timeout)
    tool_execution: Arc<tool_execution::ToolExecutionService>,
    /// Async execution router (for _async parameter handling)
    async_router: Arc<AsyncExecutionRouter>,
    /// Extension core for hook and tool management (injected, not global)
    core: Option<Arc<crate::core::ExtensionCore>>,
}

impl Services {
    /// Create new services container with default local transport
    #[must_use]
    pub fn new() -> Self {
        Self::with_transport(create_local_transport())
    }

    /// Create services with a custom async task transport
    #[must_use]
    pub fn with_transport(transport: Arc<dyn AsyncTaskTransport>) -> Self {
        Self {
            reserved_params: Arc::new(reserved_params::ReservedParamsService::new()),
            tool_execution: Arc::new(tool_execution::ToolExecutionService::new()),
            async_router: Arc::new(AsyncExecutionRouter::with_transport(transport)),
            core: None,
        }
    }

    /// Create services with an injected ExtensionCore
    ///
    /// This is the preferred constructor for CLI commands that need to
    /// manipulate hooks and built-in extensions without reaching for global state.
    #[must_use]
    pub fn with_core(core: Arc<crate::core::ExtensionCore>) -> Self {
        Self {
            reserved_params: Arc::new(reserved_params::ReservedParamsService::new()),
            tool_execution: Arc::new(tool_execution::ToolExecutionService::new()),
            async_router: Arc::new(AsyncExecutionRouter::with_transport(
                create_local_transport(),
            )),
            core: Some(core),
        }
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
    pub fn async_router(&self) -> &AsyncExecutionRouter {
        &self.async_router
    }

    /// Get arc to async execution router
    #[must_use]
    pub fn async_router_arc(&self) -> Arc<AsyncExecutionRouter> {
        self.async_router.clone()
    }

    /// List built-in extensions from the injected ExtensionCore
    ///
    /// Returns an empty vector if no core was injected.
    pub async fn list_builtin_extensions(&self) -> Vec<BuiltinExtensionInfo> {
        match self.core {
            Some(ref core) => core.list_builtin_extensions().await,
            None => Vec::new(),
        }
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

    #[test]
    fn test_services_with_core() {
        let core = Arc::new(crate::core::ExtensionCore::new());
        let services = Services::with_core(core);
        // Just verify it doesn't panic and core is set
        assert!(services.core.is_some());
    }

    #[tokio::test]
    async fn test_list_builtin_extensions_with_core() {
        let core = Arc::new(crate::core::ExtensionCore::new());
        let services = Services::with_core(core);
        let builtins = services.list_builtin_extensions().await;
        // Initially empty since no builtins are registered
        assert!(builtins.is_empty());
    }

    #[tokio::test]
    async fn test_list_builtin_extensions_without_core() {
        let services = Services::new();
        let builtins = services.list_builtin_extensions().await;
        assert!(builtins.is_empty());
    }
}
