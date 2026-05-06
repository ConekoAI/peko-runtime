//! Extension Services
//!
//! This module provides shared services for the Extension system.
//! These services handle cross-cutting concerns like parameter injection,
//! validation, and tool execution.
//!
//! Async transport infrastructure has been moved to [`crate::extension::transport`].

// Reserved parameters module
pub mod reserved_params;

// Tool execution module
pub mod tool_execution;

// Extension configuration service
pub mod config_service;

// Re-export transport modules for backward compatibility
pub use crate::extension::transport::async_router;
pub use crate::extension::transport::async_transport;

// Re-export transport types for backward compatibility
pub use crate::extension::transport::async_router::{
    AsyncExecutionRouter, AsyncReservedParams, ToolExecutionContext,
};
pub use crate::extension::transport::async_transport::{
    create_local_transport, create_transport, AsyncTaskTransport, DaemonIpcTransport,
    LocalAsyncTransport, UnavailableAsyncTransport,
};

pub use config_service::{ConfigScope, ExtensionConfigService};
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
    async_router: Arc<AsyncExecutionRouter>,
    /// Extension core for hook and tool management (injected, not global)
    core: Option<Arc<crate::extension::core::ExtensionCore>>,
}

impl Services {
    /// Create new services container with default local transport
    #[must_use]
    pub fn new() -> Self {
        Self::with_transport(crate::extension::transport::async_transport::create_local_transport())
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
    pub fn with_core(core: Arc<crate::extension::core::ExtensionCore>) -> Self {
        Self {
            reserved_params: Arc::new(reserved_params::ReservedParamsService::new()),
            tool_execution: Arc::new(tool_execution::ToolExecutionService::new()),
            async_router: Arc::new(AsyncExecutionRouter::with_transport(
                crate::extension::transport::async_transport::create_local_transport(),
            )),
            core: Some(core),
        }
    }

    /// Create services by auto-detecting the best transport
    ///
    /// - If daemon is reachable, uses `DaemonHttpTransport`
    /// - Otherwise, returns an error — async tool execution requires the daemon
    pub async fn new_auto() -> anyhow::Result<Self> {
        let transport = crate::extension::transport::async_transport::create_transport().await?;
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
    pub fn async_router(&self) -> &AsyncExecutionRouter {
        &self.async_router
    }

    /// Get arc to async execution router
    #[must_use]
    pub fn async_router_arc(&self) -> Arc<AsyncExecutionRouter> {
        self.async_router.clone()
    }

    /// Enable built-in hooks for a capability in the injected ExtensionCore
    ///
    /// # Panics
    /// Panics if no ExtensionCore was injected (use `with_core` constructor).
    pub async fn enable_builtin_hooks(&self, capability: &str) {
        let core = self
            .core
            .as_ref()
            .expect("ExtensionCore not injected — use Services::with_core()");
        let builtins = core.list_builtin_extensions().await;
        for b in &builtins {
            if b.name.eq_ignore_ascii_case(capability) {
                let ext_id = crate::extension::types::ExtensionId::new(&b.id);
                let hooks = core.get_hooks_for_extension(&ext_id).await;
                for hook in hooks {
                    let _ = core.enable_hook(&hook.id).await;
                }
                tracing::info!("Enabled built-in hooks for '{}'", b.id);
            }
        }
    }

    /// Disable built-in hooks for a capability in the injected ExtensionCore
    ///
    /// # Panics
    /// Panics if no ExtensionCore was injected (use `with_core` constructor).
    pub async fn disable_builtin_hooks(&self, capability: &str) {
        let core = self
            .core
            .as_ref()
            .expect("ExtensionCore not injected — use Services::with_core()");
        let builtins = core.list_builtin_extensions().await;
        for b in &builtins {
            if b.name.eq_ignore_ascii_case(capability) {
                let ext_id = crate::extension::types::ExtensionId::new(&b.id);
                let hooks = core.get_hooks_for_extension(&ext_id).await;
                for hook in hooks {
                    let _ = core.disable_hook(&hook.id).await;
                }
                tracing::info!("Disabled built-in hooks for '{}'", b.id);
            }
        }
    }

    /// List built-in extensions from the injected ExtensionCore
    ///
    /// Returns an empty vector if no core was injected.
    pub async fn list_builtin_extensions(&self) -> Vec<crate::extension::core::BuiltinExtensionInfo> {
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
        let core = Arc::new(crate::extension::core::ExtensionCore::new());
        let services = Services::with_core(core);
        // Just verify it doesn't panic and core is set
        assert!(services.core.is_some());
    }

    #[tokio::test]
    async fn test_list_builtin_extensions_with_core() {
        let core = Arc::new(crate::extension::core::ExtensionCore::new());
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
