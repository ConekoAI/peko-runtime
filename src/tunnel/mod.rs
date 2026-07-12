//! Runtime-Pekohub Tunnel Protocol (ADR-035)
//!
//! Provides an outbound WebSocket tunnel from the runtime to PekoHub,
//! enabling remote access to locally-hosted agents behind NAT/firewall.

pub mod a2a_audit;
pub mod a2a_pending;
pub mod a2a_signature;
pub mod backoff;
pub mod client;
pub mod credential;
pub mod cross_runtime;
pub mod did_key;
pub mod direct;
pub mod dispatcher;
pub mod host;
pub mod hub_directory;
pub mod known_runtimes;
pub mod local_directory;
pub mod principal_send_tool;
pub mod protocol;

pub use a2a_pending::{A2aResponsePayload, A2aWaitError, PendingA2aResponses};
pub use a2a_signature::{sign_request, verify_request, SignedFields, A2A_SIGNATURE_DOMAIN};
pub use backoff::ExponentialBackoff;
pub use client::{
    TunnelClient, TunnelHandle, TunnelStatusUpdate, DEFAULT_MAX_RECONNECT_ATTEMPTS,
    TUNNEL_OUTBOUND_BUFFER_SIZE,
};
pub use credential::{load_pekohub_credential, PekoHubCredential};
pub use cross_runtime::CrossRuntimeA2aCtx;
pub use did_key::{did_key_to_verifying_key, verifying_key_to_did_key};
pub use dispatcher::TunnelDispatcher;
pub use host::TunnelHost;
pub use hub_directory::{
    AgentDirectory, AgentResolution, DirectoryError, HubAgentDirectoryClient, ResolvedExposure,
};
pub use local_directory::LocalFirstAgentDirectory;
pub use principal_send_tool::{PrincipalSendArgs, PrincipalSendResult, PrincipalSendTool};
pub use protocol::TunnelMessage;
