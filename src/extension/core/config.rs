//! Extension services, configuration, and telemetry
//!
//! This module defines the service locator [`ExtensionServices`] passed to hook
//! handlers, along with [`ExtensionConfig`] and [`TelemetryService`].

use crate::extension::core::hook_points::HookPoint;
use crate::extension::types::{HookId};
use std::collections::HashMap;
use std::sync::Arc;

/// Extension services available to hook handlers
///
/// This provides access to shared services like logging, configuration,
/// and other cross-cutting concerns.
#[derive(Debug)]
pub struct ExtensionServices {
    /// Configuration service
    config: ExtensionConfig,

    /// Telemetry/metrics service
    telemetry: TelemetryService,

    /// Tool execution service (handles parameter injection)
    tool_execution: crate::extension::services::ToolExecutionService,

    /// Reserved parameters service
    reserved_params: crate::extension::services::ReservedParamsService,

    /// Async execution router
    async_router: crate::extension::transport::AsyncExecutionRouter,

    /// Stateless agent service for A2A messaging (set by AppState after initialization)
    agent_service: std::sync::RwLock<Option<Arc<crate::agent::StatelessAgentService>>>,
}

impl ExtensionServices {
    /// Create new extension services with default local transport
    #[must_use]
    pub fn new() -> Self {
        Self::with_async_router(crate::extension::transport::AsyncExecutionRouter::new())
    }

    /// Create with a custom async execution router and agent service
    #[must_use]
    pub fn with_async_router_and_agent_service(
        async_router: crate::extension::transport::AsyncExecutionRouter,
        agent_service: Arc<crate::agent::StatelessAgentService>,
    ) -> Self {
        let s = Self::with_async_router(async_router);
        s.set_agent_service(agent_service);
        s
    }

    /// Create with a custom async execution router (for custom transport)
    #[must_use]
    pub fn with_async_router(
        async_router: crate::extension::transport::AsyncExecutionRouter,
    ) -> Self {
        Self {
            config: ExtensionConfig::default(),
            telemetry: TelemetryService::new(),
            tool_execution: crate::extension::services::ToolExecutionService::new(),
            reserved_params: crate::extension::services::ReservedParamsService::new(),
            async_router,
            agent_service: std::sync::RwLock::new(None),
        }
    }

    /// Get configuration
    pub fn config(&self) -> &ExtensionConfig {
        &self.config
    }

    /// Get telemetry service
    pub fn telemetry(&self) -> &TelemetryService {
        &self.telemetry
    }

    /// Get tool execution service
    pub fn tool_execution(&self) -> &crate::extension::services::ToolExecutionService {
        &self.tool_execution
    }

    /// Get reserved parameters service
    pub fn reserved_params(&self) -> &crate::extension::services::ReservedParamsService {
        &self.reserved_params
    }

    /// Get async execution router
    pub fn async_router(&self) -> &crate::extension::transport::AsyncExecutionRouter {
        &self.async_router
    }

    /// Set the stateless agent service (for A2A messaging)
    pub fn set_agent_service(&self, service: Arc<crate::agent::StatelessAgentService>) {
        if let Ok(mut guard) = self.agent_service.write() {
            *guard = Some(service);
        }
    }

    /// Get the stateless agent service (for A2A messaging)
    #[must_use]
    pub fn agent_service(&self) -> Option<Arc<crate::agent::StatelessAgentService>> {
        self.agent_service.read().ok().and_then(|g| g.clone())
    }

    /// Record a hook invocation
    pub fn record_invocation(&self, hook_id: &HookId, point: &HookPoint, duration_ms: u64) {
        self.telemetry
            .record_hook_invocation(hook_id, point, duration_ms);
    }

    /// Wait for all async tasks to complete
    pub async fn wait_for_async_tasks(&self, timeout: std::time::Duration) {
        self.async_router.wait_for_all_tasks(timeout).await;
    }
}

impl Default for ExtensionServices {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for extensions
#[derive(Debug, Default)]
pub struct ExtensionConfig {
    /// Maximum hook execution time in milliseconds
    pub max_hook_duration_ms: u64,

    /// Enable hook tracing
    pub enable_tracing: bool,

    /// Extension-specific configuration
    pub extension_settings: HashMap<String, serde_json::Value>,
}

impl ExtensionConfig {
    /// Create default configuration
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_hook_duration_ms: 5000, // 5 seconds default
            enable_tracing: false,
            extension_settings: HashMap::new(),
        }
    }

    /// Get a setting for a specific extension
    #[must_use]
    pub fn get_extension_setting(
        &self,
        extension_id: &str,
        key: &str,
    ) -> Option<&serde_json::Value> {
        self.extension_settings
            .get(extension_id)
            .and_then(|v| v.get(key))
    }
}

/// Telemetry service for hook metrics
#[derive(Debug)]
pub struct TelemetryService {
    /// Invocation counts by hook point
    invocation_counts: std::sync::Mutex<HashMap<String, u64>>,

    /// Total execution time by hook point
    execution_times: std::sync::Mutex<HashMap<String, u64>>,
}

impl TelemetryService {
    /// Create new telemetry service
    #[must_use]
    pub fn new() -> Self {
        Self {
            invocation_counts: std::sync::Mutex::new(HashMap::new()),
            execution_times: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Record a hook invocation
    pub fn record_hook_invocation(&self, _hook_id: &HookId, point: &HookPoint, duration_ms: u64) {
        let name = point.name();

        if let Ok(mut counts) = self.invocation_counts.lock() {
            *counts.entry(name.clone()).or_insert(0) += 1;
        }

        if let Ok(mut times) = self.execution_times.lock() {
            *times.entry(name).or_insert(0) += duration_ms;
        }
    }

    /// Get invocation count for a hook point
    pub fn get_invocation_count(&self, point: &HookPoint) -> u64 {
        if let Ok(counts) = self.invocation_counts.lock() {
            counts.get(&point.name()).copied().unwrap_or(0)
        } else {
            0
        }
    }

    /// Get average execution time for a hook point
    pub fn get_average_execution_time(&self, point: &HookPoint) -> u64 {
        let name = point.name();

        let count = if let Ok(counts) = self.invocation_counts.lock() {
            counts.get(&name).copied().unwrap_or(0)
        } else {
            0
        };

        if count == 0 {
            return 0;
        }

        let total_time = if let Ok(times) = self.execution_times.lock() {
            times.get(&name).copied().unwrap_or(0)
        } else {
            0
        };

        total_time / count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_config() {
        let config = ExtensionConfig::new();
        assert_eq!(config.max_hook_duration_ms, 5000);
        assert!(!config.enable_tracing);
    }

    #[test]
    fn test_telemetry_service() {
        let telemetry = TelemetryService::new();
        let point = HookPoint::ToolRegister;
        let hook_id = HookId::new();

        telemetry.record_hook_invocation(&hook_id, &point, 100);
        telemetry.record_hook_invocation(&hook_id, &point, 200);

        assert_eq!(telemetry.get_invocation_count(&point), 2);
        assert_eq!(telemetry.get_average_execution_time(&point), 150);
    }
}
