//! View-model for `ResponsePacket::RunSummary` + footer formatting.
//!
//! The CLI's `peko send` captures a single `RunSummary` packet per run
//! (between `PrincipalSent*` and `Done`) and prints a one-line footer
//! to stderr at end-of-run. The footer is the
//! opt-in-by-default surface for cumulative token usage and any
//! tool errors that the daemon otherwise drops per ADR-042's
//! "backend-only by default" rule.
//!
//! Kept as a thin view-model rather than reaching into
//! `peko_core::ipc::packet::RunUsageSummary` / `ToolErrorEntry`
//! directly so the CLI doesn't gain a structural dependency on
//! daemon-side wire types beyond what `ResponsePacket` already
//! exposes.

/// Mirror of `peko_core::ipc::packet::RunUsageSummary`.
#[derive(Debug, Clone, Default)]
pub struct UsageView {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Mirror of `peko_core::ipc::packet::ToolErrorEntry` (we keep the
/// fields the footer actually needs and drop `tool_id`; the
/// daemon-side name lookup is good enough for human-readable output).
#[derive(Debug, Clone, Default)]
pub struct ToolErrorView {
    pub tool_name: Option<String>,
    pub error_message: String,
}

/// CLI-side view of `ResponsePacket::RunSummary`.
#[derive(Debug, Clone, Default)]
pub struct RunSummaryView {
    pub iterations: u32,
    pub usage: Option<UsageView>,
    pub tool_errors: Vec<ToolErrorView>,
}

impl RunSummaryView {
    /// Render the one-line footer that `peko send` prints to stderr.
    /// Format:
    ///
    /// ```text
    /// [peko] iterations=5 input=1234 output=567 cache=… tools_failed=2 [errors: read_file: …; bash: …]
    /// ```
    ///
    /// - `input` / `output` come from `usage.prompt_tokens` /
    ///   `usage.completion_tokens`. `total` is omitted (it's just
    ///   `input + output` for the local count). `cache` is reserved
    ///   for a future widening of `AgenticEvent::Usage` — today it's
    ///   always `0` so the field is hidden when zero.
    /// - `tools_failed` is `tool_errors.len()`. The `[errors: …]`
    ///   suffix is only appended when at least one tool failed.
    /// - Each error is truncated to ~60 chars and joined with `; `.
    ///   If the joined list exceeds 200 chars total, it's truncated
    ///   and suffixed with `…` so the footer stays on one line.
    #[must_use]
    pub fn format_footer(&self) -> String {
        let mut s = String::from("[peko] ");
        s.push_str(&format!("iterations={}", self.iterations));
        if let Some(usage) = &self.usage {
            s.push_str(&format!(
                " input={} output={} total={}",
                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
            ));
        }
        let failed = self.tool_errors.len();
        s.push_str(&format!(" tools_failed={failed}"));
        if failed > 0 {
            let errs: Vec<String> = self
                .tool_errors
                .iter()
                .map(|e| {
                    let name = e.tool_name.as_deref().unwrap_or("(unknown)");
                    // Truncate the message so one tool's noisy error
                    // doesn't blow out the whole line.
                    let msg: String = e.error_message.chars().take(60).collect();
                    if msg.len() < e.error_message.len() {
                        format!("{name}: {msg}…")
                    } else {
                        format!("{name}: {msg}")
                    }
                })
                .collect();
            let joined = errs.join("; ");
            if joined.len() > 200 {
                let truncated: String = joined.chars().take(197).collect();
                s.push_str(&format!(" [errors: {truncated}…]"));
            } else {
                s.push_str(&format!(" [errors: {joined}]"));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_summary_renders_minimal_footer() {
        let s = RunSummaryView::default();
        assert_eq!(s.format_footer(), "[peko] iterations=0 tools_failed=0");
    }

    #[test]
    fn usage_renders_input_output_total() {
        let s = RunSummaryView {
            iterations: 3,
            usage: Some(UsageView {
                prompt_tokens: 1234,
                completion_tokens: 567,
                total_tokens: 1801,
            }),
            tool_errors: Vec::new(),
        };
        let f = s.format_footer();
        assert!(f.contains("iterations=3"));
        assert!(f.contains("input=1234"));
        assert!(f.contains("output=567"));
        assert!(f.contains("total=1801"));
        assert!(f.contains("tools_failed=0"));
    }

    #[test]
    fn tool_errors_are_listed_and_truncated() {
        let s = RunSummaryView {
            iterations: 2,
            usage: None,
            tool_errors: vec![
                ToolErrorView {
                    tool_name: Some("read_file".into()),
                    error_message: "ENOENT: /tmp/missing".into(),
                },
                ToolErrorView {
                    tool_name: Some("bash".into()),
                    error_message: "command failed with very long output that should be truncated "
                        .repeat(20),
                },
            ],
        };
        let f = s.format_footer();
        assert!(f.contains("tools_failed=2"));
        assert!(f.contains("read_file: ENOENT: /tmp/missing"));
        // Long message gets a `…` truncation marker.
        assert!(f.contains("bash: "));
        assert!(f.contains("…"));
        // Footer stays a single line.
        assert!(!f.contains('\n'));
    }

    #[test]
    fn unknown_tool_name_renders_unknown_marker() {
        let s = RunSummaryView {
            iterations: 1,
            usage: None,
            tool_errors: vec![ToolErrorView {
                tool_name: None,
                error_message: "kaboom".into(),
            }],
        };
        let f = s.format_footer();
        assert!(f.contains("(unknown): kaboom"));
    }
}