//! Tunnel Message Protocol
//!
//! Defines the binary message format sent over the WebSocket tunnel.
//! Messages are serialized as JSON for simplicity and debuggability.
//!
//! NOTE: All wire-format field names use camelCase to match the TypeScript
//! peer implementation in PekoHub.

use serde::{Deserialize, Serialize};

/// Status of an instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    Online,
    Offline,
    Busy,
    Error,
}

/// Exposure level of an instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceExposure {
    Private,
    Public,
    Unexposed,
}

/// Type of an instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceType {
    Agent,
    Team,
}

/// Payload for `instance_announce` messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceAnnouncePayload {
    pub id: String,
    #[serde(rename = "type")]
    pub instance_type: InstanceType,
    pub name: String,
    /// Stable per-agent identifier (DID) — issue #28.
    ///
    /// Populated from `AgentConfig.agent_did` when the agent has been
    /// started at least once (the runtime backfills the DID on
    /// `Agent::new`). Absent for legacy agents predating #28; PekoHub
    /// treats those by falling back to the local `name` for one release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
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
#[serde(rename_all = "camelCase")]
pub struct InstanceHeartbeatPayload {
    pub id: String,
    pub status: InstanceStatus,
    pub timestamp: String,
}

/// Payload for `instance_deregister` messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceDeregisterPayload {
    pub id: String,
}

/// Payload for `exposure_update` messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExposureUpdatePayload {
    pub instance_id: String,
    pub exposure: InstanceExposure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_user_ids: Option<Vec<String>>,
}

/// Payload for `status_update` messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusUpdatePayload {
    pub instance_id: String,
    pub status: InstanceStatus,
}

/// Messages exchanged over the runtime↔PekoHub WebSocket tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TunnelMessage {
    // --- Control ---
    /// Runtime authentication hello
    #[serde(rename = "runtime_hello", rename_all = "camelCase")]
    RuntimeHello {
        /// did:key format — self-certifying identity
        runtime_id: String,
        /// Random nonce
        nonce: String,
        /// Ed25519 signature of nonce, verifiable using key derived from runtime_id
        signature: String,
    },

    /// Server-issued nonce challenge after `RuntimeHello` is accepted
    /// (pekohub issue #1). Runtime must sign and return via
    /// `TunnelChallengeAck`. Replay protection is the server's job
    /// (in-memory nonce store).
    #[serde(rename = "tunnel_challenge", rename_all = "camelCase")]
    TunnelChallenge {
        /// Server-generated base64url nonce.
        nonce: String,
    },

    /// Signed response to a `TunnelChallenge`.
    #[serde(rename = "tunnel_challenge_ack", rename_all = "camelCase")]
    TunnelChallengeAck {
        /// The nonce from the matching `TunnelChallenge` (base64url).
        nonce: String,
        /// Ed25519 signature of `nonce` using the runtime's private key.
        signature: String,
    },

    /// Tunnel ready acknowledgement from PekoHub
    #[serde(rename = "tunnel_ready", rename_all = "camelCase")]
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
    #[serde(rename = "proxied_request", rename_all = "camelCase")]
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
    #[serde(rename = "proxied_response", rename_all = "camelCase")]
    ProxiedResponse {
        /// Request ID matching the ProxiedRequest
        request_id: String,
        /// Serialized IPC ResponsePacket
        payload: Vec<u8>,
    },

    // --- Streaming ---
    /// Streaming response chunk
    #[serde(rename = "stream_chunk", rename_all = "camelCase")]
    StreamChunk {
        request_id: String,
        seq: u32,
        payload: Vec<u8>,
    },

    /// Streaming end marker
    #[serde(rename = "stream_end", rename_all = "camelCase")]
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

    /// Status update
    #[serde(rename = "status_update")]
    StatusUpdate { payload: StatusUpdatePayload },

    // --- Cross-runtime agent-to-agent (issue #29) ---
    /// Agent-to-agent request from the **caller** runtime to the
    /// **target** runtime, proxied through PekoHub. Issue #29 (Slice A —
    /// wire shape).
    ///
    /// The caller runtime resolves the target via PekoHub's directory
    /// API (pekohub#14: `GET /v1/agents/by-did/:did` or
    /// `GET /v1/agents/by-handle/:owner/:agent_name`), signs this
    /// envelope with its `PekoHubCredential` private key, and sends
    /// it to PekoHub which forwards to the target runtime over the
    /// target's existing tunnel. The target verifies the caller's
    /// `caller_runtime_id` against the hub's allowlist (defense in
    /// depth) before attributing the receiving agent's session to
    /// `Principal::Agent(caller_agent_did)` and dispatching.
    ///
    /// Slice A only defines and round-trips the wire shape. Slice B
    /// adds the outbound signer (`PekoHubCredential::sign(...)` against
    /// the canonical pre-image `request_id || caller_runtime_id ||
    /// caller_agent_did || target_agent_did || message || session_id?`).
    /// Slice C adds the inbound verifier + dispatcher route.
    #[serde(rename = "agent_to_agent_request", rename_all = "camelCase")]
    AgentToAgentRequest {
        /// Globally unique request ID. Used to correlate the matching
        /// `AgentToAgentResponse` and to scope the canonical
        /// signature pre-image (replay protection: PekoHub MAY
        /// reject duplicate IDs within a sliding window).
        request_id: String,
        /// The caller runtime's `did:key` form (the `runtime_id` it
        /// presented in `RuntimeHello`). The target runtime verifies
        /// the `signature` field against the public key derived from
        /// this DID and rejects the message if the caller is not on
        /// the hub's allowlist.
        caller_runtime_id: String,
        /// The caller agent's stable DID (issue #28 form:
        /// `did:peko:agent:<keyhash>`). Projected to
        /// `Principal::Agent(caller_agent_did)` on the target side
        /// for session attribution, permission grant lookup, and the
        /// `AuditEvent.caller` field (issue #26).
        caller_agent_did: String,
        /// The **target** agent's stable DID. The target runtime
        /// resolves this against its local agent table
        /// (`AgentConfig.agent_did`) to find the agent name to
        /// dispatch on. A missing target_agent_did on the receiving
        /// side is a 404 — the hub-side directory should have caught
        /// this, so it most often indicates a stale resolution
        /// cached on the caller.
        target_agent_did: String,
        /// Optional session ID to resume on the target side. When
        /// absent, the target runtime allocates a fresh session
        /// keyed under `peer: agent:<caller_agent_did>`.
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// The message body to deliver to the target agent.
        message: String,
        /// Optional team name to scope the target agent's session
        /// within. When absent, the target runtime uses its default
        /// team. Mirrors the local `a2a_send` semantics.
        #[serde(skip_serializing_if = "Option::is_none")]
        team: Option<String>,
        /// Ed25519 signature, base64url-encoded, over the canonical
        /// pre-image (see Slice B comment above). The target derives
        /// the verifying public key from `caller_runtime_id`
        /// (self-certifying `did:key`).
        ///
        /// Left as `String` rather than `Vec<u8>` so the wire form
        /// matches the existing `RuntimeHello.signature` /
        /// `TunnelChallengeAck.signature` shape — those use
        /// base64url-in-string and the hub-side TypeScript code
        /// expects strings.
        signature: String,
    },

    /// Agent-to-agent response from the **target** runtime back to the
    /// **caller**, also proxied through PekoHub. Issue #29 (Slice A —
    /// wire shape).
    ///
    /// The `payload` is the serialized form of an IPC `ResponsePacket`
    /// (same as `ProxiedResponse.payload`) so the caller-side decoder
    /// can be the same code path for both user-originated and
    /// agent-originated proxied responses. Slice C is what actually
    /// emits this; Slice A only pins the shape.
    #[serde(rename = "agent_to_agent_response", rename_all = "camelCase")]
    AgentToAgentResponse {
        /// Matches the `request_id` of the originating
        /// `AgentToAgentRequest`. PekoHub uses this to route the
        /// response back to the caller's tunnel.
        request_id: String,
        /// Serialized IPC `ResponsePacket` (same encoding as
        /// `ProxiedResponse.payload`). Slice C decides whether the
        /// target's `AuditEvent` is emitted on the target side, on
        /// the caller side, or both — the payload itself is
        /// indifferent.
        payload: Vec<u8>,
    },
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
            runtime_id: "did:key:z6Mk".to_string(),
            nonce: "abc123".to_string(),
            signature: "sig".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        // Verify camelCase on the wire
        assert!(
            json.contains("\"runtimeId\""),
            "Expected camelCase runtimeId, got: {}",
            json
        );
        assert!(
            json.contains("\"runtime_hello\""),
            "Expected snake_case tag"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::RuntimeHello {
                runtime_id,
                nonce,
                signature,
            } => {
                assert_eq!(runtime_id, "did:key:z6Mk");
                assert_eq!(nonce, "abc123");
                assert_eq!(signature, "sig");
            }
            _ => panic!("Expected RuntimeHello"),
        }
    }

    #[test]
    fn test_tunnel_challenge_roundtrip() {
        let msg = TunnelMessage::TunnelChallenge {
            nonce: "cmFuZG9tLW5vbmNlLTMyYg".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            json.contains("\"tunnel_challenge\""),
            "Expected tunnel_challenge tag, got: {}",
            json
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::TunnelChallenge { nonce } => {
                assert_eq!(nonce, "cmFuZG9tLW5vbmNlLTMyYg");
            }
            _ => panic!("Expected TunnelChallenge"),
        }
    }

    #[test]
    fn test_tunnel_challenge_ack_roundtrip() {
        let msg = TunnelMessage::TunnelChallengeAck {
            nonce: "nonce-xyz".to_string(),
            signature: "sig-abc".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            json.contains("\"tunnel_challenge_ack\""),
            "Expected tunnel_challenge_ack tag, got: {}",
            json
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::TunnelChallengeAck { nonce, signature } => {
                assert_eq!(nonce, "nonce-xyz");
                assert_eq!(signature, "sig-abc");
            }
            _ => panic!("Expected TunnelChallengeAck"),
        }
    }

    #[test]
    fn test_tunnel_ready_roundtrip() {
        let msg = TunnelMessage::TunnelReady {
            heartbeat_interval_secs: 30,
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            json.contains("\"heartbeatIntervalSecs\""),
            "Expected camelCase, got: {}",
            json
        );

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
    fn test_instance_announce_roundtrip() {
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "key".to_string(),
            serde_json::Value::String("value".to_string()),
        );
        let msg = TunnelMessage::InstanceAnnounce {
            payload: InstanceAnnouncePayload {
                id: "inst-1".to_string(),
                instance_type: InstanceType::Agent,
                name: "test-agent".to_string(),
                agent_did: Some("did:peko:local:abc123".to_string()),
                bundle_ref: Some("ref".to_string()),
                runtime_display_name: Some("Test".to_string()),
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_users: Some(vec!["u1".to_string()]),
                capabilities: Some(vec!["c1".to_string()]),
                metadata: Some(metadata),
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            json.contains("\"runtimeDisplayName\""),
            "Expected camelCase, got: {}",
            json
        );
        assert!(
            json.contains("\"bundleRef\""),
            "Expected camelCase, got: {}",
            json
        );
        assert!(
            json.contains("\"allowedUsers\""),
            "Expected camelCase, got: {}",
            json
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceAnnounce { payload } => {
                assert_eq!(payload.id, "inst-1");
                assert_eq!(payload.runtime_display_name, Some("Test".to_string()));
            }
            _ => panic!("Expected InstanceAnnounce"),
        }
    }

    #[test]
    fn test_instance_announce_minimal_roundtrip() {
        let msg = TunnelMessage::InstanceAnnounce {
            payload: InstanceAnnouncePayload {
                id: "inst-2".to_string(),
                instance_type: InstanceType::Agent,
                name: "minimal".to_string(),
                agent_did: None,
                bundle_ref: None,
                runtime_display_name: None,
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_users: None,
                capabilities: None,
                metadata: None,
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(!json.contains("bundleRef"), "None fields should be skipped");
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceAnnounce { payload } => {
                assert_eq!(payload.bundle_ref, None);
            }
            _ => panic!("Expected InstanceAnnounce"),
        }
    }

    /// Issue #28: `InstanceAnnouncePayload.agent_did` must
    /// (a) round-trip when present, and
    /// (b) be omitted from the serialized wire form when `None`
    ///     (legacy agents, back-compat with pre-#28 PekoHub).
    #[test]
    fn test_instance_announce_agent_did_roundtrip() {
        let msg = TunnelMessage::InstanceAnnounce {
            payload: InstanceAnnouncePayload {
                id: "inst-3".to_string(),
                instance_type: InstanceType::Agent,
                name: "helper".to_string(),
                agent_did: Some("did:peko:local:abc123".to_string()),
                bundle_ref: None,
                runtime_display_name: None,
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_users: None,
                capabilities: None,
                metadata: None,
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        // The DID serializes to camelCase on the wire.
        assert!(
            json.contains("\"agentDid\":\"did:peko:local:abc123\""),
            "agent_did must serialize as `agentDid` on the wire (camelCase), got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceAnnounce { payload } => {
                assert_eq!(payload.agent_did.as_deref(), Some("did:peko:local:abc123"));
            }
            _ => panic!("Expected InstanceAnnounce"),
        }
    }

    #[test]
    fn test_instance_announce_omits_agent_did_when_none() {
        // Legacy agent (no DID yet) — the field must be omitted so
        // pre-#28 PekoHub doesn't reject the payload with "unknown
        // field" (camelCase is the wire format; PekoHub uses serde
        // with `deny_unknown_fields` disabled in practice but the
        // skip annotation keeps the contract explicit).
        let msg = TunnelMessage::InstanceAnnounce {
            payload: InstanceAnnouncePayload {
                id: "inst-4".to_string(),
                instance_type: InstanceType::Agent,
                name: "legacy-helper".to_string(),
                agent_did: None,
                bundle_ref: None,
                runtime_display_name: None,
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_users: None,
                capabilities: None,
                metadata: None,
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            !json.contains("agentDid"),
            "agent_did must be omitted from the wire when None (back-compat); got: {json}"
        );
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
    fn test_disconnect_roundtrip() {
        let msg = TunnelMessage::Disconnect {
            reason: "test".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::Disconnect { reason } => assert_eq!(reason, "test"),
            _ => panic!("Expected Disconnect"),
        }
    }

    #[test]
    fn test_proxied_request_roundtrip() {
        let msg = TunnelMessage::ProxiedRequest {
            request_id: "req-1".to_string(),
            agent: "agent-1".to_string(),
            payload: vec![1, 2, 3],
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            json.contains("\"requestId\""),
            "Expected camelCase, got: {}",
            json
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::ProxiedRequest {
                request_id,
                agent,
                payload,
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(agent, "agent-1");
                assert_eq!(payload, vec![1, 2, 3]);
            }
            _ => panic!("Expected ProxiedRequest"),
        }
    }

    #[test]
    fn test_exposure_update_roundtrip() {
        let msg = TunnelMessage::ExposureUpdate {
            payload: ExposureUpdatePayload {
                instance_id: "inst-1".to_string(),
                exposure: InstanceExposure::Public,
                allowed_user_ids: Some(vec!["u1".to_string()]),
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            json.contains("\"instanceId\""),
            "Expected camelCase, got: {}",
            json
        );
        assert!(
            json.contains("\"allowedUserIds\""),
            "Expected camelCase, got: {}",
            json
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::ExposureUpdate { payload } => {
                assert_eq!(payload.instance_id, "inst-1");
                assert_eq!(payload.exposure, InstanceExposure::Public);
            }
            _ => panic!("Expected ExposureUpdate"),
        }
    }

    #[test]
    fn test_status_update_roundtrip() {
        let msg = TunnelMessage::StatusUpdate {
            payload: StatusUpdatePayload {
                instance_id: "inst-1".to_string(),
                status: InstanceStatus::Busy,
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            json.contains("\"instanceId\""),
            "Expected camelCase, got: {}",
            json
        );
        assert!(
            json.contains("\"status\""),
            "Expected status field, got: {}",
            json
        );
        assert!(
            json.contains("\"busy\""),
            "Expected snake_case status value, got: {}",
            json
        );
        assert!(
            json.contains("\"status_update\""),
            "Expected snake_case tag"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::StatusUpdate { payload } => {
                assert_eq!(payload.instance_id, "inst-1");
                assert_eq!(payload.status, InstanceStatus::Busy);
            }
            _ => panic!("Expected StatusUpdate"),
        }
    }

    // -- Issue #29 (Slice A): cross-runtime a2a wire shape ------------

    /// `AgentToAgentRequest` round-trips with all fields populated.
    /// The on-wire tag is `agent_to_agent_request` (snake_case, to
    /// match the existing dispatch table on the hub side) and the
    /// field names are camelCase (matching every other tunnel
    /// message). Slice B (outbound signer) and Slice C (inbound
    /// verifier) will read these names verbatim, so pinning them
    /// here also pins the contract with pekohub#14.
    #[test]
    fn test_agent_to_agent_request_roundtrip() {
        let msg = TunnelMessage::AgentToAgentRequest {
            request_id: "req-abc-123".to_string(),
            caller_runtime_id: "did:key:zRuntime1".to_string(),
            caller_agent_did: "did:peko:agent:caller-hash".to_string(),
            target_agent_did: "did:peko:agent:target-hash".to_string(),
            session_id: Some("sess-xyz".to_string()),
            message: "review this PR".to_string(),
            team: Some("default".to_string()),
            signature: "base64url-sig".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();

        assert!(
            json.contains("\"agent_to_agent_request\""),
            "tag must be snake_case `agent_to_agent_request`, got: {json}"
        );
        // Every field is camelCase on the wire.
        assert!(
            json.contains("\"requestId\""),
            "field requestId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"callerRuntimeId\""),
            "field callerRuntimeId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"callerAgentDid\""),
            "field callerAgentDid must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"targetAgentDid\""),
            "field targetAgentDid must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"sessionId\""),
            "field sessionId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"signature\""),
            "signature must be present on the wire, got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::AgentToAgentRequest {
                request_id,
                caller_runtime_id,
                caller_agent_did,
                target_agent_did,
                session_id,
                message,
                team,
                signature,
            } => {
                assert_eq!(request_id, "req-abc-123");
                assert_eq!(caller_runtime_id, "did:key:zRuntime1");
                assert_eq!(caller_agent_did, "did:peko:agent:caller-hash");
                assert_eq!(target_agent_did, "did:peko:agent:target-hash");
                assert_eq!(session_id.as_deref(), Some("sess-xyz"));
                assert_eq!(message, "review this PR");
                assert_eq!(team.as_deref(), Some("default"));
                assert_eq!(signature, "base64url-sig");
            }
            other => panic!("Expected AgentToAgentRequest, got: {other:?}"),
        }
    }

    /// Minimal `AgentToAgentRequest` — no `session_id`, no `team`.
    /// Both Option fields must be omitted from the wire form so
    /// pre-#29 PekoHub doesn't see "unknown null field"; this is the
    /// same back-compat shape the existing `bundleRef` / `metadata`
    /// fields use elsewhere in this enum.
    #[test]
    fn test_agent_to_agent_request_minimal_roundtrip() {
        let msg = TunnelMessage::AgentToAgentRequest {
            request_id: "req-min".to_string(),
            caller_runtime_id: "did:key:zRuntime1".to_string(),
            caller_agent_did: "did:peko:agent:caller-hash".to_string(),
            target_agent_did: "did:peko:agent:target-hash".to_string(),
            session_id: None,
            message: "hi".to_string(),
            team: None,
            signature: "sig".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();

        assert!(
            !json.contains("sessionId"),
            "session_id must be omitted from the wire when None; got: {json}"
        );
        assert!(
            !json.contains("\"team\""),
            "team must be omitted from the wire when None; got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::AgentToAgentRequest {
                session_id, team, ..
            } => {
                assert!(session_id.is_none());
                assert!(team.is_none());
            }
            other => panic!("Expected AgentToAgentRequest, got: {other:?}"),
        }
    }

    /// `AgentToAgentResponse` round-trips with a binary payload (the
    /// IPC `ResponsePacket` form, opaque at this layer). Field name
    /// is camelCase on the wire; the tag is snake_case
    /// `agent_to_agent_response`.
    #[test]
    fn test_agent_to_agent_response_roundtrip() {
        let msg = TunnelMessage::AgentToAgentResponse {
            request_id: "req-abc-123".to_string(),
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();

        assert!(
            json.contains("\"agent_to_agent_response\""),
            "tag must be snake_case `agent_to_agent_response`, got: {json}"
        );
        assert!(
            json.contains("\"requestId\""),
            "field requestId must be camelCase, got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::AgentToAgentResponse {
                request_id,
                payload,
            } => {
                assert_eq!(request_id, "req-abc-123");
                assert_eq!(payload, vec![0xde, 0xad, 0xbe, 0xef]);
            }
            other => panic!("Expected AgentToAgentResponse, got: {other:?}"),
        }
    }
}
