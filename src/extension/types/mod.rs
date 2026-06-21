//! Core types for the Extension system
//!
//! This module defines the fundamental types used throughout the Extension
//! architecture. Submodules group related types by concern.

// Re-export all types to preserve backward compatibility
pub use self::async_types::AsyncReceipt;
pub use crate::extension::async_exec::executor::AsyncTaskStatus;
pub use self::hook_io::{tool_result_from_hook, HookInput, HookOutput, HookResult};
pub use self::manifest::{ExtensionDependency, ExtensionManifest};
pub use self::session::{MessageEnvelope, PromptBuildState, SessionSnapshot, ToolRegistryAccess};
pub use self::tool::{ToolMetadata, ToolSource};
pub use self::tool_exec::{
    AbortSignal, ToolContext, ToolContextAdapter, ToolError, ToolResult, ToolWithContext,
};

use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique identifier for an extension
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExtensionId(pub String);

impl ExtensionId {
    /// Create a new extension ID
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ExtensionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for ExtensionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Unique identifier for a hook registration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HookId(pub uuid::Uuid);

impl HookId {
    /// Generate a new unique hook ID
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for HookId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for HookId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Priority for hook handlers (higher = earlier)
pub type HookPriority = i32;

/// Default priority for handlers
pub const DEFAULT_HOOK_PRIORITY: HookPriority = 100;

/// Priority for system handlers (highest)
pub const SYSTEM_HOOK_PRIORITY: HookPriority = 1000;

/// Priority for user handlers (normal)
pub const USER_HOOK_PRIORITY: HookPriority = 100;

/// Priority for fallback handlers (lowest)
pub const FALLBACK_HOOK_PRIORITY: HookPriority = 0;

/// Runtime context fields for tool execution within the extension framework.
///
/// This is a framework-native struct that carries the subset of `ToolContext`
/// fields needed for reserved parameter resolution. It lives in `extension::types`
/// so the framework can construct and store it without depending on `tools::ToolContext`.
#[derive(Debug, Clone, Default)]
pub struct ToolRuntimeContext {
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub peer_id: Option<String>,
    pub workspace: Option<String>,
    pub run_id: Option<String>,
}

impl ToolRuntimeContext {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    #[must_use]
    pub fn with_peer_id(mut self, peer_id: impl Into<String>) -> Self {
        self.peer_id = Some(peer_id.into());
        self
    }

    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    #[must_use]
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }
}

// Submodules
pub mod async_types;
pub mod hook_io;
pub mod manifest;
pub mod session;
pub mod tool;
pub mod tool_exec;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_id() {
        let id = ExtensionId::new("test-skill");
        assert_eq!(id.0, "test-skill");
        assert_eq!(id.to_string(), "test-skill");
    }

    #[test]
    fn test_hook_id() {
        let id1 = HookId::new();
        let id2 = HookId::new();
        assert_ne!(id1.0, id2.0);
    }

    #[test]
    fn test_hook_priority_constants() {
        assert_eq!(DEFAULT_HOOK_PRIORITY, 100);
        assert_eq!(SYSTEM_HOOK_PRIORITY, 1000);
        assert_eq!(FALLBACK_HOOK_PRIORITY, 0);
    }
}
