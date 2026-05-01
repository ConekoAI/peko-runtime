//! Core types for the Extension system
//!
//! This module defines the fundamental types used throughout the Extension
//! architecture. Submodules group related types by concern.

// Re-export all types to preserve backward compatibility
pub use self::async_types::{AsyncReceipt, AsyncTaskStatus};
pub use self::hook_io::{HookInput, HookOutput, HookResult, tool_result_from_hook};
pub use self::manifest::ExtensionManifest;
pub use self::session::{MessageEnvelope, PromptBuildState, SessionSnapshot, ToolRegistryAccess};
pub use self::tool::{ToolMetadata, ToolSource};

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

// Submodules
pub mod async_types;
pub mod hook_io;
pub mod manifest;
pub mod session;
pub mod tool;

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
