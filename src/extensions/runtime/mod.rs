//! Extension Runtime — Backward Compatibility Re-exports
//!
//! This module is preserved during Phase 2 migration as a compatibility layer.
//! All runtime implementations have moved to `src/extensions/<type>/runtime/`.
//!
//! # New Locations
//! - `crate::extensions::mcp::runtime` — MCP runtime adapters and starters
//! - `crate::extensions::gateway::runtime` — Gateway runtime adapters and starters

// Re-export from new type-oriented locations for backward compatibility
pub use crate::extensions::gateway::runtime::*;
pub use crate::extensions::mcp::runtime::*;
