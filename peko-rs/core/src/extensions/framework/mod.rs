//! Shared host utilities used by workspace tooling adapters.
//!
//! ADR-066 retired the extension registration/lifecycle framework and its
//! contract crate. Tool dispatch lives in `crate::tools`; workspace observers
//! live in `crate::extensions::workspace_dispatcher`. This module retains
//! service handles, async transport, schema utilities, and host ports only.

/// Shared host service handles.
pub mod core;
/// Host path resolver port.
pub mod paths;
/// Schema filtering and validation utilities.
pub mod protocols;
/// Shared map utilities.
pub mod registry;
/// Async task routing and transport adapters.
pub mod transport;
/// Vault access port.
pub mod vault;
