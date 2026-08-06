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
///
/// Mirrors `peko_auth::Exposure` on the wire. See that enum's doc
/// comment for the semantics of each variant. The two enums are
/// kept separate so the persisted schema can stay in `peko-auth`
/// (a leaf crate) without dragging wire-format concerns in.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceExposure {
    Private,
    Public,
    Unlisted,
    #[default]
    Unexposed,
}

/// Type of an instance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceType {
    Agent,
    Principal,
}

// ── Edge conversions from the principal-owned persisted enums ──────────────
// `PrincipalConfig` stores its own `Exposure`/`Status` (F4: the persisted
// schema must not depend on tunnel wire types). The tunnel converts them to
// its wire enums here, at the boundary — the only place allowed to know both.

impl From<peko_auth::Exposure> for InstanceExposure {
    fn from(e: peko_auth::Exposure) -> Self {
        match e {
            peko_auth::Exposure::Private => Self::Private,
            peko_auth::Exposure::Public => Self::Public,
            peko_auth::Exposure::Unlisted => Self::Unlisted,
            peko_auth::Exposure::Unexposed => Self::Unexposed,
        }
    }
}

impl From<crate::principal::config::Status> for InstanceStatus {
    fn from(s: crate::principal::config::Status) -> Self {
        match s {
            crate::principal::config::Status::Online => Self::Online,
            crate::principal::config::Status::Offline => Self::Offline,
            crate::principal::config::Status::Busy => Self::Busy,
            crate::principal::config::Status::Error => Self::Error,
        }
    }
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
    /// Stable per-principal DID. Principals are the new single-actor
    /// boundary, so this is the DID exposed to PekoHub and used for
    /// inbound A2A routing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_display_name: Option<String>,
    pub status: InstanceStatus,
    pub exposure: InstanceExposure,
    /// Typed allow-list per ADR-041. PekoHub's post-#19 reader ignores
    /// the legacy `allowedUsers` string-array field; the runtime now
    /// emits `allowedPrincipals: Vec<Subject>` instead. The wire token
    /// is `allowedPrincipals` (camelCase) and the entries are
    /// `{kind, id}` objects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_principals: Option<Vec<peko_auth::Subject>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
    /// Callee transport preference for cross-runtime `principal_send`.
    /// Set by the runtime on `instance_announce` so the hub can return
    /// it from directory resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_preference: Option<crate::tunnel::known_runtimes::TransportPreference>,
    /// Runtime-level advertised direct endpoint for inbound direct
    /// cross-runtime connections (e.g. `wss://203.0.113.4:11436`).
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "runtimeDirectEndpoint"
    )]
    pub runtime_direct_endpoint: Option<String>,
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
    /// Typed allow-list per ADR-041. Replaces the legacy
    /// `allowedUserIds: Vec<String>` field that PekoHub post-#19
    /// no longer reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_principals: Option<Vec<peko_auth::Subject>>,
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
        /// Target principal name
        principal: String,
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

    /// Per-iteration boundary marker on the streaming channel.
    ///
    /// Pushed just before the first `StreamChunk` of each new
    /// agentic loop iteration so the hub can break assistant text
    /// into one bubble per iteration and render a "thinking"
    /// indicator between iterations. Mirrors the IPC
    /// `PrincipalSentIteration` packet's wire shape; iteration is
    /// 1-based and per `request_id`.
    #[serde(rename = "stream_iteration", rename_all = "camelCase")]
    StreamIteration {
        request_id: String,
        iteration: u32,
    },

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

    // --- Cross-runtime principal-to-principal (issue #29) ---
    /// Principal-to-principal request from the **caller** runtime to the
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
    /// depth) before attributing the receiving principal's session to
    /// `Subject::Principal(caller_principal_did)` and dispatching.
    ///
    /// Slice A only defines and round-trips the wire shape. Slice B
    /// adds the outbound signer (`PekoHubCredential::sign(...)` against
    /// the canonical pre-image `request_id || caller_runtime_id ||
    /// caller_principal_did || target_principal_did || message`).
    /// Slice C adds the inbound verifier + dispatcher route.
    ///
    /// **Wire tag note.** The Rust enum name is
    /// `PrincipalToPrincipalRequest`; the on-wire tag is
    /// `principal_to_principal_request` (snake_case). The tag was
    /// previously `agent_to_agent_request` (pre-ADR-042 name); it was
    /// renamed to match pekohub's TypeScript decoder and the
    /// principal-as-container-v2 unification (PR-A commit 1).
    #[serde(rename = "principal_to_principal_request", rename_all = "camelCase")]
    PrincipalToPrincipalRequest {
        /// Globally unique request ID. Used to correlate the matching
        /// `PrincipalToPrincipalResponse` and to scope the canonical
        /// signature pre-image (replay protection: PekoHub MAY
        /// reject duplicate IDs within a sliding window).
        request_id: String,
        /// The caller runtime's `did:key` form (the `runtime_id` it
        /// presented in `RuntimeHello`). The target runtime verifies
        /// the `signature` field against the public key derived from
        /// this DID and rejects the message if the caller is not on
        /// the hub's allowlist.
        caller_runtime_id: String,
        /// The caller principal's stable DID. Projected to
        /// `Subject::Principal(caller_principal_did)` on the target
        /// side for session attribution, permission grant lookup, and
        /// the `AuditEvent.caller` field (issue #26).
        caller_principal_did: String,
        /// The **target** principal's stable DID. The target runtime
        /// resolves this against its local principal table
        /// (`PrincipalConfig.principal_did`) to find the principal
        /// name to dispatch on. A missing target_principal_did on
        /// the receiving side is a 404 — the hub-side directory
        /// should have caught this, so it most often indicates a
        /// stale resolution cached on the caller.
        target_principal_did: String,
        /// The message body to deliver to the target principal.
        message: String,
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

    /// Principal-to-principal response from the **target** runtime back
    /// to the **caller**, also proxied through PekoHub. Issue #29
    /// (Slice A — wire shape).
    ///
    /// The `payload` is the serialized form of an IPC `ResponsePacket`
    /// (same as `ProxiedResponse.payload`) so the caller-side decoder
    /// can be the same code path for both user-originated and
    /// principal-originated proxied responses. Slice C is what actually
    /// emits this; Slice A only pins the shape.
    ///
    /// **Wire tag note.** Renamed from `agent_to_agent_response`
    /// alongside `PrincipalToPrincipalRequest` (PR-A commit 1).
    #[serde(rename = "principal_to_principal_response", rename_all = "camelCase")]
    PrincipalToPrincipalResponse {
        /// Matches the `request_id` of the originating
        /// `PrincipalToPrincipalRequest`. PekoHub uses this to route
        /// the response back to the caller's tunnel.
        request_id: String,
        /// Serialized IPC `ResponsePacket` (same encoding as
        /// `ProxiedResponse.payload`). Slice C decides whether the
        /// target's `AuditEvent` is emitted on the target side, on
        /// the caller side, or both — the payload itself is
        /// indifferent.
        payload: Vec<u8>,
    },

    // --- Cross-runtime channel events (peko-channel cross-runtime PR-A) ---
    /// Channel-event fan-out from the source runtime to the other
    /// runtimes that host a remote member of the channel. Pure push:
    /// no `request_id`-correlated response. PekoHub forwards to each
    /// recipient runtime's tunnel connection verbatim, with the same
    /// source-allowlist check used for `PrincipalToPrincipalRequest`.
    ///
    /// Source of truth: the **creator's** runtime owns the channel's
    /// `events.jsonl`. Every other runtime that hosts a member of the
    /// channel keeps a local mirror (`<runtime_dir>/channels/<chan_id>/
    /// events.jsonl`) that is hydrated by these fan-outs.
    ///
    /// The `signature` is an ed25519 signature (base64url-no-pad) over
    /// the canonical pre-image
    /// `domain || request_id || source_runtime_id ||
    /// source_principal_did || channel_id || event_json` (see
    /// `tunnel_channel_signature`). The receiver derives the
    /// verifying key from `source_runtime_id` (self-certifying
    /// `did:key`) — same defense-in-depth layer as the DM path. The
    /// hub's source-allowlist is the primary gate; signature
    /// verification is the receiver-side check that catches a hub bug
    /// or stale forwarder.
    ///
    /// `event` carries the full `ChannelEvent` payload (not just the
    /// `kind` discriminant) so the receiver can write the event
    /// verbatim into its local mirror — no second round-trip needed.
    #[serde(rename = "tunnel_channel_event", rename_all = "camelCase")]
    TunnelChannelEvent {
        /// Unique-per-fanout id (UUIDv4 from the source runtime).
        /// Not used for response correlation (channels are
        /// push-only); exists so a hub can scope replay protection
        /// and so audit logs can join an outbound `forwarded` row to
        /// each inbound `received` row.
        request_id: String,
        /// The source runtime's `did:key` form (the `runtime_id` it
        /// presented in `RuntimeHello`). The receiver derives the
        /// verifying key from this DID and rejects the message if
        /// `signature` does not verify against the pre-image built
        /// from the rest of these fields.
        source_runtime_id: String,
        /// The runtime the hub should forward this envelope to. The
        /// outbound `fanout_event` loop emits one envelope per
        /// unique recipient runtime, with each envelope addressed
        /// to that runtime's `did:key`. Without this field the hub
        /// has no way to route — option (a) in the cross-runtime
        /// plan ("implicit via known-runtimes registry") would
        /// require the hub to track channel membership, which it
        /// deliberately doesn't. The field is added here so the
        /// hub can stay a pure relay.
        recipient_runtime_id: String,
        /// The local principal on the source runtime that authored
        /// the underlying event. Carried for audit only — the
        /// signature itself is over the runtime-level pre-image.
        source_principal_did: String,
        /// The channel id (`chan_<8 base36>`). The receiver looks up
        /// the local mirror directory and appends `event` to
        /// `events.jsonl` under it.
        channel_id: String,
        /// The full `ChannelEvent` payload — `Created`, `Posted`,
        /// `MemberJoined`, or `MemberLeft`. Carries the timestamp
        /// assigned by the source runtime (the receiver MUST NOT
        /// re-stamp it on append).
        event: peko_protocol::channel::ChannelEvent,
        /// Ed25519 signature, base64url-encoded, over the canonical
        /// pre-image described in the module docs. Left as `String`
        /// (matching `PrincipalToPrincipalRequest.signature`) so the
        /// wire form matches the existing tunnel signature fields
        /// and the pekohub TypeScript decoder can share a parser.
        signature: String,
    },

    /// Bootstrap envelope for a cross-runtime channel invite.
    ///
    /// Sent from the **creator** runtime to each **invitee**'s
    /// hosting runtime when a non-self principal is invited to a
    /// channel (peko-channel cross-runtime PR-3a, the pre-req for
    /// PR-3's desktop invite UX). The receiver uses `join_remote`
    /// to create a local mirror directory + a synthetic
    /// `ChannelEvent::Created` event so PR-2b's `peko-stream`
    /// listener fires on the desktop.
    ///
    /// Mirrors `TunnelChannelEvent`'s shape: hub acts as a pure
    /// relay, source-allowlist + recipient lookup. The `creator`
    /// and `name` come from the source runtime's `ChannelStore`
    /// metadata (snapshotted at invite time) so the receiver can
    /// populate `meta.json` without a second round-trip.
    #[serde(rename = "tunnel_channel_invite", rename_all = "camelCase")]
    TunnelChannelInvite {
        /// Unique-per-fanout id (UUIDv4 from the source runtime).
        /// Mirrors `TunnelChannelEvent.request_id`; not used for
        /// response correlation (channel events are push-only).
        request_id: String,
        /// Source runtime's `did:key`. The receiver derives the
        /// verifying key from this DID and rejects the envelope if
        /// `signature` does not verify against the pre-image built
        /// from the rest of these fields.
        source_runtime_id: String,
        /// The runtime the hub should forward this envelope to. The
        /// outbound `fanout_invite` loop emits one envelope per
        /// unique invitee runtime. Without this field the hub has
        /// no way to route — channel membership tracking is a
        /// source-runtime concern.
        recipient_runtime_id: String,
        /// The local principal on the source runtime that issued
        /// the invite. Recorded for audit only — the signature is
        /// over the runtime-level pre-image.
        source_principal_did: String,
        /// The channel id (`chan_<8 base36>`). The receiver looks
        /// up the local mirror directory and calls `join_remote`
        /// with the rest of the envelope to bootstrap it.
        channel_id: String,
        /// The creator's display name (principal DID, e.g.
        /// `prin_alice`). Mirrors the `creator` field written into
        /// the local mirror's `meta.json`.
        creator: String,
        /// Human-readable channel name (`team`, `general`, etc.).
        /// Snapshotted from the source runtime's `meta.json` at
        /// invite time so the receiver doesn't need a follow-up
        /// `peek` to display the channel.
        name: String,
        /// Initial membership snapshot: every principal that
        /// should appear in the receiver's `members.json` row
        /// table. Each row pairs a principal DID with the runtime
        /// that hosts it (`runtime_id: None` = local to the
        /// source). The receiver builds both the local-members
        /// (`local_members`) and `remote_members` arrays from this
        /// list by partitioning on the optional `runtime_id`.
        initial_members: Vec<InitialMember>,
        /// Ed25519 signature, base64url-no-pad, over the canonical
        /// pre-image described in `tunnel_channel_signature`. Same
        /// shape as `TunnelChannelEvent.signature`.
        signature: String,
    },
}

/// One row of the `initialMembers` list on
/// `TunnelChannelInvite`. `runtime_id: None` means the principal
/// is local to the source runtime (will land in `members.json`'s
/// local array); `Some(runtime_id)` means the principal lives on a
/// peer runtime and the receiver should record a `RemoteMember`
/// row in `members.json`.
///
/// Canonical home is `peko_protocol::channel::InitialMember`
/// (mirrors how `TunnelChannelEvent.event` re-uses
/// `peko_protocol::channel::ChannelEvent`). Defined here as a
/// type alias so the field on `TunnelChannelInvite` reads naturally
/// at the call site (`initial_members: Vec<InitialMember>`).
pub type InitialMember = peko_protocol::channel::InitialMember;

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
                principal_did: None,
                runtime_display_name: Some("Test".to_string()),
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_principals: Some(vec![peko_auth::Subject::User("u1".to_string())]),
                capabilities: Some(vec!["c1".to_string()]),
                metadata: Some(metadata),
                transport_preference: Some(
                    crate::tunnel::known_runtimes::TransportPreference::Direct,
                ),
                runtime_direct_endpoint: Some("wss://203.0.113.4:11436".to_string()),
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
            json.contains("\"allowedPrincipals\""),
            "Expected camelCase, got: {}",
            json
        );
        assert!(
            json.contains("\"transportPreference\":\"direct\""),
            "Expected transportPreference in wire form, got: {}",
            json
        );
        assert!(
            json.contains("\"runtimeDirectEndpoint\":\"wss://203.0.113.4:11436\""),
            "Expected runtimeDirectEndpoint in wire form, got: {}",
            json
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceAnnounce { payload } => {
                assert_eq!(payload.id, "inst-1");
                assert_eq!(payload.runtime_display_name, Some("Test".to_string()));
                assert_eq!(
                    payload.transport_preference,
                    Some(crate::tunnel::known_runtimes::TransportPreference::Direct)
                );
                assert_eq!(
                    payload.runtime_direct_endpoint,
                    Some("wss://203.0.113.4:11436".to_string())
                );
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
                principal_did: None,
                runtime_display_name: None,
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_principals: None,
                capabilities: None,
                metadata: None,
                transport_preference: None,
                runtime_direct_endpoint: None,
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(!json.contains("bundleRef"), "None fields should be skipped");
        assert!(
            !json.contains("transportPreference"),
            "None transport_preference should be skipped"
        );
        assert!(
            !json.contains("runtimeDirectEndpoint"),
            "None runtime_direct_endpoint should be skipped"
        );
        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::InstanceAnnounce { payload } => {
                assert_eq!(payload.bundle_ref, None);
                assert_eq!(payload.transport_preference, None);
                assert_eq!(payload.runtime_direct_endpoint, None);
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
                principal_did: None,
                runtime_display_name: None,
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_principals: None,
                capabilities: None,
                metadata: None,
                transport_preference: None,
                runtime_direct_endpoint: None,
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
                principal_did: None,
                runtime_display_name: None,
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_principals: None,
                capabilities: None,
                metadata: None,
                transport_preference: None,
                runtime_direct_endpoint: None,
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
            principal: "agent-1".to_string(),
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
                principal,
                payload,
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(principal, "agent-1");
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
                allowed_principals: Some(vec![peko_auth::Subject::User("u1".to_string())]),
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
            json.contains("\"allowedPrincipals\""),
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

    // -- Issue #29 (Slice A): cross-runtime p2p wire shape ------------

    /// `PrincipalToPrincipalRequest` round-trips with all fields
    /// populated. The on-wire tag is `principal_to_principal_request`
    /// (snake_case — renamed PR-A commit 1 to match pekohub's
    /// TypeScript decoder + ADR-042) and the field names are camelCase
    /// (matching every other tunnel message). Slice B (outbound signer)
    /// and Slice C (inbound verifier) read these names verbatim, so
    /// pinning them here also pins the contract with pekohub#14.
    #[test]
    fn test_principal_to_principal_request_roundtrip() {
        let msg = TunnelMessage::PrincipalToPrincipalRequest {
            request_id: "req-abc-123".to_string(),
            caller_runtime_id: "did:key:zRuntime1".to_string(),
            caller_principal_did: "did:peko:agent:caller-hash".to_string(),
            target_principal_did: "did:peko:agent:target-hash".to_string(),
            message: "review this PR".to_string(),
            signature: "base64url-sig".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();

        assert!(
            json.contains("\"principal_to_principal_request\""),
            "tag must be snake_case `principal_to_principal_request`, got: {json}"
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
            json.contains("\"callerPrincipalDid\""),
            "field callerPrincipalDid must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"targetPrincipalDid\""),
            "field targetPrincipalDid must be camelCase, got: {json}"
        );
        assert!(
            !json.contains("sessionId"),
            "session_id must not appear on the wire, got: {json}"
        );
        assert!(
            json.contains("\"signature\""),
            "signature must be present on the wire, got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::PrincipalToPrincipalRequest {
                request_id,
                caller_runtime_id,
                caller_principal_did,
                target_principal_did,
                message,
                signature,
            } => {
                assert_eq!(request_id, "req-abc-123");
                assert_eq!(caller_runtime_id, "did:key:zRuntime1");
                assert_eq!(caller_principal_did, "did:peko:agent:caller-hash");
                assert_eq!(target_principal_did, "did:peko:agent:target-hash");
                assert_eq!(message, "review this PR");
                assert_eq!(signature, "base64url-sig");
            }
            other => panic!("Expected PrincipalToPrincipalRequest, got: {other:?}"),
        }
    }

    /// `PrincipalToPrincipalResponse` round-trips with a binary payload
    /// (the IPC `ResponsePacket` form, opaque at this layer). Field
    /// name is camelCase on the wire; the tag is snake_case
    /// `principal_to_principal_response` (renamed PR-A commit 1).
    #[test]
    fn test_principal_to_principal_response_roundtrip() {
        let msg = TunnelMessage::PrincipalToPrincipalResponse {
            request_id: "req-abc-123".to_string(),
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();

        assert!(
            json.contains("\"principal_to_principal_response\""),
            "tag must be snake_case `principal_to_principal_response`, got: {json}"
        );
        assert!(
            json.contains("\"requestId\""),
            "field requestId must be camelCase, got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::PrincipalToPrincipalResponse {
                request_id,
                payload,
            } => {
                assert_eq!(request_id, "req-abc-123");
                assert_eq!(payload, vec![0xde, 0xad, 0xbe, 0xef]);
            }
            other => panic!("Expected PrincipalToPrincipalResponse, got: {other:?}"),
        }
    }

    /// StreamIteration pin: round-trip the new variant with the
    /// snake_case tag and camelCase fields. PekoHub forwards these
    /// frames unchanged and re-projects them into the SSE
    /// `event: iteration` channel, so any wire-shape change here
    /// would silently break the hub-side relay and the SPA's
    /// iteration-bubble UX.
    #[test]
    fn test_stream_iteration_roundtrip() {
        let msg = TunnelMessage::StreamIteration {
            request_id: "req-stream-1".to_string(),
            iteration: 3,
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            json.contains("\"stream_iteration\""),
            "tag must be snake_case `stream_iteration`, got: {json}"
        );
        assert!(
            json.contains("\"requestId\""),
            "field requestId must be camelCase, got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::StreamIteration {
                request_id,
                iteration,
            } => {
                assert_eq!(request_id, "req-stream-1");
                assert_eq!(iteration, 3);
            }
            other => panic!("Expected StreamIteration, got: {other:?}"),
        }
    }

    /// `TunnelChannelEvent` (peko-channel cross-runtime PR-A commit 2)
    /// round-trips with a `Posted` payload. The wire tag is
    /// `tunnel_channel_event` (snake_case); fields are camelCase.
    /// The nested `event` is the canonical `ChannelEvent` JSON shape
    /// (its own `tag = "kind"` discriminant) — pinning it here also
    /// pins the contract with pekohub's forwarding layer.
    #[test]
    fn test_tunnel_channel_event_roundtrip() {
        let event = peko_protocol::channel::ChannelEvent::Posted {
            channel: peko_protocol::channel::ChannelId("chan_abcdefgh".to_string()),
            author: "prin_alice".to_string(),
            parent: None,
            text: "hello from A".to_string(),
            at: "2026-08-06T12:00:00Z".to_string(),
        };
        let msg = TunnelMessage::TunnelChannelEvent {
            request_id: "chan-evt-1".to_string(),
            source_runtime_id: "did:key:zRuntimeA".to_string(),
            recipient_runtime_id: "did:key:zRuntimeB".to_string(),
            source_principal_did: "prin_alice".to_string(),
            channel_id: "chan_abcdefgh".to_string(),
            event,
            signature: "base64url-sig".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();

        // Wire tag is snake_case.
        assert!(
            json.contains("\"tunnel_channel_event\""),
            "tag must be snake_case `tunnel_channel_event`, got: {json}"
        );
        // Fields are camelCase.
        assert!(
            json.contains("\"requestId\""),
            "field requestId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"sourceRuntimeId\""),
            "field sourceRuntimeId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"recipientRuntimeId\""),
            "field recipientRuntimeId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"sourcePrincipalDid\""),
            "field sourcePrincipalDid must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"channelId\""),
            "field channelId must be camelCase, got: {json}"
        );
        // The nested event uses its own `kind` discriminant.
        assert!(
            json.contains("\"kind\":\"posted\""),
            "nested ChannelEvent must use its `kind` tag, got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::TunnelChannelEvent {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                event,
                signature,
            } => {
                assert_eq!(request_id, "chan-evt-1");
                assert_eq!(source_runtime_id, "did:key:zRuntimeA");
                assert_eq!(recipient_runtime_id, "did:key:zRuntimeB");
                assert_eq!(source_principal_did, "prin_alice");
                assert_eq!(channel_id, "chan_abcdefgh");
                assert_eq!(signature, "base64url-sig");
                match event {
                    peko_protocol::channel::ChannelEvent::Posted {
                        channel,
                        author,
                        text,
                        ..
                    } => {
                        assert_eq!(channel.as_str(), "chan_abcdefgh");
                        assert_eq!(author, "prin_alice");
                        assert_eq!(text, "hello from A");
                    }
                    other => panic!("Expected Posted, got: {other:?}"),
                }
            }
            other => panic!("Expected TunnelChannelEvent, got: {other:?}"),
        }
    }

    /// `TunnelChannelInvite` (peko-channel cross-runtime PR-3a
    /// commit 1) round-trips with a two-member `initialMembers`
    /// snapshot — one local to the source runtime, one remote.
    /// Wire tag is `tunnel_channel_invite` (snake_case); fields are
    /// camelCase. Pins the contract with the `sign_channel_invite`
    /// helpers and with pekohub's pure-relay forwarding layer
    /// (mirrors PR-C's `tunnel_channel_event` add).
    #[test]
    fn test_tunnel_channel_invite_roundtrip() {
        let initial_members = vec![
            InitialMember {
                principal_did: "prin_alice".to_string(),
                runtime_id: None,
            },
            InitialMember {
                principal_did: "prin_bob".to_string(),
                runtime_id: Some("did:key:zRuntimeB".to_string()),
            },
        ];
        let msg = TunnelMessage::TunnelChannelInvite {
            request_id: "chan-invite-1".to_string(),
            source_runtime_id: "did:key:zRuntimeA".to_string(),
            recipient_runtime_id: "did:key:zRuntimeB".to_string(),
            source_principal_did: "prin_alice".to_string(),
            channel_id: "chan_abcdefgh".to_string(),
            creator: "prin_alice".to_string(),
            name: "team-chat".to_string(),
            initial_members,
            signature: "base64url-sig".to_string(),
        };
        let bytes = msg.to_bytes().unwrap();
        let json = String::from_utf8(bytes.clone()).unwrap();

        // Wire tag is snake_case.
        assert!(
            json.contains("\"tunnel_channel_invite\""),
            "tag must be snake_case `tunnel_channel_invite`, got: {json}"
        );
        // All flat fields are camelCase.
        assert!(
            json.contains("\"requestId\""),
            "field requestId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"sourceRuntimeId\""),
            "field sourceRuntimeId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"recipientRuntimeId\""),
            "field recipientRuntimeId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"sourcePrincipalDid\""),
            "field sourcePrincipalDid must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"channelId\""),
            "field channelId must be camelCase, got: {json}"
        );
        assert!(
            json.contains("\"initialMembers\""),
            "field initialMembers must be camelCase, got: {json}"
        );
        // `runtime_id: None` is skipped on the wire (no null leak).
        assert!(
            !json.contains("\"runtimeId\":null"),
            "runtime_id=None must be skipped (not serialized as null), got: {json}"
        );
        // The present `runtime_id` is camelCase.
        assert!(
            json.contains("\"runtimeId\":\"did:key:zRuntimeB\""),
            "remote member's runtimeId must be camelCase, got: {json}"
        );

        let decoded = TunnelMessage::from_bytes(&bytes).unwrap();
        match decoded {
            TunnelMessage::TunnelChannelInvite {
                request_id,
                source_runtime_id,
                recipient_runtime_id,
                source_principal_did,
                channel_id,
                creator,
                name,
                initial_members: decoded_members,
                signature,
            } => {
                assert_eq!(request_id, "chan-invite-1");
                assert_eq!(source_runtime_id, "did:key:zRuntimeA");
                assert_eq!(recipient_runtime_id, "did:key:zRuntimeB");
                assert_eq!(source_principal_did, "prin_alice");
                assert_eq!(channel_id, "chan_abcdefgh");
                assert_eq!(creator, "prin_alice");
                assert_eq!(name, "team-chat");
                assert_eq!(signature, "base64url-sig");
                assert_eq!(decoded_members.len(), 2);
                assert_eq!(decoded_members[0].principal_did, "prin_alice");
                assert_eq!(decoded_members[0].runtime_id, None);
                assert_eq!(decoded_members[1].principal_did, "prin_bob");
                assert_eq!(
                    decoded_members[1].runtime_id.as_deref(),
                    Some("did:key:zRuntimeB")
                );
            }
            other => panic!("Expected TunnelChannelInvite, got: {other:?}"),
        }
    }
}
