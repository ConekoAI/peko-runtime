//! Common types shared across CLI and API
//!
//! This module provides data structures that represent entities
//! in the Peko system, used by both CLI commands and API routes.
//!
//! The `src/types/` directory was merged into this module in issue #31e.
//
// Phase 14.c.2a: `OutputFormat` lifted to
// `peko_principal::runtime::OutputFormat` (the principal layer owns it
// because every caller — slash dispatcher, IPC handlers, CLI send —
// composes it with principal-side data).

pub mod config;
pub mod extension;
pub mod principal_message;
pub mod task;
