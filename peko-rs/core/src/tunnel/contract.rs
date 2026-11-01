//! The tunnel wire contract with PekoHub, as a golden fixture.
//!
//! `fixtures/tunnel_messages.json` holds one sample of every
//! [`TunnelMessage`] (one per channel-event kind for channel envelopes),
//! serialized by the real types, with every optional field set so its
//! name is on the wire. PekoHub's CI type-checks each sample against its
//! own `TunnelMessage` type, so a field one side renames, adds, or drops
//! fails there instead of in production.
//!
//! Adding a variant fails to compile in [`variant`] until it is named
//! there and given a sample here. Regenerate the fixture with
//! `UPDATE_TUNNEL_FIXTURE=1 cargo test -p peko --lib tunnel::contract`.

use super::protocol::{
    ExposureUpdatePayload, InstanceAnnouncePayload, InstanceDeregisterPayload, InstanceExposure,
    InstanceHeartbeatPayload, InstanceStatus, InstanceType, StatusUpdatePayload, TunnelMessage,
};
use peko_protocol::channel::{
    Attachment, ChannelEvent, ChannelId, ConversationKind, InitialMember,
};

const FIXTURE: &str = include_str!("fixtures/tunnel_messages.json");
const FIXTURE_PATH: &str = "src/tunnel/fixtures/tunnel_messages.json";

/// Every variant's wire tag. The exhaustive match is the point: a new
/// variant does not compile until it is listed, and the test then
/// requires a sample of it.
fn variant(message: &TunnelMessage) -> &'static str {
    match message {
        TunnelMessage::RuntimeHello { .. } => "runtime_hello",
        TunnelMessage::TunnelChallenge { .. } => "tunnel_challenge",
        TunnelMessage::TunnelChallengeAck { .. } => "tunnel_challenge_ack",
        TunnelMessage::TunnelReady { .. } => "tunnel_ready",
        TunnelMessage::Heartbeat { .. } => "heartbeat",
        TunnelMessage::HeartbeatAck { .. } => "heartbeat_ack",
        TunnelMessage::Disconnect { .. } => "disconnect",
        TunnelMessage::ProxiedRequest { .. } => "proxied_request",
        TunnelMessage::ProxiedResponse { .. } => "proxied_response",
        TunnelMessage::StreamChunk { .. } => "stream_chunk",
        TunnelMessage::StreamEnd { .. } => "stream_end",
        TunnelMessage::StreamIteration { .. } => "stream_iteration",
        TunnelMessage::PrincipalRegistrationRejected { .. } => "principal_registration_rejected",
        TunnelMessage::PrincipalRegistered { .. } => "principal_registered",
        TunnelMessage::PrincipalUnlinked { .. } => "principal_unlinked",
        TunnelMessage::PrincipalTransfer { .. } => "principal_transfer",
        TunnelMessage::PrincipalTransferRejected { .. } => "principal_transfer_rejected",
        TunnelMessage::PrincipalOwnerChanged { .. } => "principal_owner_changed",
        TunnelMessage::InstanceDeregister { .. } => "instance_deregister",
        TunnelMessage::InstanceAnnounce { .. } => "instance_announce",
        TunnelMessage::InstanceHeartbeat { .. } => "instance_heartbeat",
        TunnelMessage::ExposureUpdate { .. } => "exposure_update",
        TunnelMessage::StatusUpdate { .. } => "status_update",
        TunnelMessage::TunnelChannelEvent { .. } => "tunnel_channel_event",
        TunnelMessage::TunnelChannelInvite { .. } => "tunnel_channel_invite",
    }
}

const ALL_VARIANTS: &[&str] = &[
    "runtime_hello",
    "tunnel_challenge",
    "tunnel_challenge_ack",
    "tunnel_ready",
    "heartbeat",
    "heartbeat_ack",
    "disconnect",
    "proxied_request",
    "proxied_response",
    "stream_chunk",
    "stream_end",
    "stream_iteration",
    "principal_registration_rejected",
    "principal_registered",
    "principal_unlinked",
    "principal_transfer",
    "principal_transfer_rejected",
    "principal_owner_changed",
    "instance_deregister",
    "instance_announce",
    "instance_heartbeat",
    "exposure_update",
    "status_update",
    "tunnel_channel_event",
    "tunnel_channel_invite",
];

const RUNTIME_A: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
const RUNTIME_B: &str = "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH";
const HUB_ID: &str = "156f1a95-d977-4c4a-979e-8ed4848ae6eb";
const ACCOUNT: &str = "0b6c7a4e-1d2f-4a3b-8c9d-0e1f2a3b4c5d";

fn channel_envelope(event: ChannelEvent) -> TunnelMessage {
    TunnelMessage::TunnelChannelEvent {
        request_id: format!("evt-{}", event.kind()),
        source_runtime_id: RUNTIME_A.into(),
        recipient_runtime_id: RUNTIME_B.into(),
        source_principal_did: "did:key:z6MkPrincipal".into(),
        channel_id: "group:research".into(),
        event,
        signature: "runtime-signature".into(),
        author_signature: "author-signature".into(),
    }
}

/// One sample per variant (per channel-event kind), optional fields set.
fn samples() -> Vec<TunnelMessage> {
    let channel = ChannelId("group:research".into());
    let at = "2026-11-01T10:00:00Z".to_string();
    vec![
        TunnelMessage::RuntimeHello {
            runtime_id: RUNTIME_A.into(),
            nonce: "client-nonce".into(),
            signature: "hello-signature".into(),
        },
        TunnelMessage::TunnelChallenge {
            nonce: "server-nonce".into(),
        },
        TunnelMessage::TunnelChallengeAck {
            nonce: "server-nonce".into(),
            signature: "challenge-signature".into(),
        },
        TunnelMessage::TunnelReady {
            heartbeat_interval_secs: 30,
        },
        TunnelMessage::Heartbeat { seq: 7 },
        TunnelMessage::HeartbeatAck { seq: 7 },
        TunnelMessage::Disconnect {
            reason: "shutting down".into(),
        },
        TunnelMessage::ProxiedRequest {
            request_id: "req-1".into(),
            principal: "ada".into(),
            payload: vec![123, 125],
        },
        TunnelMessage::ProxiedResponse {
            request_id: "req-1".into(),
            payload: vec![123, 125],
        },
        TunnelMessage::StreamChunk {
            request_id: "req-1".into(),
            seq: 1,
            payload: vec![123, 125],
        },
        TunnelMessage::StreamEnd {
            request_id: "req-1".into(),
        },
        TunnelMessage::StreamIteration {
            request_id: "req-1".into(),
            iteration: 2,
        },
        TunnelMessage::PrincipalRegistrationRejected {
            registration_id: "reg-1".into(),
            message: "already linked".into(),
        },
        TunnelMessage::PrincipalRegistered {
            registration_id: Some("reg-1".into()),
            local_id: "prin_ada".into(),
            hub_id: HUB_ID.into(),
            signing_did: "did:key:z6MkPrincipal".into(),
        },
        TunnelMessage::PrincipalUnlinked {
            local_id: "prin_ada".into(),
            hub_id: HUB_ID.into(),
        },
        TunnelMessage::PrincipalTransfer {
            request_id: "xfer-1".into(),
            local_id: "prin_ada".into(),
            hub_id: HUB_ID.into(),
            owner_id: ACCOUNT.into(),
        },
        TunnelMessage::PrincipalTransferRejected {
            request_id: "xfer-1".into(),
            message: "not the owner".into(),
        },
        TunnelMessage::PrincipalOwnerChanged {
            request_id: Some("xfer-1".into()),
            local_id: "prin_ada".into(),
            hub_id: HUB_ID.into(),
            owner_id: ACCOUNT.into(),
        },
        TunnelMessage::InstanceDeregister {
            payload: InstanceDeregisterPayload {
                id: "inst-1".into(),
            },
        },
        TunnelMessage::InstanceAnnounce {
            payload: InstanceAnnouncePayload {
                id: "inst-1".into(),
                instance_type: InstanceType::Principal,
                name: "ada".into(),
                registration_id: Some("reg-1".into()),
                local_id: "prin_ada".into(),
                identity_key: "3f0f6b1e-0000-4000-8000-000000000001".into(),
                agent_did: Some("did:key:z6MkPrincipal".into()),
                principal_did: Some("did:key:z6MkPrincipal".into()),
                principal_pop: Some("header.payload.signature".into()),
                owner: Some(peko_auth::ActorRef::User(peko_subject::Route::hub(
                    ACCOUNT.to_string(),
                ))),
                bundle_ref: Some("alice/ada:1.0.0".into()),
                runtime_display_name: Some("peko-runtime".into()),
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Public,
                capabilities: Some(vec!["chat".into()]),
                metadata: Some(serde_json::Map::new()),
                avatar: Some("🦉".into()),
            },
        },
        TunnelMessage::InstanceHeartbeat {
            payload: InstanceHeartbeatPayload {
                id: "inst-1".into(),
                status: InstanceStatus::Busy,
                timestamp: at.clone(),
            },
        },
        TunnelMessage::ExposureUpdate {
            payload: ExposureUpdatePayload {
                instance_id: "inst-1".into(),
                exposure: InstanceExposure::Private,
            },
        },
        TunnelMessage::StatusUpdate {
            payload: StatusUpdatePayload {
                instance_id: "inst-1".into(),
                status: InstanceStatus::Offline,
            },
        },
        channel_envelope(ChannelEvent::Created {
            channel: channel.clone(),
            creator: "did:key:z6MkPrincipal".into(),
            name: "research".into(),
            at: at.clone(),
        }),
        channel_envelope(ChannelEvent::Posted {
            channel: channel.clone(),
            author: "did:key:z6MkPrincipal".into(),
            parent: Some("msg-0".into()),
            text: "hello".into(),
            at: at.clone(),
            via: Some("/user-alice".into()),
            msg_id: Some("msg-1".into()),
            reply_to: Some("msg-0".into()),
            attachments: vec![Attachment {
                name: "notes.md".into(),
                path: "attachments/notes.md".into(),
                mime: "text/markdown".into(),
                size: 42,
            }],
        }),
        channel_envelope(ChannelEvent::MemberJoined {
            channel: channel.clone(),
            member: "did:key:z6MkOther".into(),
            at: at.clone(),
        }),
        channel_envelope(ChannelEvent::MemberLeft {
            channel: channel.clone(),
            member: "did:key:z6MkOther".into(),
            at: at.clone(),
        }),
        channel_envelope(ChannelEvent::Received {
            channel,
            request_ids: vec!["evt-posted".into()],
            at,
        }),
        TunnelMessage::TunnelChannelInvite {
            request_id: "inv-1".into(),
            source_runtime_id: RUNTIME_A.into(),
            recipient_runtime_id: RUNTIME_B.into(),
            source_principal_did: format!("user:hub:{ACCOUNT}"),
            channel_id: "dm_0123".into(),
            creator: format!("user:hub:{ACCOUNT}"),
            creator_did: format!("user:hub:{ACCOUNT}"),
            name: "dm".into(),
            kind: ConversationKind::Dm,
            initial_members: vec![InitialMember {
                principal_did: "did:key:z6MkPrincipal".into(),
                hub_id: Some(HUB_ID.into()),
                identity_key: Some("3f0f6b1e-0000-4000-8000-000000000001".into()),
                name: Some("ada".into()),
                runtime_id: Some(RUNTIME_B.into()),
            }],
            signature: "runtime-signature".into(),
            author_signature: String::new(),
        },
    ]
}

fn render(messages: &[TunnelMessage]) -> String {
    let mut out = serde_json::to_string_pretty(messages).expect("samples serialize");
    out.push('\n');
    out
}

#[test]
fn every_variant_has_a_sample() {
    let covered: std::collections::BTreeSet<&str> = samples().iter().map(variant).collect();
    let all: std::collections::BTreeSet<&str> = ALL_VARIANTS.iter().copied().collect();
    assert_eq!(covered, all, "give every tunnel message a sample");
}

#[test]
fn the_fixture_is_the_samples() {
    let rendered = render(&samples());
    if std::env::var_os("UPDATE_TUNNEL_FIXTURE").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_PATH);
        std::fs::write(&path, &rendered).expect("write the fixture");
        return;
    }
    assert_eq!(
        FIXTURE, rendered,
        "the tunnel fixture is stale: regenerate it with \
         UPDATE_TUNNEL_FIXTURE=1 cargo test -p peko --lib tunnel::contract"
    );
}

#[test]
fn the_fixture_decodes() {
    let messages: Vec<TunnelMessage> = serde_json::from_str(FIXTURE).expect("fixture decodes");
    assert_eq!(messages.len(), samples().len());
}
