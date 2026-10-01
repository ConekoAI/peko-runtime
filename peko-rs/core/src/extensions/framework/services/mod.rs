//! Extension services (Phase 8b).
//!
//! `reserved_params.rs` lives here because the MCP adapter
//! (`mcp::protocol::{config, manager}`, `mcp::runtime::injectable_proxy`)
//! consumes it. ADR-066 P1 deleted `config_service.rs`,
//! `tool_execution.rs`, and the `Services` orchestrator struct — zero
//! production consumers.

pub mod reserved_params;

pub use reserved_params::{ParamSource, ReservedParamsConfig, ReservedParamsService};
// `ToolExecutionContext` was promoted to live alongside the router in
// `transport::async_router` (mirrors how the trait port calls it);
// re-exported here for backwards compat with the historical
// `services::ToolExecutionContext` import path.
pub use crate::extensions::framework::transport::async_router::ToolExecutionContext;
