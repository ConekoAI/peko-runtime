//! Runtime-Pekohub Tunnel Protocol (ADR-035)
//!
//! Provides an outbound WebSocket tunnel from the runtime to PekoHub,
//! enabling remote access to locally-hosted agents behind NAT/firewall.

pub mod a2a_audit;
pub mod a2a_pending;
pub mod a2a_send_tool;
pub mod a2a_signature;
pub mod backoff;
pub mod client;
pub mod credential;
pub mod cross_runtime;
pub mod did_key;
pub mod dispatcher;
pub mod hub_directory;
pub mod known_runtimes;
pub mod protocol;

pub use a2a_pending::{A2aResponsePayload, A2aWaitError, PendingA2aResponses};
pub use a2a_signature::{sign_request, verify_request, SignedFields, A2A_SIGNATURE_DOMAIN};
pub use backoff::ExponentialBackoff;
pub use client::{TunnelClient, TunnelHandle, TunnelStatusUpdate, DEFAULT_MAX_RECONNECT_ATTEMPTS};
pub use credential::{load_pekohub_credential, PekoHubCredential};
pub use cross_runtime::CrossRuntimeA2aCtx;
pub use did_key::{did_key_to_verifying_key, verifying_key_to_did_key};
pub use dispatcher::TunnelDispatcher;
pub use hub_directory::{
    AgentDirectory, AgentResolution, DirectoryError, HubAgentDirectoryClient, ResolvedExposure,
};
pub use protocol::TunnelMessage;

// Issue #29 Slice E: end-to-end integration test exercising two
// runtimes + a synthetic hub forwarder. The forwarder sidesteps the
// pekohub forwarding dependency (pekohub#17, merged) so the test
// validates the runtime-side path end-to-end without needing a live
// hub. The forwarder's behavior is the same as pekohub's
// `tunnel-manager.ts::handleAgentToAgentRequest` — it routes the
// request to the target runtime's tunnel and returns the response
// back to the caller's tunnel.
#[cfg(test)]
mod a2a_e2e_tests;
