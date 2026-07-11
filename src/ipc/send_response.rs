//! Free helper for emitting a `ResponsePacket` over any `ResponseSink`.
//!
//! Extracted from `IpcServer::send_sink` so the per-domain request handlers
//! in `crate::ipc::handlers` can emit responses without depending on the
//! server impl. The legacy monolithic match in `ipc::server` also uses
//! this helper.

use crate::ipc::packet::ResponsePacket;
use crate::ipc::response_sink::ResponseSink;
use tracing::trace;

/// Serialize `packet` and deliver it via `sink`.
pub(crate) async fn send_response(
    sink: &dyn ResponseSink,
    packet: ResponsePacket,
) -> anyhow::Result<()> {
    let bytes = packet.to_bytes()?;
    trace!("Sending response: {:?} ({} bytes)", packet, bytes.len());
    sink.send_bytes(&bytes).await?;
    Ok(())
}