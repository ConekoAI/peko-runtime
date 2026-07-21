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
use futures::stream::Stream;
use std::path::PathBuf;
use std::pin::Pin;

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
