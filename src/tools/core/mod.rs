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
//! `ContextSource` trait. Older paths (`extensions::framework::types::*` and
//! `extensions::framework::protocols::shared::context_resolver::ContextSource`)
//! re-export these names for one commit while consumers migrate.

pub mod context_source;
pub mod exec;
pub mod traits;

pub use context_source::ContextSource;
pub use exec::{AbortSignal, ToolContext, ToolContextAdapter, ToolError, ToolProgressEvent, ToolResult, ToolWithContext};
pub use traits::Tool;
