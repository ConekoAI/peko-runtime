//! Core tool infrastructure
//!
//! This module defines the fundamental types and traits for the tool system:
//! - `Tool` trait: the interface all tools implement
//! - `ToolContext`: execution context with abort signals and progress reporting
//! - `ToolError` / `ToolResult`: error and result types
//!
//! # Module Boundary Note
//!
//! `tools::core` is the canonical home for the execution primitives and the
//! `ContextSource` trait. There are no re-exports anywhere else in the crate.

pub mod context_source;
pub mod exec;
pub mod interrupt;
pub mod traits;

pub use context_source::ContextSource;
pub use exec::{
    bridge_from_cancellation_token, bridge_to_cancellation_token, AbortSignal,
    AbortSignalBridgeGuard, CancellationTokenBridgeGuard, ToolContext, ToolContextAdapter,
    ToolError, ToolProgressEvent, ToolResult, ToolWithContext,
};
pub use interrupt::ToolInterruptNotice;
pub use traits::Tool;
