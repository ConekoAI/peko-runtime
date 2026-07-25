//! Integration bridges between the extension system and other frameworks
//!
//! This module provides adapters that connect extension hooks to external
//! framework traits.
//!
//! # Note
//!
//! The `ExtensionAsyncTool` wrapper (which implements the `Tool` trait for an
//! extension adapter) previously lived here but has been moved to
//! `tools::registry::extension_async_tool` per Issue 016. The generic extension
//! framework must not depend on `crate::tools` per ADR-017.
