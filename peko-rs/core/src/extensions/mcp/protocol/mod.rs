//! MCP Protocol Module
//!
//! Hand-rolled JSON-RPC client, SSE transport, sampling handler, OAuth flow,
//! and wire types used by the principal-owned service (ADR-075) for legacy
//! SSE peers.

pub mod client;
pub mod config;
pub mod oauth;
pub mod sampling;
pub mod transport;
pub mod types;

// Re-export common types for convenience
pub use client::{ClientError, McpClient};
pub use config::McpAuthConfig;
pub use sampling::SamplingRequestHandler;
pub use transport::{InMemoryTransport, McpTransport, SseTransport, TransportError};
pub use types::*;
