//! Shared protocol helpers (Phase 8b lift + Phase 8c.1.C completion).
//!
//! Phase 8c.1.C lifts the remaining three
//! files (`process_transport.rs`, `validation.rs`; `proxy_utils.rs` was
//! deleted with the legacy MCP runtime proxies)
//! so they no longer depend on root crate paths.

pub mod process_transport;
pub mod schema_filter;
pub mod validation;
