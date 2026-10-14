//! Tool execution API — the canonical home for `Tool` plus its
//! abort/cancellation, context, progress, and result primitives.
//!
//! Every extension built-in or external implements the [`Tool`] trait
//! defined here. Registration, hook dispatch, and execution wiring live in
//! core's tools domain. This crate stays a leaf contract layer, with no
//! dependency on the tooling runtime or concrete tool implementations.
//!
//! ## Module map
//!
//! - [`traits::Tool`] — the trait every tool implements.
//! - [`exec::ToolContext`], [`exec::AbortSignal`],
//!   [`exec::ToolProgressEvent`] — execution context, abort mechanism,
//!   and progress reporting.
//! - [`exec::ToolResult`], [`exec::ToolError`] — typed result / error.
//! - [`exec::ToolWithContext`] — marker trait for context-aware tools.
//! - [`interrupt::ToolInterruptNotice`] — structured cancel notice.
//! - [`acting_for::ActingFor`] — the runtime stamp of whom a run acts for.

pub mod acting_for;
pub mod constants;
pub mod exec;
pub mod interrupt;
pub mod schema;
pub mod traits;

pub use acting_for::{neutralize_runtime_tags, ActingFor, Compartment, Origin, Trust};
pub use constants::HOOK_TIMEOUT;
pub use exec::{
    bridge_from_cancellation_token, bridge_to_cancellation_token, AbortSignal,
    AbortSignalBridgeGuard, CancellationTokenBridgeGuard, ToolContext, ToolError,
    ToolProgressEvent, ToolResult, ToolWithContext,
};
pub use interrupt::ToolInterruptNotice;
pub use traits::Tool;

pub mod async_status;
pub mod background;
pub use background::{BackgroundContext, BackgroundSpawn, BackgroundSpawner};
pub mod paths;
pub use async_status::{AsyncTaskId, AsyncTaskResult, AsyncTaskStatus};
pub use paths::{default_agent_workspace, default_data_dir};
