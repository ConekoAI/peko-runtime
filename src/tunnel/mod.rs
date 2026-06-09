//! Runtime-Pekohub Tunnel Protocol (ADR-035)
//!
//! Provides an outbound WebSocket tunnel from the runtime to PekoHub,
//! enabling remote access to locally-hosted agents behind NAT/firewall.

pub mod backoff;
pub mod client;
pub mod credential;
pub mod dispatcher;
pub mod protocol;

pub use backoff::ExponentialBackoff;
pub use client::{TunnelClient, TunnelHandle};
pub use credential::{load_pekohub_credential, PekoHubCredential};
pub use dispatcher::TunnelDispatcher;
pub use protocol::TunnelMessage;
