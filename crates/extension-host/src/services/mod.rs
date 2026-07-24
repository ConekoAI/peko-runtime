//! Extension services (Phase 8b partial move).
//!
//! Only `tool_execution.rs` and `reserved_params.rs` moved in this
//! phase because `transport::async_router` depends on them. The
//! remaining service (`config_service.rs`) moves in Phase 8c.

pub mod reserved_params;
pub mod tool_execution;

pub use reserved_params::{ParamSource, ReservedParamsConfig, ReservedParamsService};
pub use tool_execution::{ToolExecutionConfig, ToolExecutionService};
// `ToolExecutionContext` was promoted to live alongside the router in
// `transport::async_router` (mirrors how the trait port calls it);
// re-exported here for backwards compat with the historical
// `services::ToolExecutionContext` import path.
pub use crate::transport::async_router::ToolExecutionContext;
