//! Cross-boundary transport trait-port surface.
//!
//! ## Module layout
//!
//! - [`ToolExecConfig`] / [`PreprocessorFn`] / [`ExecFn`] (this file):
//!   the trait-port surface used by [`AsyncExecutionRouter::execute_from_hook`].
//! - [`AsyncExecutionRouter`] (this file): the trait-port implemented
//!   in root by the concrete `AsyncExecutionRouter` struct that now
//!   lives in the submodule.
//! - [`async_router`]: concrete router (5-minute timeout funnel). Implements
//!   [`AsyncExecutionRouter`] for `Self`.
//! - [`async_transport`]: the in-process transport (`LocalAsyncTransport`)
//!   + the `BoxedExecutionFn` helper type and the `create_local_transport*`
//!   factories.
//!
//! 2026-09-27 consolidation (ADR-063 (dead IPC path)): the
//! `DaemonTransport` IPC projection is deleted with the rest of the dead
//! IPC async-spawn path — the CLI never executes tools (ADR-021), so no
//! component hands background work to the daemon over IPC.

use async_trait::async_trait;
use serde_json::Value;

// Phase 8b lift: concrete implementations live alongside the
// trait contracts above (which are the parent module's surface).
pub mod async_router;
pub mod async_transport;

/// Minimal tool-execution config visible to the [`AsyncExecutionRouter`]
/// trait port.
///
/// Holds the same fields as root's `services::ToolExecutionConfig`
/// (`full_schema` + `reserved_params`), but uses the
/// `peko_extension_api::ReservedParamsConfig` type so the host crate
/// doesn't need to depend on root's `framework::services/` (lifted
/// in Phase 8c). Root callers construct `ToolExecConfig` directly at
/// the trait boundary.
#[derive(Debug, Clone, Default)]
pub struct ToolExecConfig {
    /// Full JSON Schema for the tool's parameters (with reserved params).
    pub full_schema: Value,
    /// Reserved-params configuration.
    pub reserved_params: peko_extension_api::reserved_params::ReservedParamsConfig,
}

impl ToolExecConfig {
    /// Create a new execution config from a reserved-params config and
    /// full schema.
    #[must_use]
    pub fn new(
        reserved_params: peko_extension_api::reserved_params::ReservedParamsConfig,
        full_schema: Value,
    ) -> Self {
        Self {
            full_schema,
            reserved_params,
        }
    }

    /// Create a config with empty reserved params and the given schema.
    /// Mirrors root's `services::ToolExecutionConfig::with_schema` so
    /// root call sites can use a single constructor regardless of
    /// which side of the trait boundary they sit on.
    #[must_use]
    pub fn with_schema(full_schema: Value) -> Self {
        Self {
            reserved_params: peko_extension_api::reserved_params::ReservedParamsConfig::new(),
            full_schema,
        }
    }
}

/// Preprocessor closure type for [`AsyncExecutionRouter::execute_from_hook`].
///
/// `Fn` (not `FnOnce`) because the impl may invoke it zero or more times.
pub type PreprocessorFn = Box<dyn Fn(&mut Value, Option<&str>) + Send + Sync>;

/// ExecFn callback type for [`AsyncExecutionRouter::execute_from_hook`].
///
/// `FnOnce` (consumed once during dispatch).
pub type ExecFn = Box<dyn FnOnce(Value) -> BoxFuture<'static, anyhow::Result<Value>> + Send>;

use futures::future::BoxFuture;

/// Async execution router port trait.
///
/// This is the host-crate-side projection of root's
/// `framework::transport::AsyncExecutionRouter`. Root implements
/// this trait and the host stores an `Arc<dyn AsyncExecutionRouter>`
/// in `ExtensionServices` so the field is self-contained without a
/// host → services/transport dep.
///
/// The trait ports only the methods called via
/// `ExtensionServices::async_router()`: `execute_from_hook` (root
/// adapter callers) and `wait_for_all_tasks` (host-side
/// `wait_for_async_tasks`).
#[async_trait]
pub trait AsyncExecutionRouter: Send + Sync {
    /// Route a tool call through the async dispatch funnel.
    ///
    /// Equivalent to root's `AsyncExecutionRouter::execute_from_hook`
    /// (the F37 hook funnel). See the root-side doc comment for the
    /// semantics. The boxed `PreprocessorFn` and `ExecFn` types make
    /// the trait dyn-compatible — callers wrap their closures with
    /// `Box::new(...)`.
    async fn execute_from_hook(
        &self,
        ctx: &crate::extensions::framework::core::context::HookContext,
        tool_name: &str,
        exec_config: &ToolExecConfig,
        preprocessor: Option<PreprocessorFn>,
        exec_fn: ExecFn,
    ) -> crate::extensions::framework::types::HookResult;

    /// Wait for all async tasks to complete.
    ///
    /// For the local transport, waits until tasks reach terminal state
    /// or `timeout` elapses.
    async fn wait_for_all_tasks(&self, timeout: std::time::Duration);
}
