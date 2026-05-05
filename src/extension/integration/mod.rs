//! Integration bridges between the extension system and other frameworks
//!
//! This module provides adapters that connect extension hooks to external
//! framework traits (e.g., the [`Tool`] trait).

pub mod tool_bridge;

// Re-export for backward compatibility
pub use tool_bridge::ExtensionAsyncTool;
