//! IPC error helpers.

use crate::ipc::packet::ResponsePacket;

/// Create an `anyhow::Error` for an unexpected IPC response without leaking
/// payload bytes via debug formatting.
pub fn unexpected_response(response: &ResponsePacket) -> anyhow::Error {
    anyhow::anyhow!("Unexpected response: {}", response.variant_name())
}
