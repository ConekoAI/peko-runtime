//! Extension services, configuration, and telemetry
//!
//! This module defines the service locator [`ExtensionServices`] passed to hook
//! handlers, along with [`ExtensionConfig`] and [`TelemetryService`].

use crate::core::context::HookContext;
use crate::core::hook_points::HookPoint;
use crate::transport::AsyncExecutionRouter;
use crate::types::HookId;
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

    /// Tool execution service (handles parameter injection).
    ///
    /// Type-erased to `Arc<dyn Any + Send + Sync>` in Phase 8a. The
    /// concrete `services::ToolExecutionService` lives in root
    /// `framework/services/` (lifted in 8c). No method on this
    /// service is called from within the host crate — only from the
    /// transport subtree that stays in root until 8b — so we don't
    /// need a trait port yet. 8c will replace this with a proper
    /// trait object once services/ lifts.
    tool_execution: Arc<dyn std::any::Any + Send + Sync>,

    /// Reserved parameters service.
    ///
    /// Type-erased for the same reason as `tool_execution`. The
    /// concrete `services::ReservedParamsService` lives in root.
    reserved_params: Arc<dyn std::any::Any + Send + Sync>,

    /// Async execution router.
    ///
    /// Stored as `Arc<dyn AsyncExecutionRouter>` (Phase 8a trait
    /// port). The concrete `transport::AsyncExecutionRouter` stays
    /// in root until Phase 8b. The impl is `Send + Sync` so the
    /// trait object can live in `ExtensionServices` and be cloned
    /// into per-agent `ExtensionCore` instances.
    async_router: Arc<dyn AsyncExecutionRouter>,

    /// Stateless principal message service (set by AppState after initialization).
    /// Implements principal-to-principal message dispatch
    /// ([`crate::principal_message::PrincipalMessageService`]).
    /// Held as a trait object to avoid a framework → agents dependency.
    principal_message_service:
        std::sync::RwLock<Option<Arc<dyn crate::principal_message::PrincipalMessageService>>>,

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
}

impl std::fmt::Debug for ExtensionServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual impl: `StatelessAgentService` no longer derives Debug
        // (carries an `LlmResolver` Arc which has no Debug impl). All
        // other fields are stable identifiers.
        f.debug_struct("ExtensionServices")
            .field("config", &self.config)
            .field("telemetry", &self.telemetry)
            .field("async_router", &"<dyn AsyncExecutionRouter>")
            .field(
                "principal_message_service",
                &"<RwLock<Option<Arc<dyn PrincipalMessageService>>>>",
            )
            .field(
                "cross_runtime_a2a_ctx",
                &"<RwLock<Option<Arc<dyn Any + Send + Sync>>>>",
            )
            .field("llm_resolver", &"<RwLock<Option<Arc<LlmResolver>>>>")
            .finish_non_exhaustive()
    }
}

impl ExtensionServices {
    /// Create new extension services with a no-op async router.
    ///
    /// The default router does nothing — `execute_from_hook` returns
    /// `HookResult::PassThrough` and `wait_for_all_tasks` returns
    /// immediately. Production callers (daemon, CLI, main) should
    /// use [`Self::with_async_router`] and pass a real router
    /// wrapping the local or HTTP transport. The no-op default keeps
    /// the host self-contained and avoids a host → root dep on the
    /// concrete `framework::transport::AsyncExecutionRouter`
    /// (lifted in Phase 8b).
    #[must_use]
    pub fn new() -> Self {
        Self::with_async_router(Arc::new(NoopAsyncExecutionRouter))
    }

    /// Create with a custom async execution router and principal message service
    #[must_use]
    pub fn with_async_router_and_principal_message_service(
        async_router: Arc<dyn AsyncExecutionRouter>,
        principal_message_service: Arc<dyn crate::principal_message::PrincipalMessageService>,
    ) -> Self {
        let s = Self::with_async_router(async_router);
        s.set_principal_message_service(principal_message_service);
        s
    }

    /// Create with a custom async execution router (for custom transport)
    #[must_use]
    pub fn with_async_router(async_router: Arc<dyn AsyncExecutionRouter>) -> Self {
        Self {
            config: ExtensionConfig::default(),
            telemetry: TelemetryService::new(),
            tool_execution: Arc::new(()),
            reserved_params: Arc::new(()),
            async_router,
            principal_message_service: std::sync::RwLock::new(None),
            // Issue #29: cross-runtime a2a ctx starts as None and
            // is filled in by the daemon-state after the tunnel
            // client is wired. Until then, every per-agent
            // PrincipalSendTool is built without a ctx and falls back to
            // the local-only path (the same behavior as pre-#29).
            cross_runtime_a2a_ctx: std::sync::RwLock::new(None),
            llm_resolver: std::sync::RwLock::new(None),
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

    /// Get the async execution router.
    ///
    /// Returns the trait-object reference so callers can dispatch via
    /// the [`AsyncExecutionRouter`] port. Root-side adapters
    /// (`extensions/{builtin,mcp,universal}/adapter.rs`) call
    /// `.execute_from_hook(...)` on this; the host's
    /// [`Self::wait_for_async_tasks`] delegates to
    /// `.wait_for_all_tasks(...)`.
    pub fn async_router(&self) -> &Arc<dyn AsyncExecutionRouter> {
        &self.async_router
    }

    /// Set the stateless principal message service
    pub fn set_principal_message_service(
        &self,
        service: Arc<dyn crate::principal_message::PrincipalMessageService>,
    ) {
        if let Ok(mut guard) = self.principal_message_service.write() {
            *guard = Some(service);
        }
    }

    /// Get the stateless principal message service
    #[must_use]
    pub fn principal_message_service(
        &self,
    ) -> Option<Arc<dyn crate::principal_message::PrincipalMessageService>> {
        self.principal_message_service
            .read()
            .ok()
            .and_then(|g| g.clone())
    }

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

/// Synchronous-local [`AsyncExecutionRouter`] used as the default router by
/// [`ExtensionServices::new`].
///
/// The router pulls the tool-call input out of `ctx`, runs the F32b JSON-Schema
/// validator, runs the caller's preprocessor (if any), invokes the caller's
/// `exec_fn` (the actual tool call), and maps the `Result<Value>` back to a
/// `HookResult`. This is enough to satisfy the F37 funnel used by
/// `BuiltinExecuteHandler` (and the parallel MCP / Universal adapters) when
/// no real daemon-side `AsyncExecutor` is wired in — typical for unit tests
/// that construct an `ExtensionCore` via `ExtensionCore::new()`.
///
/// Production callers (the daemon, CLI `peko` binary, scenario tests) should
/// override with a real router via [`ExtensionServices::with_async_router`]
/// so async tools (`AsyncSpawn`, `AsyncOutput`, …) reach the
/// `AsyncExecutor`'s spawn path. Sync tools work either way.
struct NoopAsyncExecutionRouter;

#[async_trait::async_trait]
impl AsyncExecutionRouter for NoopAsyncExecutionRouter {
    async fn execute_from_hook(
        &self,
        ctx: &HookContext,
        tool_name: &str,
        exec_config: &crate::transport::ToolExecConfig,
        preprocessor: Option<crate::transport::PreprocessorFn>,
        exec_fn: crate::transport::ExecFn,
    ) -> crate::types::HookResult {
        // Pull the (name, params, workspace) triple out of the tool-call
        // input. If the input isn't a ToolCall (e.g. a prompt-section
        // hook mistakenly routed here), fall back to PassThrough.
        let (called_tool_name, mut params, workspace) = match ctx.as_tool_call() {
            Some((name, params, ws)) => (name.to_string(), params.clone(), ws.map(str::to_string)),
            None => return crate::types::HookResult::PassThrough,
        };

        // F32b — validate LLM-emitted args against the tool's declared JSON
        // schema before invoking the preprocessor. Mirrors the root-side
        // router's behavior so the noop-router is faithful enough for
        // production semantics on synchronous tools.
        if let Err(msg) = validate_tool_args(&exec_config.full_schema, &params, tool_name) {
            return crate::types::HookResult::Error(anyhow::anyhow!(msg));
        }

        // Run the preprocessor if the caller passed one. Preprocessor is
        // `Fn` (callable multiple times) and `Send + Sync`; treat it as
        // such.
        if let Some(pre) = preprocessor {
            pre(&mut params, workspace.as_deref());
        }

        // Invoke the caller's `exec_fn`. The `exec_fn` closure is the
        // adapter-supplied wrapper around `tool.execute_with_context(...)`;
        // running it here yields the tool's JSON output (or an error).
        match exec_fn(params).await {
            Ok(value) => crate::types::HookResult::Continue(crate::types::HookOutput::Json(value)),
            Err(e) => crate::types::HookResult::Error(e),
        }
    }

    async fn wait_for_all_tasks(&self, _timeout: std::time::Duration) {
        // No-op.
    }
}

/// Mirror of root's `AsyncExecutionRouter::validate_tool_args` so the
/// noop router stays self-contained. The validator reports a structured
/// error per F32b: names the tool + the missing/invalid fields.
fn validate_tool_args(
    schema: &serde_json::Value,
    params: &serde_json::Value,
    tool_name: &str,
) -> Result<(), String> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(e) => return Err(format!("schema compile error for {tool_name}: {e}")),
    };
    let errors: Vec<_> = validator.iter_errors(params).collect();
    if errors.is_empty() {
        Ok(())
    } else {
        let joined = errors
            .iter()
            .map(|e| format!("{}", e))
            .collect::<Vec<_>>()
            .join("; ");
        Err(format!("{tool_name} args failed schema: {joined}"))
    }
}
