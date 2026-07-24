//! `DaemonTransport` impl for `DaemonClient`.
//!
//! Phase 8b lifts the framework's async-task transport (the
//! `DaemonIpcTransport`) into `peko_extension_host`. The host-side
//! transport consumes an `Arc<dyn DaemonTransport>` — a value-returning
//! projection over the root `ipc::DaemonClient` that hides the raw
//! `RequestPacket` / `ResponsePacket` / `PacketStream` wire shape from
//! the host crate.
//!
//! The trait declaration lives at
//! `peko_extension_host::transport::DaemonTransport` (see
//! `crates/extension-host/src/transport.rs`); the impl is here because
//! `DaemonClient` is a root-only type.

use futures::StreamExt;
use peko_extension_host::async_exec::executor::{AsyncTaskId, AsyncTaskReceipt};
use peko_extension_host::transport::DaemonTransport;
use std::path::PathBuf;

use crate::ipc::client::DaemonClient;

/// Stream-based `DaemonClient` methods return `anyhow::Result<PacketStream>`.
/// `PacketStream` is `impl Stream<Item = ResponsePacket> + Send + 'static`
/// — for the host projection we walk the stream for the terminal
/// outcome (`AsyncReceipt { receipt, .. }` for spawn, `Done { success, .. }`
/// for cancel, or `Error { message, .. }`).
#[async_trait::async_trait]
impl DaemonTransport for DaemonClient {
    async fn is_reachable(&self) -> bool {
        self.is_running().await
    }

    async fn spawn_async_task(
        &self,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
    ) -> anyhow::Result<AsyncTaskReceipt> {
        let mut stream = self
            .spawn_async_task(tool_name, params, session_key, workspace)
            .await?;
        while let Some(packet) = stream.next().await {
            use crate::ipc::packet::ResponsePacket;
            match packet {
                ResponsePacket::AsyncReceipt { receipt, .. } => return Ok(receipt),
                ResponsePacket::Error { message, .. } => anyhow::bail!("{message}"),
                other => {
                    tracing::debug!(
                        "DaemonTransport::spawn_async_task: ignoring interim packet {other:?}"
                    );
                }
            }
        }
        anyhow::bail!("Daemon closed the response stream before sending an AsyncReceipt")
    }

    async fn cancel_async_task(&self, task_id: &AsyncTaskId) -> anyhow::Result<bool> {
        let mut stream = self.cancel_async_task(task_id.as_str()).await?;
        while let Some(packet) = stream.next().await {
            use crate::ipc::packet::ResponsePacket;
            match packet {
                ResponsePacket::Done { success, .. } => return Ok(success),
                ResponsePacket::Error { message, .. } => anyhow::bail!("{message}"),
                other => {
                    tracing::debug!(
                        "DaemonTransport::cancel_async_task: ignoring interim packet {other:?}"
                    );
                }
            }
        }
        anyhow::bail!("Daemon closed the response stream before sending a Done marker")
    }
}
