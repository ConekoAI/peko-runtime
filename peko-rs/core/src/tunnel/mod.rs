//! Runtime-Pekohub Tunnel Protocol (ADR-035)
//!
//! Provides an outbound WebSocket tunnel from the runtime to PekoHub,
//! enabling remote access to locally-hosted agents behind NAT/firewall.

pub mod a2a_signature;
pub mod backoff;
pub(crate) mod channel_outbox;
pub mod client;
#[cfg(test)]
mod contract;
pub mod credential;
pub mod cross_runtime;
pub mod cross_runtime_channel;
pub mod did_key;
pub mod dispatcher;
pub mod host;
pub mod hub_directory;
pub mod invite_token;
pub mod local_directory;
pub mod protocol;
pub mod registration;
pub mod tls;
pub mod tunnel_channel_audit;
pub mod tunnel_channel_port;
pub mod tunnel_channel_signature;

pub use a2a_signature::A2A_SIGNATURE_DOMAIN;
pub use backoff::ExponentialBackoff;
pub use client::{
    TunnelClient, TunnelHandle, TunnelStatusUpdate, DEFAULT_MAX_RECONNECT_ATTEMPTS,
    TUNNEL_OUTBOUND_BUFFER_SIZE,
};
pub use credential::{hub_origin, load_pekohub_credential, PekoHubCredential};
pub use cross_runtime::CrossRuntimeA2aCtx;
pub use cross_runtime_channel::CrossRuntimeChannelCtx;
pub use did_key::{did_key_to_verifying_key, verifying_key_to_did_key};
pub use dispatcher::TunnelDispatcher;
pub use host::{DmChannelInviteBootstrap, TunnelHost};
pub use hub_directory::{
    AgentDirectory, AgentResolution, DirectoryError, HubAgentDirectoryClient, ResolvedExposure,
};
pub use invite_token::{
    InviteClaims, InviteRevocationSet, InviteTokenError, MintedInvite, INVITE_TOKEN_DOMAIN,
    MAX_REVOKED_JTIS,
};
pub use local_directory::LocalFirstAgentDirectory;
pub use protocol::{
    AccountDm, AccountGroup, AccountPersonDm, DmBlock, DmRoute, PekoApproval, PekoModel,
    TunnelMessage,
};
pub use tunnel_channel_audit::{emit_forwarded_outbound, emit_received_inbound};
pub use tunnel_channel_port::{
    human_dm_channel_id, person_dm_channel_id, DmRequestAnswer, HistoryPage, HubDm, PersonDm,
    TunnelChannelPort, ATTACHMENT_CHUNK_BYTES, HISTORY_PAGE_LIMIT,
};
pub use tunnel_channel_signature::{
    jws_sign, jws_verify, sign_channel_event, sign_channel_invite, sign_history,
    verify_attachment_request, verify_author_signature, verify_channel_event,
    verify_channel_invite, verify_history, verify_history_request, AttachmentRequestPayload,
    ChannelEventPayload, ChannelHistoryPayload, ChannelHistoryRequestPayload, ChannelInvitePayload,
    ChannelInviteSignedFields, ChannelSignedFields, ATTACHMENT_REQUEST_VERSION,
    CHANNEL_EVENT_PAYLOAD_VERSION, CHANNEL_HISTORY_REQUEST_VERSION, CHANNEL_HISTORY_VERSION,
    CHANNEL_INVITE_PAYLOAD_VERSION,
};
pub use tunnel_channel_signature::{
    verify_account_dms, verify_account_dms_request, verify_dm_route, verify_person_dm_invite,
    AccountDmsPayload, AccountDmsRequestPayload, DmRoutePayload, PersonDmInvitePayload,
    ACCOUNT_DMS_REQUEST_VERSION, ACCOUNT_DMS_VERSION, DM_ROUTE_VERSION, PERSON_DM_INVITE_VERSION,
};
pub use tunnel_channel_signature::{
    verify_group_invitation, verify_group_join, verify_group_join_request, GroupInvitationPayload,
    GroupInviteToken, GroupJoinPayload, GroupJoinRequestPayload, GROUP_INVITATION_VERSION,
    GROUP_INVITE_TOKEN_VERSION, GROUP_JOIN_REQUEST_VERSION, GROUP_JOIN_VERSION,
};
pub use tunnel_channel_signature::{
    verify_peko_approval_decide, verify_peko_approval_decided, verify_peko_approvals,
    verify_peko_approvals_request, PekoApprovalDecidePayload, PekoApprovalDecidedPayload,
    PekoApprovalsPayload, PekoApprovalsRequestPayload, PEKO_APPROVALS_REQUEST_VERSION,
    PEKO_APPROVALS_VERSION, PEKO_APPROVAL_DECIDED_VERSION, PEKO_APPROVAL_DECIDE_VERSION,
};
pub use tunnel_channel_signature::{
    verify_peko_models, verify_peko_models_request, PekoModelsPayload, PekoModelsRequestPayload,
    PEKO_MODELS_REQUEST_VERSION, PEKO_MODELS_VERSION,
};
