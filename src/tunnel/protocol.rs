//! Tunnel Message Protocol
//!
//! Defines the binary message format sent over the WebSocket tunnel.
//! Messages are serialized as JSON for simplicity and debuggability.

use serde::{Deserialize, Serialize};

/// Status of an instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    Online,
    Offline,
    Busy,
    Error,
}

/// Exposure level of an instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceExposure {
    Private,
    Public,
    Unexposed,
}

/// Type of an instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceType {
    Agent,
    Team,
}

/// Payload for `instance_announce` messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceAnnouncePayload {
    pub id: String,
    #[serde(rename = "type")]
    pub instance_type: InstanceType,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_display_name: Option<String>,
    pub status: InstanceStatus,
    pub exposure: InstanceExposure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_users: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Payload for `instance_heartbeat` messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceHeartbeatPayload {
    pub id: String,
    pub status: InstanceStatus,
    pub timestamp: String,
}

/// Payload for `instance_deregister` messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceDeregisterPayload {
    pub id: String,
}

/// Payload for `exposure_update` messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposureUpdatePayload {
    pub instance_id: String,
    pub exposure: InstanceExposure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_user_ids: Option<Vec<String>>,
}

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

    // --- Instance lifecycle ---
    /// Instance announcement
    #[serde(rename = "instance_announce")]
    InstanceAnnounce { payload: InstanceAnnouncePayload },

    /// Instance heartbeat
    #[serde(rename = "instance_heartbeat")]
    InstanceHeartbeat { payload: InstanceHeartbeatPayload },

    /// Instance deregistration
    #[serde(rename = "instance_deregister")]
    InstanceDeregister { payload: InstanceDeregisterPayload },

    /// Exposure update
    #[serde(rename = "exposure_update")]
    ExposureUpdate { payload: ExposureUpdatePayload },
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

    #[test]
    fn test_instance_announce_roundtrip() {
        let mut metadata = serde_json::Map::new();
        metadata.insert("key".to_string(), serde_json::Value::String("value".to_string()));

        let msg = TunnelMessage::InstanceAnnounce {
            payload: InstanceAnnouncePayload {
                id: "inst-1".to_string(),
                instance_type: InstanceType::Agent,
                name: "Test Agent".to_string(),
                bundle_ref: Some("bundle-abc".to_string()),
                runtime_display_name: Some("My Runtime".to_string()),
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Public,
                allowed_users: Some(vec!["user1".to_string(), "user2".to_string()]),
                capabilities: Some(vec!["chat".to_string()]),
                metadata: Some(metadata),
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceAnnounce { payload } => {
                assert_eq!(payload.id, "inst-1");
                assert_eq!(payload.name, "Test Agent");
                assert_eq!(payload.bundle_ref, Some("bundle-abc".to_string()));
                assert_eq!(payload.runtime_display_name, Some("My Runtime".to_string()));
                assert_eq!(payload.allowed_users, Some(vec!["user1".to_string(), "user2".to_string()]));
                assert_eq!(payload.capabilities, Some(vec!["chat".to_string()]));
                assert!(payload.metadata.is_some());
            }
            _ => panic!("Expected InstanceAnnounce"),
        }
    }

    #[test]
    fn test_instance_announce_minimal_roundtrip() {
        let msg = TunnelMessage::InstanceAnnounce {
            payload: InstanceAnnouncePayload {
                id: "inst-2".to_string(),
                instance_type: InstanceType::Team,
                name: "Minimal".to_string(),
                bundle_ref: None,
                runtime_display_name: None,
                status: InstanceStatus::Offline,
                exposure: InstanceExposure::Private,
                allowed_users: None,
                capabilities: None,
                metadata: None,
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceAnnounce { payload } => {
                assert_eq!(payload.id, "inst-2");
                assert_eq!(payload.bundle_ref, None);
                assert_eq!(payload.runtime_display_name, None);
                assert_eq!(payload.allowed_users, None);
                assert_eq!(payload.capabilities, None);
                assert_eq!(payload.metadata, None);
            }
            _ => panic!("Expected InstanceAnnounce"),
        }
    }

    #[test]
    fn test_instance_heartbeat_roundtrip() {
        let msg = TunnelMessage::InstanceHeartbeat {
            payload: InstanceHeartbeatPayload {
                id: "inst-1".to_string(),
                status: InstanceStatus::Busy,
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceHeartbeat { payload } => {
                assert_eq!(payload.id, "inst-1");
                assert_eq!(payload.timestamp, "2024-01-01T00:00:00Z");
            }
            _ => panic!("Expected InstanceHeartbeat"),
        }
    }

    #[test]
    fn test_instance_deregister_roundtrip() {
        let msg = TunnelMessage::InstanceDeregister {
            payload: InstanceDeregisterPayload {
                id: "inst-1".to_string(),
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceDeregister { payload } => {
                assert_eq!(payload.id, "inst-1");
            }
            _ => panic!("Expected InstanceDeregister"),
        }
    }

    #[test]
    fn test_exposure_update_roundtrip() {
        let msg = TunnelMessage::ExposureUpdate {
            payload: ExposureUpdatePayload {
                instance_id: "inst-1".to_string(),
                exposure: InstanceExposure::Public,
                allowed_user_ids: Some(vec!["user1".to_string()]),
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::ExposureUpdate { payload } => {
                assert_eq!(payload.instance_id, "inst-1");
                assert_eq!(payload.allowed_user_ids, Some(vec!["user1".to_string()]));
            }
            _ => panic!("Expected ExposureUpdate"),
        }
    }

    #[test]
    fn test_exposure_update_minimal_roundtrip() {
        let msg = TunnelMessage::ExposureUpdate {
            payload: ExposureUpdatePayload {
                instance_id: "inst-2".to_string(),
                exposure: InstanceExposure::Unexposed,
                allowed_user_ids: None,
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::ExposureUpdate { payload } => {
                assert_eq!(payload.instance_id, "inst-2");
                assert_eq!(payload.allowed_user_ids, None);
            }
            _ => panic!("Expected ExposureUpdate"),
        }
    }
}
