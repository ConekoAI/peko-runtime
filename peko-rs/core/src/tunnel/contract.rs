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
    AccountDm, AccountGroup, AccountPersonDm, DmBlock, DmRoute, ExposureUpdatePayload,
    InstanceAnnouncePayload, InstanceDeregisterPayload, InstanceExposure, InstanceHeartbeatPayload,
    InstanceStatus, InstanceType, StatusUpdatePayload, TunnelMessage,
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
        TunnelMessage::PeerRuntimeOnline { .. } => "peer_runtime_online",
        TunnelMessage::PrincipalTransfer { .. } => "principal_transfer",
        TunnelMessage::PrincipalTransferRejected { .. } => "principal_transfer_rejected",
        TunnelMessage::PrincipalTransferOffered { .. } => "principal_transfer_offered",
        TunnelMessage::PrincipalOwnerChanged { .. } => "principal_owner_changed",
        TunnelMessage::InstanceDeregister { .. } => "instance_deregister",
        TunnelMessage::InstanceAnnounce { .. } => "instance_announce",
        TunnelMessage::InstanceHeartbeat { .. } => "instance_heartbeat",
        TunnelMessage::ExposureUpdate { .. } => "exposure_update",
        TunnelMessage::StatusUpdate { .. } => "status_update",
        TunnelMessage::TunnelChannelEvent { .. } => "tunnel_channel_event",
        TunnelMessage::TunnelChannelInvite { .. } => "tunnel_channel_invite",
        TunnelMessage::TunnelChannelHistoryRequest { .. } => "tunnel_channel_history_request",
        TunnelMessage::TunnelChannelHistory { .. } => "tunnel_channel_history",
        TunnelMessage::TunnelAttachmentRequest { .. } => "tunnel_attachment_request",
        TunnelMessage::TunnelAttachmentChunk { .. } => "tunnel_attachment_chunk",
        TunnelMessage::TunnelGroupInvitation { .. } => "tunnel_group_invitation",
        TunnelMessage::TunnelGroupJoin { .. } => "tunnel_group_join",
        TunnelMessage::TunnelGroupJoinRequest { .. } => "tunnel_group_join_request",
        TunnelMessage::TunnelAccountDmsRequest { .. } => "tunnel_account_dms_request",
        TunnelMessage::TunnelAccountDms { .. } => "tunnel_account_dms",
        TunnelMessage::TunnelPersonDmInvite { .. } => "tunnel_person_dm_invite",
        TunnelMessage::TunnelDmRoute { .. } => "tunnel_dm_route",
        TunnelMessage::GroupRegister { .. } => "group_register",
        TunnelMessage::GroupRegistered { .. } => "group_registered",
        TunnelMessage::GroupRegisterRejected { .. } => "group_register_rejected",
        TunnelMessage::GroupRoster { .. } => "group_roster",
        TunnelMessage::AccountGroupsRequest { .. } => "account_groups_request",
        TunnelMessage::AccountGroups { .. } => "account_groups",
        TunnelMessage::GroupLookupRequest { .. } => "group_lookup_request",
        TunnelMessage::GroupLookup { .. } => "group_lookup",
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
    "peer_runtime_online",
    "principal_transfer",
    "principal_transfer_rejected",
    "principal_transfer_offered",
    "principal_owner_changed",
    "instance_deregister",
    "instance_announce",
    "instance_heartbeat",
    "exposure_update",
    "status_update",
    "tunnel_channel_event",
    "tunnel_channel_invite",
    "tunnel_channel_history_request",
    "tunnel_channel_history",
    "tunnel_attachment_request",
    "tunnel_attachment_chunk",
    "tunnel_group_invitation",
    "tunnel_group_join",
    "tunnel_group_join_request",
    "tunnel_account_dms_request",
    "tunnel_account_dms",
    "tunnel_person_dm_invite",
    "tunnel_dm_route",
    "group_register",
    "group_registered",
    "group_register_rejected",
    "group_roster",
    "account_groups_request",
    "account_groups",
    "group_lookup_request",
    "group_lookup",
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
        TunnelMessage::PeerRuntimeOnline {
            runtime_id: "did:key:z6MkPeerRuntime".into(),
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
        TunnelMessage::PrincipalTransferOffered {
            request_id: "xfer-1".into(),
            message: "Offered to @bob. It moves when they accept on PekoHub.".into(),
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
                sha256: None,
            }],
            options: vec!["Yes".into(), "Not yet".into()],
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
        TunnelMessage::TunnelChannelHistoryRequest {
            request_id: "hist-1".into(),
            source_runtime_id: RUNTIME_A.into(),
            recipient_runtime_id: RUNTIME_B.into(),
            source_principal_did: format!("user:hub:{ACCOUNT}"),
            channel_id: "dm_0123".into(),
            before: Some("msg-older".into()),
            after: Some("msg-newer".into()),
            limit: 100,
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelChannelHistory {
            request_id: "hist-1".into(),
            source_runtime_id: RUNTIME_B.into(),
            recipient_runtime_id: RUNTIME_A.into(),
            channel_id: "dm_0123".into(),
            events: vec![ChannelEvent::Posted {
                channel: ChannelId("dm_0123".into()),
                author: format!("user:hub:{ACCOUNT}"),
                parent: None,
                text: "hello again".into(),
                at: "2026-11-02T10:00:00Z".into(),
                via: None,
                msg_id: Some("msg-1".into()),
                reply_to: Some("msg-0".into()),
                attachments: vec![Attachment {
                    name: "notes.pdf".into(),
                    path: String::new(),
                    mime: "application/pdf".into(),
                    size: 2048,
                    sha256: Some(
                        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
                    ),
                }],
                options: vec!["Yes".into(), "Not yet".into()],
            }],
            has_more: true,
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelAttachmentRequest {
            request_id: "att-1".into(),
            source_runtime_id: RUNTIME_A.into(),
            recipient_runtime_id: RUNTIME_B.into(),
            channel_id: "dm_0123".into(),
            sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            offset: 262_144,
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelAttachmentChunk {
            request_id: "att-1".into(),
            source_runtime_id: RUNTIME_B.into(),
            recipient_runtime_id: RUNTIME_A.into(),
            channel_id: "dm_0123".into(),
            sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            offset: 262_144,
            size: 300_000,
            data: "dGVzdA==".into(),
        },
        TunnelMessage::TunnelAccountDmsRequest {
            request_id: "adms-1".into(),
            source_runtime_id: RUNTIME_A.into(),
            recipient_account: format!("user:hub:{ACCOUNT}"),
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelAccountDms {
            request_id: "adms-1".into(),
            source_runtime_id: RUNTIME_B.into(),
            recipient_runtime_id: RUNTIME_A.into(),
            dms: vec![AccountDm {
                channel_id: "dm_0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                peer_hub_id: HUB_ID.into(),
                peer_signing_did: "did:key:z6MkPrincipal".into(),
                peer_identity_key: "peko-key".into(),
                peer_name: Some("peko".into()),
                peer_runtime_id: RUNTIME_B.into(),
                peer_exposure: "private".into(),
            }],
            people: vec![AccountPersonDm {
                channel_id: "dm_1111111111111111111111111111111111111111111111111111111111111111"
                    .into(),
                peer: format!("user:hub:{HUB_ID}"),
                peer_name: Some("bob".into()),
                routes: vec![DmRoute {
                    runtime_id: RUNTIME_B.into(),
                    person: format!("user:hub:{ACCOUNT}"),
                }],
            }],
            blocks: vec![DmBlock {
                person: format!("user:hub:{HUB_ID}"),
                blocked: true,
                at: "2026-11-04T10:00:00+00:00".into(),
            }],
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelPersonDmInvite {
            request_id: "pdm-1".into(),
            source_runtime_id: RUNTIME_A.into(),
            recipient_account: format!("user:hub:{HUB_ID}"),
            source_principal_did: format!("user:hub:{ACCOUNT}"),
            channel_id: "dm_1111111111111111111111111111111111111111111111111111111111111111"
                .into(),
            name: Some("alice".into()),
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelDmRoute {
            request_id: "route-1".into(),
            source_runtime_id: RUNTIME_B.into(),
            recipient_runtime_id: RUNTIME_A.into(),
            source_principal_did: format!("user:hub:{HUB_ID}"),
            channel_id: "dm_1111111111111111111111111111111111111111111111111111111111111111"
                .into(),
            routes: vec![DmRoute {
                runtime_id: RUNTIME_B.into(),
                person: format!("user:hub:{HUB_ID}"),
            }],
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelGroupInvitation {
            request_id: "ginv-1".into(),
            source_runtime_id: RUNTIME_B.into(),
            recipient_account: format!("user:hub:{ACCOUNT}"),
            channel_id: "chan_0123abcd".into(),
            name: "book club".into(),
            inviter: format!("principal:hub:{HUB_ID}"),
            initial_members: vec![InitialMember {
                principal_did: "did:key:z6MkPrincipal".into(),
                hub_id: Some(HUB_ID.into()),
                identity_key: Some("3f0f6b1e-0000-4000-8000-000000000001".into()),
                name: Some("ada".into()),
                runtime_id: Some(RUNTIME_B.into()),
            }],
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelGroupJoin {
            request_id: "gjoin-1".into(),
            source_runtime_id: RUNTIME_A.into(),
            recipient_runtime_id: RUNTIME_B.into(),
            source_principal_did: format!("user:hub:{ACCOUNT}"),
            channel_id: "chan_0123abcd".into(),
            accept: true,
            signature: "runtime-signature".into(),
        },
        TunnelMessage::TunnelGroupJoinRequest {
            request_id: "gjreq-1".into(),
            source_runtime_id: RUNTIME_A.into(),
            recipient_runtime_id: RUNTIME_B.into(),
            source_principal_did: format!("user:hub:{ACCOUNT}"),
            channel_id: "chan_0123abcd".into(),
            invite: Some("home-signed-invite".into()),
            signature: "runtime-signature".into(),
        },
        TunnelMessage::GroupRegister {
            request_id: "greg-1".into(),
            channel_id: "chan_0123abcd".into(),
            name: "book club".into(),
            owner_address: format!("principal:hub:{HUB_ID}"),
            visibility: "invite_only".into(),
        },
        TunnelMessage::GroupRegistered {
            request_id: "greg-1".into(),
            group_id: "1234567890123456789".into(),
            page_url: "https://pekohub.ai/g/1234567890123456789".into(),
        },
        TunnelMessage::GroupRegisterRejected {
            request_id: "greg-2".into(),
            message: "this runtime is not registered".into(),
        },
        TunnelMessage::GroupRoster {
            group_id: "1234567890123456789".into(),
            accounts: vec![ACCOUNT.into()],
        },
        TunnelMessage::AccountGroupsRequest {
            request_id: "agroups-1".into(),
        },
        TunnelMessage::AccountGroups {
            request_id: "agroups-1".into(),
            groups: vec![AccountGroup {
                group_id: "1234567890123456789".into(),
                name: "book club".into(),
                home_runtime: RUNTIME_B.into(),
                channel_id: "chan_0123abcd".into(),
            }],
        },
        TunnelMessage::GroupLookupRequest {
            request_id: "glookup-1".into(),
            group_id: "1234567890123456789".into(),
            invite: Some("home-signed-invite".into()),
        },
        TunnelMessage::GroupLookup {
            request_id: "glookup-1".into(),
            group: Some(AccountGroup {
                group_id: "1234567890123456789".into(),
                name: "book club".into(),
                home_runtime: RUNTIME_B.into(),
                channel_id: "chan_0123abcd".into(),
            }),
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
