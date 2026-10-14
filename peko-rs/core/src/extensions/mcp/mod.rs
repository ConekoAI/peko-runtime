//! Principal-owned MCP service (ADR-075).
//!
//! `service` drives native workspace discovery, connections, OAuth and scoped
//! calls. `protocol` holds the hand-rolled JSON-RPC client, SSE transport,
//! sampling handler, and OAuth flow that `service` uses for legacy SSE peers.

pub mod protocol;
pub mod service;
