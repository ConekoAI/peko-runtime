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
#[allow(dead_code)] // tool_execution + reserved_params stay until 8c services/ lifts
pub struct ExtensionServices {
    /// Configuration service
    config: ExtensionConfig,

    /// Telemetry/metrics service
    telemetry: TelemetryService,

    /// Tool execution service (handles parameter injection).
    ///
    /// Type-erased to `Arc<dyn Any + Send + Sync>` in Phase 8a. No
    /// method on this service is called — ADR-066 P1 deleted the
    /// concrete `services::ToolExecutionService`; the slot stays
    /// until the `ExtensionServices` collapse (P3).
    tool_execution: Arc<dyn std::any::Any + Send + Sync>,

    /// Reserved parameters service.
    ///
    /// Type-erased for the same reason as `tool_execution`. The
    /// concrete `services::ReservedParamsService` lives in root.
    reserved_params: Arc<dyn std::any::Any + Send + Sync>,

    // Sprint 9 Commit 4: `principal_message_service` slot retired.
    // `StatelessAgentService` was the sole
    // `PrincipalMessageService` impl; its only caller was the
    // chat-gateway adapter framework deleted in Commit 3. Per-peer
    // standing children (the agent-session paradigm) own principal
    // dispatch directly via `PrincipalManager::receive_streaming`.
    /// Cross-runtime a2a dispatch context (issue #29). Set by the
    /// daemon-state after the tunnel client is built and the
    /// `HubAgentDirectoryClient` is ready. `None` on runtimes that
    /// haven't run `peko tunnel setup` (no PekoHub credential) or
    /// are running offline.
    ///
    /// Stored as `Arc<dyn Any + Send + Sync>` so the framework does
    /// not depend on the concrete `tunnel::CrossRuntimeA2aCtx` type.
    /// Consumers downcast to the concrete type when building tools.
    cross_runtime_a2a_ctx:
        std::sync::RwLock<Option<Arc<dyn std::any::Any + Send + Sync + 'static>>>,

    /// Runtime LLM resolver. Set by AppState once the resolver is built
    /// so that extension code (e.g. MCP sampling) can request host-model
    /// completions without holding provider-specific state.
    llm_resolver: std::sync::RwLock<Option<Arc<peko_providers::LlmResolver>>>,

    /// Channel port (sprint 4 — `ChannelSend` per-agent tool needs the
    /// file-backed `ChannelPort` so the bare / group / principal branches
    /// can post to channels. Set by AppState once the channel store is
    /// wired; `None` on tests that construct an `ExtensionServices` via
    /// `new()` without a real channel store.
    channel_port: std::sync::RwLock<Option<Arc<dyn peko_channel::ChannelPort>>>,
}

impl std::fmt::Debug for ExtensionServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionServices")
            .field("config", &self.config)
            .field("telemetry", &self.telemetry)
            .field(
                "cross_runtime_a2a_ctx",
                &"<RwLock<Option<Arc<dyn Any + Send + Sync>>>>",
            )
            .field("llm_resolver", &"<RwLock<Option<Arc<LlmResolver>>>>")
            .field("channel_port", &"<RwLock<Option<Arc<dyn ChannelPort>>>>")
            // Sprint 9 Commit 4: `principal_message_service` field
            // removed; Debug formatter no longer emits it.
            .finish_non_exhaustive()
    }
}

impl ExtensionServices {
    /// Create new extension services (ADR-066 P3: the async-router slot
    /// moved to the `ToolDispatcher`; `ExtensionServices` now carries
    /// only the hook-context slots: channel port, cross-runtime ctx).
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: ExtensionConfig::default(),
            telemetry: TelemetryService::new(),
            tool_execution: Arc::new(()),
            reserved_params: Arc::new(()),
            // Issue #29: cross-runtime a2a ctx starts as None and
            // is filled in by the daemon-state after the tunnel
            // client is wired. Until then, every per-agent
            // PrincipalSendTool is built without a ctx and falls back to
            // the local-only path (the same behavior as pre-#29).
            cross_runtime_a2a_ctx: std::sync::RwLock::new(None),
            llm_resolver: std::sync::RwLock::new(None),
            channel_port: std::sync::RwLock::new(None),
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

    // Sprint 9 Commit 4: `set_principal_message_service` and
    // `principal_message_service` getter retired along with the
    // slot. Per-peer standing children (the agent-session paradigm)
    // own principal dispatch via `PrincipalManager::receive_streaming`.

    /// Set the cross-runtime a2a dispatch context (issue #29). The
    /// daemon-state calls this after the tunnel client is built and
    /// the `HubAgentDirectoryClient` is wired; the per-agent tool
    /// constructor in `agent.rs` reads via `cross_runtime_a2a_ctx`
    /// and injects the ctx into each `PrincipalSendTool` it builds.
    pub fn set_cross_runtime_a2a_ctx(&self, ctx: Arc<dyn std::any::Any + Send + Sync + 'static>) {
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
    pub fn cross_runtime_a2a_ctx(&self) -> Option<Arc<dyn std::any::Any + Send + Sync + 'static>> {
        self.cross_runtime_a2a_ctx
            .read()
            .ok()
            .and_then(|g| g.clone())
    }

    /// Set the runtime LLM resolver. Called by AppState once the resolver
    /// has been constructed.
    pub fn set_llm_resolver(&self, resolver: Arc<peko_providers::LlmResolver>) {
        if let Ok(mut guard) = self.llm_resolver.write() {
            *guard = Some(resolver);
        }
    }

    /// Get the runtime LLM resolver, if one has been set.
    #[must_use]
    pub fn llm_resolver(&self) -> Option<Arc<peko_providers::LlmResolver>> {
        self.llm_resolver.read().ok().and_then(|g| g.clone())
    }

    /// Set the channel port. Called by AppState once the channel store
    /// has been wired (the same handle that `PrincipalManager::channel_port`
    /// already caches). The per-agent `ChannelSendTool` constructor in
    /// `agent.rs` reads via `channel_port` so the bare / group / principal
    /// branches can post to channels.
    pub fn set_channel_port(&self, port: Arc<dyn peko_channel::ChannelPort>) {
        if let Ok(mut guard) = self.channel_port.write() {
            *guard = Some(port);
        }
    }

    /// Get the channel port, if one is installed.
    #[must_use]
    pub fn channel_port(&self) -> Option<Arc<dyn peko_channel::ChannelPort>> {
        self.channel_port.read().ok().and_then(|g| g.clone())
    }

    /// Record a hook invocation
    pub fn record_invocation(&self, hook_id: &HookId, point: &HookPoint, duration_ms: u64) {
        self.telemetry
            .record_hook_invocation(hook_id, point, duration_ms);
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
