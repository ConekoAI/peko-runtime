//! Cross-boundary transport trait + value-returning projection.
//!
//! Phase 8a shipped a narrow [`DaemonTransport`] trait as a placeholder.
//! Phase 8b expands it into the real value-returning surface that the
//! host's async-task transport consumes.
//!
//! ## Module layout
//!
//! This file is the parent for the [`transport`] module:
//!
//! - [`DaemonTransport`] (this file): the value-returning IPC projection
//!   trait implemented in root for `Arc<DaemonClient>`.
//! - [`ToolExecConfig`] / [`PreprocessorFn`] / [`ExecFn`] (this file):
//!   the trait-port surface used by [`AsyncExecutionRouter::execute_from_hook`].
//! - [`AsyncExecutionRouter`] (this file): the trait-port implemented
//!   in root by the concrete `AsyncExecutionRouter` struct that now
//!   lives in the submodule.
//! - [`async_router`]: concrete router (5-minute timeout funnel). Implements
//!   [`AsyncExecutionRouter`] for `Self`. Lifted from
//!   `src/extensions/framework/transport/async_router.rs`.
//! - [`async_transport`]: transport implementations (`LocalAsyncTransport`,
//!   `DaemonIpcTransport`, `UnavailableAsyncTransport`) + the
//!   `BoxedExecutionFn` helper type and `create_transport_with` /
//!   `create_local_transport` factories. Lifted from
//!   `src/extensions/framework/transport/async_transport.rs`.
//!
//! ## Why value-returning
//!
//! The host's `DaemonIpcTransport` (CLI side) consumes a stream of
//! `ipc::ResponsePacket` values from `ipc::DaemonClient`, walks it
//! for `ResponsePacket::AsyncReceipt { receipt, .. }` / `Done { success, .. }`
//! / `Error { message, .. }`, and projects the final outcome. The
//! trait sits one level above the host transport and exposes just the
//! projected values (`AsyncTaskReceipt`, `bool`), so the host transport
//! is the sole consumer of the raw stream.
//!
//! This is the minimal-surface trait port for the IPC dep: the host
//! crate never imports `crate::ipc::*`; root implements `DaemonTransport`
//! for `Arc<DaemonClient>` (see
//! `src/ipc/daemon_transport_impl.rs`) and the host consumes
//! `Arc<dyn DaemonTransport>`.
//!
//! ## Test doubles
//!
//! The trait is dyn-compatible (Send + Sync + 'static) so tests can
//! substitute a `MockDaemonTransport` that returns canned receipts /
//! `false` cancel results without needing a real daemon.

use std::path::PathBuf;

use crate::async_exec::executor::AsyncTaskId;
use crate::async_exec::executor::AsyncTaskReceipt;
use async_trait::async_trait;
use serde_json::Value;

// Phase 8b lift: concrete implementations live alongside the
// trait contracts above (which are the parent module's surface).
pub mod async_router;
pub mod async_transport;

/// Cross-boundary abstraction over `ipc::DaemonClient`.
///
/// Implemented by root's `DaemonClient` (production) and by test
/// doubles. The trait projects the root `ipc::ResponsePacket` stream
/// down to value-returning methods so the host crate does not need
/// to depend on root IPC types.
#[async_trait]
pub trait DaemonTransport: Send + Sync + 'static {
    /// Reachability probe — `true` if the daemon responds to a ping
    /// within the configured timeout.
    async fn is_reachable(&self) -> bool;

    /// Spawn an async task on the daemon. Returns the receipt the
    /// daemon emitted for the new task id; the caller stores it on
    /// the local `AsyncTaskEntry` and can poll status / cancel via
    /// the other trait methods.
    ///
    /// # Errors
    ///
    /// Returns an error if the daemon is unreachable, refuses the
    /// spawn, or the response stream closes before an
    /// `AsyncReceipt` is observed.
    async fn spawn_async_task(
        &self,
        tool_name: String,
        params: Value,
        session_key: String,
        workspace: PathBuf,
    ) -> anyhow::Result<AsyncTaskReceipt>;

    /// Cancel an async task by id. Returns `true` if the daemon
    /// confirmed the cancel, `false` if the task was already gone.
    async fn cancel_async_task(&self, task_id: &AsyncTaskId) -> anyhow::Result<bool>;
}

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
            reserved_params,
            full_schema,
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
/// `framework::transport::AsyncExecutionRouter`. The concrete struct
/// lifts into the host in Phase 8b; for Phase 8a, root implements
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
        ctx: &crate::core::context::HookContext,
        tool_name: &str,
        exec_config: &ToolExecConfig,
        preprocessor: Option<PreprocessorFn>,
        exec_fn: ExecFn,
    ) -> crate::types::HookResult;

    /// Wait for all async tasks to complete.
    ///
    /// For the local transport, waits until tasks reach terminal state
    /// or `timeout` elapses. For the HTTP transport, returns
    /// immediately because tasks live on the daemon and survive CLI
    /// exit.
    async fn wait_for_all_tasks(&self, timeout: std::time::Duration);
}
