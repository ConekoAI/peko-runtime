//! Root-side shim for the historical `framework::transport::create_transport()`
//! factory. Phase 8b deletes the original factory (which lived in root)
//! and re-implements it here as a thin probe + delegation that hands off
//! to the host crate's `create_transport_with(client)` factory.
//!
//! Behaviour:
//! 1. Probe the daemon via `DaemonClient::connect()`. Fail fast if
//!    unreachable.
//! 2. Wrap the client as `Arc<DaemonClient>` (the host
//!    `DaemonTransport` impl is in `ipc::daemon_transport_impl`).
//! 3. Delegate to `peko_extension_host::create_transport_with(client)`.

use std::sync::Arc;

use peko_extension_host::transport::async_transport::{create_transport_with, AsyncTaskTransport};
use peko_extension_host::transport::DaemonTransport;

use crate::ipc::client::DaemonClient;

/// Probe the daemon and build an IPC `AsyncTaskTransport`.
///
/// Returns `Err` with a clear message when the daemon is unreachable —
/// the CLI fails fast instead of falling back to in-process execution
/// (which would be dropped on CLI exit, the F1 "phantom success" bug).
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
