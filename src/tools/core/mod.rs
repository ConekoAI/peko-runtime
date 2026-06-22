//! Core tool infrastructure
//!
//! This module defines the fundamental types and traits for the tool system:
//! - `Tool` trait: the interface all tools implement
//! - `ToolContext`: execution context with abort signals and progress reporting
//! - `ToolError` / `ToolResult`: error and result types
//!
//! # Module Boundary Note
//!
//! Execution primitives (`ToolContext`, `ToolError`, `AbortSignal`, `ToolResult`,
//! `ToolWithContext`) have been moved to `extension::types::tool_exec` so the
//! generic extension framework can use them without depending on `crate::tools`.
//! This module re-exports them for convenience.

pub mod traits;

pub use crate::extensions::framework::types::{
    AbortSignal, ToolContext, ToolContextAdapter, ToolError, ToolResult, ToolWithContext,
};
pub use traits::Tool;
