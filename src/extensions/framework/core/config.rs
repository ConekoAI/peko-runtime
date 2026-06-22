//! Extension services, configuration, and telemetry
//!
//! This module defines the service locator [`ExtensionServices`] passed to hook
//! handlers, along with [`ExtensionConfig`] and [`TelemetryService`].

use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::types::HookId;
use std::collections::HashMap;
use std::sync::Arc;

/// Extension services available to hook handlers
///
/// This provides access to shared services like logging, configuration,
/// and other cross-cutting concerns.
pub struct ExtensionServices {
    /// Configuration service
    config: ExtensionConfig,

    /// Telemetry/metrics service
    telemetry: TelemetryService,

    /// Tool execution service (handles parameter injection)
    tool_execution: crate::extensions::framework::services::ToolExecutionService,

    /// Reserved parameters service
    reserved_params: crate::extensions::framework::services::ReservedParamsService,

    /// Async execution router
    async_router: crate::extensions::framework::transport::AsyncExecutionRouter,

    /// Stateless agent service for A2A messaging (set by AppState after initialization)
    agent_service: std::sync::RwLock<Option<Arc<crate::agents::StatelessAgentService>>>,

    /// Cross-runtime a2a dispatch context (issue #29). Set by the
    /// daemon-state after the tunnel client is built and the
    /// `HubAgentDirectoryClient` is ready. `None` on runtimes that
    /// haven't run `peko tunnel setup` (no PekoHub credential) or
    /// are running offline. The `A2aSendTool::with_cross_runtime`
    /// builder consults this slot when registering the tool per
    /// agent; tools built without a ctx fall back to the local-only
    /// path.
    cross_runtime_a2a_ctx:
        std::sync::RwLock<Option<Arc<crate::tunnel::CrossRuntimeA2aCtx>>>,
}

impl std::fmt::Debug for ExtensionServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual impl: `StatelessAgentService` no longer derives Debug
        // (carries an `LlmResolver` Arc which has no Debug impl). All
        // other fields are stable identifiers.
        f.debug_struct("ExtensionServices")
            .field("config", &self.config)
            .field("telemetry", &self.telemetry)
            .field("async_router", &self.async_router)
            .field("agent_service", &"<RwLock<Option<Arc<StatelessAgentService>>>>")
            .field("cross_runtime_a2a_ctx", &"<RwLock<Option<Arc<CrossRuntimeA2aCtx>>>>")
            .finish_non_exhaustive()
    }
}

impl ExtensionServices {
    /// Create new extension services with default local transport
    #[must_use]
    pub fn new() -> Self {
        Self::with_async_router(crate::extensions::framework::transport::AsyncExecutionRouter::new())
    }

    /// Create with a custom async execution router and agent service
    #[must_use]
    pub fn with_async_router_and_agent_service(
        async_router: crate::extensions::framework::transport::AsyncExecutionRouter,
        agent_service: Arc<crate::agents::StatelessAgentService>,
    ) -> Self {
        let s = Self::with_async_router(async_router);
        s.set_agent_service(agent_service);
        s
    }

    /// Create with a custom async execution router (for custom transport)
    #[must_use]
    pub fn with_async_router(
        async_router: crate::extensions::framework::transport::AsyncExecutionRouter,
    ) -> Self {
        Self {
            config: ExtensionConfig::default(),
            telemetry: TelemetryService::new(),
            tool_execution: crate::extensions::framework::services::ToolExecutionService::new(),
            reserved_params: crate::extensions::framework::services::ReservedParamsService::new(),
            async_router,
            agent_service: std::sync::RwLock::new(None),
            // Issue #29: cross-runtime a2a ctx starts as None and
            // is filled in by the daemon-state after the tunnel
            // client is wired. Until then, every per-agent
            // A2aSendTool is built without a ctx and falls back to
            // the local-only path (the same behavior as pre-#29).
            cross_runtime_a2a_ctx: std::sync::RwLock::new(None),
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
    pub fn tool_execution(&self) -> &crate::extensions::framework::services::ToolExecutionService {
        &self.tool_execution
    }

    /// Get reserved parameters service
    pub fn reserved_params(&self) -> &crate::extensions::framework::services::ReservedParamsService {
        &self.reserved_params
    }

    /// Get async execution router
    pub fn async_router(&self) -> &crate::extensions::framework::transport::AsyncExecutionRouter {
        &self.async_router
    }

    /// Set the stateless agent service (for A2A messaging)
    pub fn set_agent_service(&self, service: Arc<crate::agents::StatelessAgentService>) {
        if let Ok(mut guard) = self.agent_service.write() {
            *guard = Some(service);
        }
    }

    /// Get the stateless agent service (for A2A messaging)
    #[must_use]
    pub fn agent_service(&self) -> Option<Arc<crate::agents::StatelessAgentService>> {
        self.agent_service.read().ok().and_then(|g| g.clone())
    }

    /// Set the cross-runtime a2a dispatch context (issue #29). The
    /// daemon-state calls this after the tunnel client is built and
    /// the `HubAgentDirectoryClient` is wired; the per-agent tool
    /// constructor in `agent.rs` reads via `cross_runtime_a2a_ctx`
    /// and injects the ctx into each `A2aSendTool` it builds.
    pub fn set_cross_runtime_a2a_ctx(
        &self,
        ctx: Arc<crate::tunnel::CrossRuntimeA2aCtx>,
    ) {
        if let Ok(mut guard) = self.cross_runtime_a2a_ctx.write() {
            *guard = Some(ctx);
        }
    }

    /// Get the cross-runtime a2a dispatch context, if one is set.
    /// Returns `None` on runtimes that haven't initialized
    /// cross-runtime dispatch (offline runtimes, runtimes without
    /// a PekoHub credential, runtimes before this PR's bootstrap
    /// follow-up).
    #[must_use]
    pub fn cross_runtime_a2a_ctx(&self) -> Option<Arc<crate::tunnel::CrossRuntimeA2aCtx>> {
        self.cross_runtime_a2a_ctx
            .read()
            .ok()
            .and_then(|g| g.clone())
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
