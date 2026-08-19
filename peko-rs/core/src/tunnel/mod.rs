//! Runtime-Pekohub Tunnel Protocol (ADR-035)
//!
//! Provides an outbound WebSocket tunnel from the runtime to PekoHub,
//! enabling remote access to locally-hosted agents behind NAT/firewall.

pub mod a2a_signature;
pub mod backoff;
pub mod client;
pub mod credential;
pub mod cross_runtime;
pub mod cross_runtime_channel;
pub mod did_key;
pub mod dispatcher;
pub mod host;
pub mod hub_directory;
pub mod invite_token;
pub mod known_runtimes;
pub mod local_directory;
pub mod principal_send_tool;
pub mod protocol;
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
pub use credential::{load_pekohub_credential, PekoHubCredential};
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
pub use principal_send_tool::{PrincipalSendResult, SendPeerArgs, SendPeerTool};
pub use protocol::TunnelMessage;
pub use tunnel_channel_audit::{emit_forwarded_outbound, emit_received_inbound};
pub use tunnel_channel_port::TunnelChannelPort;
pub use tunnel_channel_signature::{
    sign_channel_event, verify_channel_event, ChannelSignedFields, CHANNEL_SIGNATURE_DOMAIN,
    sign_channel_invite, verify_channel_invite, ChannelInviteSignedFields,
    CHANNEL_INVITE_SIGNATURE_DOMAIN,
};
