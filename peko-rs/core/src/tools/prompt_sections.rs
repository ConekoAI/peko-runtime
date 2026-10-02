//! Prompt-section providers (ADR-066 D2/D3).
//!
//! The built-in per-turn prompt sections (`identity`, `roles`,
//! `skills`, `workflows`, `session_context`) are plain providers
//! registered on the [`crate::tools::runtime::ToolingRuntime`] — no
//! `HookHandler` impls, no priority dispatch through the hook
//! registry. The funnel's `render_prompt_sections` aggregates them;
//! workspace-hook `PromptSection` binds (`<workspace>/hooks/`) still
//! ride the hook registry inside the same aggregation until P4 unifies
//! them.
//!
//! A provider's file-walking / mtime-cache internals are its own
//! business — this module only defines the seam.

use std::path::PathBuf;
use std::sync::Arc;

/// Everything a provider needs to render its section for one turn.
#[derive(Clone)]
pub struct PromptSectionInput {
    /// Owning principal id (empty when unattributed — providers treat
    /// that as "no principal scope").
    pub principal_id: String,
    /// The principal's workspace path.
    pub workspace: PathBuf,
    /// The running session's id (session-context providers key off it;
    /// empty for the system-prompt render).
    pub session_id: String,
    /// The daemon's channel port (channel-digest / peer-list providers
    /// read channel state through it; `None` in tests).
    pub channel_port: Option<Arc<dyn peko_channel::ChannelPort>>,
}

/// A named prompt-section provider.
///
/// `priority` only orders same-section aggregation (the
/// `session_context` section has three providers): higher renders
/// first, matching the pre-P3 hook-registry order (highest priority
/// first).
#[async_trait::async_trait]
pub trait PromptSectionProvider: Send + Sync {
    /// The section this provider renders (`roles`, `skills`,
    /// `identity`, `workflows`, `session_context`, …).
    fn section(&self) -> &'static str;

    /// Aggregation order within a section (higher first). Default 100.
    fn priority(&self) -> i32 {
        100
    }

    /// Render the section; `None` means "no content this turn" (the
    /// section is omitted).
    async fn render(&self, ctx: &PromptSectionInput) -> Option<String>;
}
