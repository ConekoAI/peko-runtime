//! Structured Summary Format and File Operation Tracking
//!
//! Provides the proven pi-mono inspired summary format with:
//! - Goal, Constraints, Progress (Done/In Progress/Blocked)
//! - Key Decisions, Next Steps, Critical Context
//! - File operation tracking (read_files, modified_files)

use serde::{Deserialize, Serialize};

use crate::compaction::types::CompactionPhase;

/// Details tracked across compactions for cumulative file operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionDetails {
    /// Files that were read (via tool calls)
    pub read_files: Vec<String>,
    /// Files that were modified (via tool calls)
    pub modified_files: Vec<String>,
    /// Approximate token cost of images summarised in this entry
    /// (sum of `estimate_image_tokens` across all `ContentBlock::Image`
    /// blocks at the time of compaction). Lets post-hoc analysis see
    /// how much of the context-window budget images were consuming.
    /// `#[serde(default)]` keeps pre-PR-1 JSONL entries compatible
    /// (missing field reads as 0).
    #[serde(default)]
    pub image_token_count: usize,
    /// Which point in the agentic loop fired this compaction. PR 3
    /// stamps the phase onto details so audit hooks can distinguish
    /// pre-turn from mid-turn summaries. Defaults to
    /// [`CompactionPhase::PreTurn`] for backwards compatibility with
    /// pre-PR-3 JSONL (the only kind of compaction that existed
    /// before PR 3).
    #[serde(default)]
    pub phase: CompactionPhase,
}

impl Default for CompactionDetails {
    fn default() -> Self {
        Self {
            read_files: Vec::new(),
            modified_files: Vec::new(),
            image_token_count: 0,
            phase: CompactionPhase::PreTurn,
        }
    }
}

impl CompactionDetails {
    /// Merge another details set into this one, deduplicating.
    pub fn merge(&mut self, other: &CompactionDetails) {
        for f in &other.read_files {
            if !self.read_files.contains(f) {
                self.read_files.push(f.clone());
            }
        }
        for f in &other.modified_files {
            if !self.modified_files.contains(f) {
                self.modified_files.push(f.clone());
            }
        }
        // Image token counts are summed (not deduplicated) because
        // each compact measures its own slice.
        self.image_token_count = self.image_token_count.saturating_add(other.image_token_count);
        // Phase is last-write-wins: the most recent compaction's
        // phase is the phase of the cumulative details. Most
        // sessions stay in `PreTurn` (the default); flipping to
        // `MidTurn` is sticky until the next pre-turn fires.
        self.phase = other.phase;
    }
}

/// Format a structured summary with file operations appended.
///
/// The output follows the ADR-022 structured format:
/// ```markdown
/// ## Goal
/// ...
/// ## Progress
/// ...
/// <read-files>
/// path/to/file1.rs
/// </read-files>
/// <modified-files>
/// path/to/changed.rs
/// </modified-files>
/// ```
pub fn format_summary_with_file_ops(summary: &str, details: &CompactionDetails) -> String {
    let mut result = summary.trim().to_string();

    if !details.read_files.is_empty() {
        result.push_str("\n\n<read-files>\n");
        for f in &details.read_files {
            result.push_str(f);
            result.push('\n');
        }
        result.push_str("</read-files>");
    }

    if !details.modified_files.is_empty() {
        result.push_str("\n\n<modified-files>\n");
        for f in &details.modified_files {
            result.push_str(f);
            result.push('\n');
        }
        result.push_str("</modified-files>");
    }

    result
}

/// Extract file operations from a list of messages being summarized.
///
/// Scans tool calls for `Read`, `Write`, `Edit`, etc.
/// This is a best-effort heuristic — exact tracking depends on tool naming.
pub fn extract_file_ops_from_messages(messages: &[peko_message::LlmMessage]) -> CompactionDetails {
    use peko_message::ContentBlock;
    use peko_message::MessageRole;

    let mut read = Vec::new();
    let mut modified = Vec::new();
    let mut image_tokens: usize = 0;

    for msg in messages {
        // Image tokens count across all roles — users may attach
        // images, assistants may return them (F28 / Responses API).
        for block in &msg.content {
            if let ContentBlock::Image { source, mime_type } = block {
                image_tokens =
                    image_tokens.saturating_add(crate::estimate_image_tokens(source, mime_type));
            }
        }

        if msg.role != MessageRole::Assistant {
            continue;
        }

        // Look for tool calls in assistant messages
        for block in &msg.content {
            if let ContentBlock::ToolCall {
                name, arguments, ..
            } = block
            {
                let name_lower = name.to_lowercase();
                if let Ok(_args) = serde_json::to_string(arguments) {
                    // Extract path from arguments if present
                    if let Some(path) = extract_path_from_args(arguments) {
                        if name_lower.contains("read")
                            || name_lower.contains("view")
                            || name_lower.contains("grep")
                            || name_lower.contains("search")
                        {
                            if !read.contains(&path) {
                                read.push(path);
                            }
                        } else if name_lower.contains("write")
                            || name_lower.contains("edit")
                            || name_lower.contains("create")
                            || name_lower.contains("modify")
                        {
                            if !modified.contains(&path) {
                                modified.push(path);
                            }
                        } else {
                            // Unknown tool — add to read as conservative default
                            if !read.contains(&path) {
                                read.push(path);
                            }
                        }
                    }
                }
            }
        }
    }

    CompactionDetails {
        read_files: read,
        modified_files: modified,
        image_token_count: image_tokens,
        // `phase` is not derivable from message contents alone — the
        // compactor stamps it from the `CompactionRequest.phase` at
        // the top of `Compactor::compact`. Defaulting here keeps this
        // helper usable from non-Compactor call sites (tests, ad-hoc
        // file-ops queries) without forcing every caller to thread
        // the phase through.
        phase: CompactionPhase::PreTurn,
    }
}

/// Try to extract a file path from tool call arguments.
fn extract_path_from_args(args: &serde_json::Value) -> Option<String> {
    // Common patterns: {"file_path": "..."}, {"path": "..."}, {"file": "..."}, {"target": "..."}
    for key in &[
        "file_path",
        "path",
        "file",
        "target",
        "filepath",
        "filename",
    ] {
        if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
            return Some(path.to_string());
        }
    }
    None
}

/// Build a cumulative details from previous details and new messages.
pub fn compute_cumulative_details(
    previous: Option<&CompactionDetails>,
    new_messages: &[peko_message::LlmMessage],
) -> CompactionDetails {
    let mut details = previous.cloned().unwrap_or_default();
    let new_ops = extract_file_ops_from_messages(new_messages);
    details.merge(&new_ops);
    details
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_message::ContentBlock;
    use peko_message::LlmMessage;
    use peko_message::MessageRole;

    #[test]
    fn test_format_summary_with_file_ops() {
        let summary = "## Goal\nTest goal".to_string();
        let details = CompactionDetails {
            read_files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
            modified_files: vec!["src/main.rs".to_string()],
            image_token_count: 0,
            phase: CompactionPhase::PreTurn,
        };

        let formatted = format_summary_with_file_ops(&summary, &details);
        assert!(formatted.contains("## Goal"));
        assert!(formatted.contains("<read-files>"));
        assert!(formatted.contains("src/main.rs"));
        assert!(formatted.contains("<modified-files>"));
    }

    #[test]
    fn test_format_summary_no_files() {
        let summary = "## Goal\nTest".to_string();
        let details = CompactionDetails::default();
        let formatted = format_summary_with_file_ops(&summary, &details);
        assert!(!formatted.contains("<read-files>"));
        assert!(!formatted.contains("<modified-files>"));
    }

    #[test]
    fn test_extract_file_ops_from_messages() {
        let messages = vec![LlmMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "I'll read the file.".to_string(),
                },
                ContentBlock::ToolCall {
                    id: "tc1".to_string(),
                    name: "Read".to_string(),
                    arguments: serde_json::json!({"file_path": "src/main.rs"}),
                },
                ContentBlock::ToolCall {
                    id: "tc2".to_string(),
                    name: "Write".to_string(),
                    arguments: serde_json::json!({"file_path": "src/lib.rs", "content": "..."}),
                },
            ],
            ..Default::default()
        }];

        let ops = extract_file_ops_from_messages(&messages);
        assert_eq!(ops.read_files, vec!["src/main.rs"]);
        assert_eq!(ops.modified_files, vec!["src/lib.rs"]);
    }

    #[test]
    fn test_cumulative_details_merge() {
        let prev = CompactionDetails {
            read_files: vec!["a.rs".to_string()],
            modified_files: vec!["b.rs".to_string()],
            image_token_count: 0,
            phase: CompactionPhase::PreTurn,
        };

        let messages = vec![LlmMessage {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "tc1".to_string(),
                name: "Read".to_string(),
                arguments: serde_json::json!({"file_path": "c.rs"}),
            }],
            ..Default::default()
        }];

        let cumulative = compute_cumulative_details(Some(&prev), &messages);
        assert!(cumulative.read_files.contains(&"a.rs".to_string()));
        assert!(cumulative.read_files.contains(&"c.rs".to_string()));
        assert!(cumulative.modified_files.contains(&"b.rs".to_string()));
    }

    #[test]
    fn test_details_merge_deduplicates() {
        let mut d1 = CompactionDetails {
            read_files: vec!["a.rs".to_string()],
            modified_files: vec![],
            image_token_count: 0,
            phase: CompactionPhase::PreTurn,
        };
        let d2 = CompactionDetails {
            read_files: vec!["a.rs".to_string(), "b.rs".to_string()],
            modified_files: vec![],
            image_token_count: 1500,
            phase: CompactionPhase::PreTurn,
        };
        d1.merge(&d2);
        assert_eq!(d1.read_files.len(), 2);
        assert!(d1.read_files.contains(&"a.rs".to_string()));
        assert!(d1.read_files.contains(&"b.rs".to_string()));
    }

    /// PR1: image token count is summed across merge (not deduplicated)
    /// because each compaction measures its own slice. The total
    /// represents cumulative image cost across the session's lifetime.
    #[test]
    fn test_details_merge_sums_image_tokens() {
        let mut d1 = CompactionDetails {
            image_token_count: 1500,
            phase: CompactionPhase::PreTurn,
            ..Default::default()
        };
        let d2 = CompactionDetails {
            image_token_count: 2500,
            phase: CompactionPhase::PreTurn,
            ..Default::default()
        };
        d1.merge(&d2);
        assert_eq!(d1.image_token_count, 4000);
    }

    /// PR3: phase is last-write-wins on merge. A session that fires
    /// mid-turn after a pre-turn inherits the mid-turn flag on the
    /// cumulative details — hooks see what *most recently* fired.
    #[test]
    fn test_details_merge_phase_last_write_wins() {
        let mut d1 = CompactionDetails {
            phase: CompactionPhase::PreTurn,
            ..Default::default()
        };
        let d2 = CompactionDetails {
            phase: CompactionPhase::MidTurn,
            ..Default::default()
        };
        d1.merge(&d2);
        assert_eq!(d1.phase, CompactionPhase::MidTurn);
    }

    /// PR1: extract_file_ops_from_messages counts image tokens across
    /// user + assistant messages (users attach images, assistants
    /// return them in Responses output_image).
    #[test]
    fn test_extract_counts_image_tokens_across_roles() {
        let messages = vec![
            LlmMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Image {
                    source: peko_message::ImageSource::Url {
                        url: "https://x.png".to_string(),
                        dimensions: None,
                    },
                    mime_type: "image/png".to_string(),
                }],
                ..Default::default()
            },
            LlmMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Image {
                    source: peko_message::ImageSource::Url {
                        url: "https://x.jpg".to_string(),
                        dimensions: None,
                    },
                    mime_type: "image/jpeg".to_string(),
                }],
                ..Default::default()
            },
        ];
        let ops = extract_file_ops_from_messages(&messages);
        // tier-3 PNG = 2500, jpeg = 1500 → total 4000.
        assert_eq!(ops.image_token_count, 4000);
    }

    /// PR1: pre-PR-1 JSONL (no `image_token_count` field) reads as 0.
    /// Backwards-compat for the `#[serde(default)]` attribute.
    #[test]
    fn test_details_legacy_loads_with_zero_image_tokens() {
        let legacy = serde_json::json!({
            "read_files": ["a.rs"],
            "modified_files": [],
        });
        let parsed: CompactionDetails = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.image_token_count, 0);
        assert_eq!(parsed.read_files, vec!["a.rs"]);
        // PR3: pre-PR-3 entries default the phase to `PreTurn`.
        assert_eq!(parsed.phase, CompactionPhase::PreTurn);
    }

    /// PR3: `CompactionPhase` round-trips through serde for every
    /// variant (snake_case wire shape).
    #[test]
    fn test_phase_serde_round_trip() {
        for phase in [
            CompactionPhase::PreTurn,
            CompactionPhase::MidTurn,
            CompactionPhase::StandaloneTurn,
        ] {
            let v = serde_json::to_value(phase).unwrap();
            assert!(
                v.is_string(),
                "phase should serialize as a string, got {v:?}"
            );
            let back: CompactionPhase = serde_json::from_value(v).unwrap();
            assert_eq!(back, phase);
        }
    }
}
