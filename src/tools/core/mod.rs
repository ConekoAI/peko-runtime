//! Core tool infrastructure
//!
//! This module defines the fundamental types and traits for the tool system:
//! - `Tool` trait: the interface all tools implement
//! - `ToolContext`: execution context with abort signals and progress reporting
//! - `ToolError` / `ToolResult`: error and result types

pub mod context;
pub mod traits;

pub use context::{AbortSignal, ToolContext, ToolError, ToolWithContext};
pub use traits::{Tool, ToolResult};
