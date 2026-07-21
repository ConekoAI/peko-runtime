//! Per-tool interrupt hook point and structured cancel notice.
//!
//! This module lives next to [`Tool`](crate::tools::Tool) and
//! [`ToolContext`](crate::tools::ToolContext) but is kept in its own file so
//! the core trait surface stays small. It provides:
//!
//! - [`ToolInterruptNotice`]: a structured record describing the consequences
//!   of a cancelled tool call (what was preserved, rolled back, leaked, and a
//!   resume hint for the calling agent).
//!
//! The hook point itself lives on the [`Tool`] trait as
//! [`Tool::on_interrupt`](crate::tools::Tool::on_interrupt). Every tool
//! inherits a soft-path default that emits a minimal notice; individual tools
//! override the method to describe their own side-effects.

/// Structured notice emitted by the framework when a tool call is cancelled.
///
/// Custom tools override [`Tool::on_interrupt`](crate::tools::Tool::on_interrupt)
/// to fill in the consequence fields. The soft default (provided by the trait
/// default) leaves `preserved`, `rolled_back`, and `leaked` empty and supplies
/// a generic resume hint.
#[derive(Debug, Clone)]
pub struct ToolInterruptNotice {
    /// Identifier of the cancelled tool call. The framework populates this
    /// when it is known; custom tools may override it.
    pub tool_call_id: String,
    /// Name of the tool that was cancelled.
    pub tool_name: String,
    /// Side-effects that were durably committed before the cancel.
    pub preserved: Vec<String>,
    /// Side-effects that were rolled back by the tool.
    pub rolled_back: Vec<String>,
    /// Side-effects left in an indeterminate state.
    pub leaked: Vec<String>,
    /// Optional hint the calling agent should consider on the next turn
    /// (e.g. "re-read file before retrying — partial write possible").
    pub resume_hint: Option<String>,
}

impl ToolInterruptNotice {
    /// Construct the canonical minimal notice for a soft-default tool.
    #[must_use]
    pub fn soft_default(tool_call_id: impl Into<String>, tool_name: impl Into<String>) -> Self {
        let tool_name = tool_name.into();
        Self {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.clone(),
            preserved: Vec::new(),
            rolled_back: Vec::new(),
            leaked: Vec::new(),
            resume_hint: Some(format!(
                "{tool_name} call was cancelled by user. The tool will be observed \
                 at its next polling boundary; if it completed, the response above \
                 reflects completion."
            )),
        }
    }

    /// Format for injection into a tool-result content block.
    ///
    /// The rendered text becomes the body of the `ToolResult` message the
    /// calling agent sees on its next turn, replacing the tool's natural
    /// output when the cancel wins.
    #[must_use]
    pub fn to_tool_result_text(&self) -> String {
        let mut out = if self.tool_call_id.is_empty() {
            format!("[{} call was CANCELLED]", self.tool_name)
        } else {
            format!(
                "[{} call {} was CANCELLED]",
                self.tool_name, self.tool_call_id
            )
        };
        if !self.preserved.is_empty() {
            out.push_str("\nPreserved: ");
            out.push_str(&self.preserved.join("; "));
        }
        if !self.rolled_back.is_empty() {
            out.push_str("\nRolled back: ");
            out.push_str(&self.rolled_back.join("; "));
        }
        if !self.leaked.is_empty() {
            out.push_str("\nLeaked (indeterminate): ");
            out.push_str(&self.leaked.join("; "));
        }
        if let Some(hint) = &self.resume_hint {
            out.push_str(&format!("\nHint: {hint}"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `Tool` and `ToolContext` are re-exported at the crate root via
    // `lib.rs`. Tests inside the crate use `crate::Tool` /
    // `crate::ToolContext` rather than the historical
    // `crate::tools::Tool` facade (which lives one layer up in the
    // root `peko` package).
    use crate::{Tool, ToolContext};
    use serde_json::json;

    struct SoftTool {
        name: String,
    }

    #[async_trait::async_trait]
    impl Tool for SoftTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> String {
            "soft path tool".to_string()
        }

        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(json!({"ok": true}))
        }
    }

    struct EnrichingTool;

    #[async_trait::async_trait]
    impl Tool for EnrichingTool {
        fn name(&self) -> &str {
            "Enriching"
        }

        fn description(&self) -> String {
            "enriches cancel notice".to_string()
        }

        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(json!({"ok": true}))
        }

        async fn on_interrupt(
            &self,
            tool_call_id: &str,
            _ctx: &ToolContext,
        ) -> ToolInterruptNotice {
            ToolInterruptNotice {
                tool_call_id: tool_call_id.to_string(),
                tool_name: self.name().to_string(),
                preserved: vec!["committed row 7".to_string()],
                rolled_back: vec!["temp_file.dat".to_string()],
                leaked: vec![],
                resume_hint: Some("Safe to retry.".to_string()),
            }
        }
    }

    #[test]
    fn soft_default_notice_carries_call_id_and_name() {
        let notice = ToolInterruptNotice::soft_default("call_1", "Bash");
        assert_eq!(notice.tool_call_id, "call_1");
        assert_eq!(notice.tool_name, "Bash");
        assert!(notice.preserved.is_empty());
        assert!(notice.rolled_back.is_empty());
        assert!(notice.leaked.is_empty());
        assert!(notice.resume_hint.is_some());
    }

    #[tokio::test]
    async fn trait_default_provides_minimal_notice_for_soft_path_tool() {
        let tool = SoftTool {
            name: "Soft".to_string(),
        };
        let ctx = ToolContext::default_for_tool("Soft");
        let notice = tool.on_interrupt("id-1", &ctx).await;
        assert_eq!(notice.tool_call_id, "id-1");
        assert_eq!(notice.tool_name, "Soft");
        assert!(notice.preserved.is_empty());
        assert!(notice.rolled_back.is_empty());
        assert!(notice.leaked.is_empty());
        assert!(notice.resume_hint.is_some());
    }

    #[tokio::test]
    async fn custom_tool_can_enrich_notice() {
        let tool = EnrichingTool;
        let ctx = ToolContext::default_for_tool("Enriching");
        let notice = tool.on_interrupt("id-2", &ctx).await;
        assert!(notice.rolled_back.contains(&"temp_file.dat".to_string()));
        assert!(notice.preserved.contains(&"committed row 7".to_string()));
        assert_eq!(notice.resume_hint.as_deref(), Some("Safe to retry."));
    }

    #[test]
    fn notice_to_tool_result_text_includes_consequences() {
        let notice = ToolInterruptNotice {
            tool_call_id: "call_42".to_string(),
            tool_name: "Edit".to_string(),
            preserved: vec!["db row 42".to_string()],
            rolled_back: vec!["stage buffer".to_string()],
            leaked: vec!["network socket".to_string()],
            resume_hint: Some("retry-safe".to_string()),
        };
        let text = notice.to_tool_result_text();
        assert!(text.contains("[Edit call call_42 was CANCELLED]"));
        assert!(text.contains("Preserved: db row 42"));
        assert!(text.contains("Rolled back: stage buffer"));
        assert!(text.contains("Leaked (indeterminate): network socket"));
        assert!(text.contains("Hint: retry-safe"));
    }

    #[test]
    fn notice_without_call_id_omits_id_from_rendering() {
        let notice = ToolInterruptNotice::soft_default("", "Read");
        let text = notice.to_tool_result_text();
        assert!(text.starts_with("[Read call was CANCELLED]"));
    }
}
