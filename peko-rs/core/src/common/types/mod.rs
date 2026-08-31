//! Common types shared across CLI and API
//!
//! This module provides data structures that represent entities
//! in the Peko system, used by both CLI commands and API routes.
//!
//! The `src/types/` directory was merged into this module in issue #31e.
//
// Phase 14.c.2a lifted `OutputFormat` to
// `crate::principal::runtime::OutputFormat` for the slash dispatcher
// + IPC + CLI send. **Retired post-slash-removal**: the only consumer
// was the slash-command dispatch path, which has been removed.

pub mod config;
pub mod extension;
pub mod task;
