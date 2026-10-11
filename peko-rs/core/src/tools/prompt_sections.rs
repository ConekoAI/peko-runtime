//! Prompt-section providers (ADR-066 D2/D3).
//!
//! The built-in per-turn prompt sections (`identity`, `roles`,
//! `skills`, `workflows`, `session_context`) are plain providers
//! registered on the [`crate::tools::runtime::ToolingRuntime`]. Its prompt
//! rendering aggregates providers and workspace-hook `PromptSection` output.
//! Workspace hooks run in registration order; provider priority retains the
//! existing ordering of built-in session-context components.
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

/// Join catalog `lines` with newlines within `max_bytes`. On overflow the
/// remaining lines are dropped and `notice` is appended, so a dropped entry
/// is never silent; room for the notice is reserved while lines remain, so
/// the result stays within `max_bytes` (a single first line longer than the
/// cap is kept whole).
#[must_use]
pub fn render_capped_lines(lines: &[String], max_bytes: usize, notice: &str) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let reserve = if i + 1 < lines.len() {
            notice.len() + 1
        } else {
            0
        };
        if !out.is_empty() && out.len() + 1 + line.len() + reserve > max_bytes {
            out.push('\n');
            out.push_str(notice);
            return out;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
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

#[cfg(test)]
mod capped_lines_tests {
    use super::render_capped_lines;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    /// Exact boundaries: everything that fits is kept; the first line that
    /// doesn't is replaced by the notice, never dropped silently, and the
    /// result stays within the cap.
    #[test]
    fn keeps_what_fits_and_marks_what_does_not() {
        assert_eq!(
            render_capped_lines(&lines(&["aaa", "bbb"]), 7, "N"),
            "aaa\nbbb"
        );
        assert_eq!(
            render_capped_lines(&lines(&["aaa", "bbb"]), 6, "N"),
            "aaa\nN"
        );
        // A middle line also needs room for the notice that may follow.
        let three = lines(&["aa", "bb", "cc"]);
        assert_eq!(render_capped_lines(&three, 8, "NN"), "aa\nbb\ncc");
        assert_eq!(render_capped_lines(&three, 7, "NN"), "aa\nNN");
        assert_eq!(render_capped_lines(&[], 10, "NN"), "");
        for max in 5..12 {
            let out = render_capped_lines(&three, max, "NN");
            assert!(out.len() <= max, "{max}: {out:?}");
        }
    }
}
