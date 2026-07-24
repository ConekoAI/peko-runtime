//! Shared protocol helpers (Phase 8b).
//!
//! Lifted from `src/extensions/framework/protocols/shared/`. Only
//! `schema_filter.rs` moved in this phase because `tool_execution.rs`
//! needs `filter_reserved_params`. The remaining files
//! (`process_transport.rs`, `proxy_utils.rs`, `validation.rs`) move
//! in Phase 8c.

pub mod schema_filter;
