//! Shared runtime components for agent and daemon
//!
//! This module provides components that can be used by both the agent
//! and the daemon, enabling the daemon to execute tools without
//! instantiating a full agent.

pub mod identity;
pub mod metadata;
pub mod migration;
pub mod registry;
pub mod tool_runtime;

pub use identity::{did_key_to_public_key, public_key_to_did_key, EncryptedKey, RuntimeIdentity};
pub use metadata::{HostInfo, RuntimeMetadata};
pub use registry::{KnownRuntime, KnownRuntimes, TrustLevel};
pub use tool_runtime::{execute_tool_via_core, execute_tool_via_core_with_context, ToolRuntime};
