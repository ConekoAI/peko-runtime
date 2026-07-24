//! Cross-boundary transport trait + response projection.
//!
//! Phase 8 commit 1 ships the narrow [`DaemonTransport`] trait that
//! the framework host's IPC clients implement. The trait projects
//! the root `ipc::ResponsePacket` stream down to a tiny
//! [`DaemonResponse`] enum (just success / failure / error markers)
//! so the host crate does not need to depend on root IPC types.
//!
//! Phase 8 commit 2 will move the host's `AsyncTaskTransport` and
//! `DaemonIpcTransport` into this crate. At that point the host
//! transport consumes `DaemonTransport`, and a fuller `DaemonResponse`
//! variant set (with `AsyncReceipt` payload) will replace the
//! current minimal one.

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::Stream;
use serde_json::Value;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use crate::core::context::HookContext;
use crate::types::HookResult;

/// Subset of the daemon IPC response stream that the host cares
/// about. The full `ipc::ResponsePacket` enum lives in the root
/// (Phase 11 will move it to `peko-protocol`); the host uses this
/// projection so it does not depend on root IPC types.
///
/// Phase 8 commit 1 only models the terminal markers (`Done` and
/// `Error`); an `AsyncReceipt` variant is added in commit 2 when
/// the host transport moves into the crate.
#[derive(Debug, Clone)]
pub enum DaemonResponse {
    /// Final success/failure marker for a request.
    Done {
        success: bool,
        error: Option<String>,
        request_id: u64,
    },
    /// Error response.
    Error { message: String, request_id: u64 },
}

/// Boxed stream of daemon responses for a single in-flight request.
pub type DaemonResponseStream = Pin<Box<dyn Stream<Item = DaemonResponse> + Send + 'static>>;

/// Cross-boundary abstraction over `ipc::DaemonClient`.
///
/// Implemented by root's `DaemonClient` (production) and by test
/// doubles. Phase 8 commit 1 ships the trait + projection; commit 2
/// will move `AsyncTaskTransport` and `DaemonIpcTransport` into the
/// host crate and have `DaemonIpcTransport` consume this trait.
#[async_trait]
pub trait DaemonTransport: Send + Sync + 'static {
    /// Reachability probe — `true` if the daemon responds.
    async fn is_reachable(&self) -> bool;

    /// Spawn an async task on the daemon. Returns a stream of
    /// [`DaemonResponse`]s; the caller interprets the terminal
    /// `Done { success, .. }` or `Error { message, .. }` packet.
    async fn spawn_async_task(
        &self,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
    ) -> anyhow::Result<DaemonResponseStream>;

    /// Cancel an async task by id. Returns a stream; the caller
    /// interprets the terminal packet.
    async fn cancel_async_task(&self, task_id: &str) -> anyhow::Result<DaemonResponseStream>;
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
        ctx: &HookContext,
        tool_name: &str,
        exec_config: &ToolExecConfig,
        preprocessor: Option<PreprocessorFn>,
        exec_fn: ExecFn,
    ) -> HookResult;

    /// Wait for all async tasks to complete.
    ///
    /// For the local transport, waits until tasks reach terminal state
    /// or `timeout` elapses. For the HTTP transport, returns
    /// immediately because tasks live on the daemon and survive CLI
    /// exit.
    async fn wait_for_all_tasks(&self, timeout: Duration);
}
