//! Cross-boundary protocol helpers (Phase 8b partial move).
//!
//! Only `shared::schema_filter` moved in this phase (its
//! `filter_reserved_params` is used by `services::tool_execution`).
//! `process_transport.rs`, `proxy_utils.rs`, `validation.rs` move in
//! Phase 8c.

pub mod shared;
