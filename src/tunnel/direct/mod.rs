//! Direct runtime-to-runtime transport (ADR-035 extension).
//!
//! This module implements an opt-in direct IP/port transport for
//! cross-runtime principal-to-principal communication. It is intended for
//! advanced users who control their own network topology and want to bypass
//! the PekoHub tunnel relay.
//!
//! The direct transport reuses the runtime's `did:key` identity, the A2A
//! request signature scheme (`a2a:v1`), and the existing inbound dispatcher.
//! It adds a minimal runtime-to-runtime identity handshake on top of TLS.

pub mod client;
pub mod handshake;
pub mod manager;
pub mod routing;
pub mod server;
pub mod tls;

pub use crate::tunnel::known_runtimes::{DirectTlsConfig, TransportPreference};
pub use client::{DirectClient, DirectConnection, DirectConnectionError};
pub use manager::DirectConnectionManager;
pub use routing::{select_transport, TransportChoice};
pub use server::{DirectMessageHandler, DirectServer};
pub use tls::{
    build_client_config, build_root_cert_store, build_server_config, load_cert_chain,
    load_private_key, PinningServerCertVerifier, TlsError,
};
