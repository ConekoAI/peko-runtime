//! Tunnel Message Protocol
//!
//! Defines the binary message format sent over the WebSocket tunnel.
//! Messages are serialized as JSON for simplicity and debuggability.

use serde::{Deserialize, Serialize};

/// Messages exchanged over the runtime↔PekoHub WebSocket tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TunnelMessage {
    // --- Control ---
    /// Runtime authentication hello
    #[serde(rename = "runtime_hello")]
    RuntimeHello {
        /// did:key format — self-certifying identity
        runtime_id: String,
        /// Random nonce
        nonce: String,
        /// Ed25519 signature of nonce, verifiable using key derived from runtime_id
        signature: String,
    },

    /// Tunnel ready acknowledgement from PekoHub
    #[serde(rename = "tunnel_ready")]
    TunnelReady {
        /// Heartbeat interval in seconds
        heartbeat_interval_secs: u32,
    },

    /// Heartbeat ping
    #[serde(rename = "heartbeat")]
    Heartbeat { seq: u64 },

    /// Heartbeat acknowledgement
    #[serde(rename = "heartbeat_ack")]
    HeartbeatAck { seq: u64 },

    /// Graceful disconnect
    #[serde(rename = "disconnect")]
    Disconnect { reason: String },

    // --- Request routing: PekoHub → runtime ---
    /// Proxied request from a web user
    #[serde(rename = "proxied_request")]
    ProxiedRequest {
        /// Globally unique request ID
        request_id: String,
        /// Target agent name
        agent: String,
        /// Serialized IPC RequestPacket
        payload: Vec<u8>,
    },

    // --- Response routing: runtime → PekoHub ---
    /// Proxied response back to PekoHub
    #[serde(rename = "proxied_response")]
    ProxiedResponse {
        /// Request ID matching the ProxiedRequest
        request_id: String,
        /// Serialized IPC ResponsePacket
        payload: Vec<u8>,
    },

    // --- Streaming ---
    /// Streaming response chunk
    #[serde(rename = "stream_chunk")]
    StreamChunk {
        request_id: String,
        seq: u32,
        payload: Vec<u8>,
    },

    /// Streaming end marker
    #[serde(rename = "stream_end")]
    StreamEnd { request_id: String },
}

impl TunnelMessage {
    /// Serialize to JSON bytes
    ///
    /// # Errors
    /// Returns error if serialization fails
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Deserialize from JSON bytes
    ///
    /// # Errors
    /// Returns error if deserialization fails
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_hello_roundtrip() {
        let msg = TunnelMessage::RuntimeHello {
            runtime_id: "did:key:z6MkTest".to_string(),
            nonce: "abc123".to_string(),
            signature: "sig456".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::RuntimeHello {
                runtime_id,
                nonce,
                signature,
            } => {
                assert_eq!(runtime_id, "did:key:z6MkTest");
                assert_eq!(nonce, "abc123");
                assert_eq!(signature, "sig456");
            }
            _ => panic!("Expected RuntimeHello"),
        }
    }

    #[test]
    fn test_tunnel_ready_roundtrip() {
        let msg = TunnelMessage::TunnelReady {
            heartbeat_interval_secs: 30,
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::TunnelReady {
                heartbeat_interval_secs,
            } => {
                assert_eq!(heartbeat_interval_secs, 30);
            }
            _ => panic!("Expected TunnelReady"),
        }
    }

    #[test]
    fn test_heartbeat_roundtrip() {
        let msg = TunnelMessage::Heartbeat { seq: 42 };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::Heartbeat { seq } => assert_eq!(seq, 42),
            _ => panic!("Expected Heartbeat"),
        }
    }

    #[test]
    fn test_proxied_request_roundtrip() {
        let msg = TunnelMessage::ProxiedRequest {
            request_id: "req-123".to_string(),
            agent: "my-agent".to_string(),
            payload: vec![1, 2, 3],
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::ProxiedRequest {
                request_id,
                agent,
                payload,
            } => {
                assert_eq!(request_id, "req-123");
                assert_eq!(agent, "my-agent");
                assert_eq!(payload, vec![1, 2, 3]);
            }
            _ => panic!("Expected ProxiedRequest"),
        }
    }

    #[test]
    fn test_stream_chunk_roundtrip() {
        let msg = TunnelMessage::StreamChunk {
            request_id: "req-123".to_string(),
            seq: 7,
            payload: vec![0xAB, 0xCD],
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::StreamChunk {
                request_id,
                seq,
                payload,
            } => {
                assert_eq!(request_id, "req-123");
                assert_eq!(seq, 7);
                assert_eq!(payload, vec![0xAB, 0xCD]);
            }
            _ => panic!("Expected StreamChunk"),
        }
    }
}
