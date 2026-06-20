//! Runtime-Pekohub Tunnel Protocol (ADR-035)
//!
//! Provides an outbound WebSocket tunnel from the runtime to PekoHub,
//! enabling remote access to locally-hosted agents behind NAT/firewall.

pub mod a2a_pending;
pub mod a2a_signature;
pub mod backoff;
pub mod client;
pub mod credential;
pub mod dispatcher;
pub mod hub_directory;
pub mod protocol;

pub use a2a_pending::{A2aResponsePayload, A2aWaitError, PendingA2aResponses};
pub use backoff::ExponentialBackoff;
pub use client::{TunnelClient, TunnelHandle, TunnelStatusUpdate, DEFAULT_MAX_RECONNECT_ATTEMPTS};
pub use credential::{load_pekohub_credential, PekoHubCredential};
pub use dispatcher::TunnelDispatcher;
pub use hub_directory::{
    AgentDirectory, AgentResolution, DirectoryError, HubAgentDirectoryClient, ResolvedExposure,
};
pub use protocol::TunnelMessage;
