//! Daemon-probing async-task-transport factory (Phase 8c.1.D.6).
//!
//! Phase 8b.2 relocated the historical `framework::transport::create_transport()`
//! factory into a root-side shim under `extensions/framework/transport/`.
//! Phase 8c.1.D.6 deletes that shim and re-homes it next to `DaemonClient`
//! at `crate::ipc::create_transport::create_transport` — its logical
//! neighbour (it probes the daemon via `DaemonClient::connect`).
//!
//! Behaviour:
//! 1. Probe the daemon via `DaemonClient::connect()`. Fail fast if unreachable.
//! 2. Wrap the client as `Arc<DaemonClient>` (the host `DaemonTransport`
//!    impl is in `ipc::daemon_transport_impl`).
//! 3. Delegate to `peko_extension_host::create_transport_with(client)`.
//!
//! The factory returns `Err` with a clear message when the daemon is
//! unreachable — the CLI fails fast instead of falling back to in-process
//! execution (which would be dropped on CLI exit, the F1 "phantom success"
//! bug).

use std::sync::Arc;

use peko_extension_host::transport::async_transport::{create_transport_with, AsyncTaskTransport};
use peko_extension_host::transport::DaemonTransport;

use crate::ipc::client::DaemonClient;

/// Probe the daemon and build an IPC `AsyncTaskTransport`.
///
/// # Errors
///
/// Returns an error if the daemon cannot be reached or its client
/// cannot be wrapped.
pub async fn create_transport() -> anyhow::Result<Arc<dyn AsyncTaskTransport>> {
    let client = DaemonClient::connect().await?;
    let client: Arc<dyn DaemonTransport> = Arc::new(client);
    Ok(create_transport_with(client))
}
