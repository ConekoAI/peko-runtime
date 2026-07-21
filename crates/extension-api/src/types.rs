//! Core extension-framework identity and runtime-context types
//!
//! Lifted from `src/extensions/framework/types/mod.rs` in Phase 7. Owns
//! the stable IDs (`ExtensionId`, `HookId`), hook priority constants,
//! and the framework-native `ToolRuntimeContext` that extensions thread
//! through tool execution so they can resolve per-principal state and
//! bridge the engine's `CancellationToken` into the tool layer.

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
/// fields needed for reserved parameter resolution. It lives in the
/// extension-API crate so the framework can construct and store it without
/// depending on the concrete tool-execution crate.
#[derive(Debug, Clone, Default)]
pub struct ToolRuntimeContext {
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub peer_id: Option<String>,
    pub workspace: Option<String>,
    pub run_id: Option<String>,
    pub principal_id: Option<String>,
    pub principal_name: Option<String>,
    /// Soft-interrupt abort signal receiver. Plumbed from the engine's
    /// `CancellationToken` (PR #128) via
    /// [`bridge_from_cancellation_token`] in `peko_tools_core::exec`.
    /// When `Some`, the tool layer's `is_aborted()` check is meaningful
    /// in production; `None` for hooks fired outside a tool execution
    /// (prompt-build, async status checks) and for legacy callers that
    /// haven't been migrated to thread a token through.
    pub abort_signal: Option<tokio::sync::watch::Receiver<bool>>,
    /// Principal capability grants carried with the tool call. Used by
    /// extension-scoped tools (e.g. `Skill`) and prompt handlers to
    /// decide whether a skill/agent is visible without consulting a
    /// per-principal global registry.
    pub capabilities: Option<Vec<String>>,
    /// IDs of extensions that are active for the current principal.
    /// Prompt handlers filter section entries by membership in this set.
    pub active_extensions: Option<Vec<String>>,
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

    #[must_use]
    pub fn with_principal_id(mut self, principal_id: impl Into<String>) -> Self {
        self.principal_id = Some(principal_id.into());
        self
    }

    #[must_use]
    pub fn with_principal_name(mut self, principal_name: impl Into<String>) -> Self {
        self.principal_name = Some(principal_name.into());
        self
    }

    /// Bridge the engine's `CancellationToken` into the tool layer by
    /// supplying the `watch::Receiver<bool>` half of an `AbortSignal`.
    #[must_use]
    pub fn with_abort_signal(mut self, abort_signal: tokio::sync::watch::Receiver<bool>) -> Self {
        self.abort_signal = Some(abort_signal);
        self
    }

    /// Bridge the principal's capability grants into the runtime context.
    #[must_use]
    pub fn with_capabilities(
        mut self,
        capabilities: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.capabilities = Some(capabilities.into_iter().map(Into::into).collect());
        self
    }

    /// Bridge the active extension snapshot into the runtime context.
    #[must_use]
    pub fn with_active_extensions(
        mut self,
        active_extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.active_extensions = Some(active_extensions.into_iter().map(Into::into).collect());
        self
    }
}

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
