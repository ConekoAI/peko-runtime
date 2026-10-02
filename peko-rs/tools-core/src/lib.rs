//! Tool execution API — the canonical home for `Tool` plus its
//! abort/cancellation, context, progress, and result primitives.
//!
//! Every extension built-in or external implements the [`Tool`] trait
//! defined here. Tool wiring (registration, capability gate, hook
//! dispatch) lives in `peko-extension-host`, not in this crate, so
//! `peko-tools-core` stays a domain types layer with no inbound
//! dependency on the framework host or any concrete extension
//! implementation.
//!
//! ## Module map
//!
//! - [`traits::Tool`] — the trait every tool implements.
//! - [`exec::ToolContext`], [`exec::AbortSignal`],
//!   [`exec::ToolProgressEvent`] — execution context, abort mechanism,
//!   and progress reporting.
//! - [`exec::ToolResult`], [`exec::ToolError`] — typed result / error.
//! - [`exec::ToolWithContext`], [`exec::ToolContextAdapter`] — adapter
//!   that bridges a raw `Tool` into the context-aware framework.
//! - [`interrupt::ToolInterruptNotice`] — structured cancel notice.
//! - [`context_source::ContextSource`] — unified context resolver.

pub mod constants;
pub mod context_source;
pub mod exec;
pub mod interrupt;
pub mod traits;

pub use constants::HOOK_TIMEOUT;
pub use context_source::{ContextResolver, ContextSource};
pub use exec::{
    bridge_from_cancellation_token, bridge_to_cancellation_token, AbortSignal,
    AbortSignalBridgeGuard, CancellationTokenBridgeGuard, ToolContext, ToolContextAdapter,
    ToolError, ToolProgressEvent, ToolResult, ToolWithContext,
};
pub use interrupt::ToolInterruptNotice;
pub use traits::Tool;
