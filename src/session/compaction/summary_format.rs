//! Structured Summary Format and File Operation Tracking
//!
//! Provides the proven pi-mono inspired summary format with:
//! - Goal, Constraints, Progress (Done/In Progress/Blocked)
//! - Key Decisions, Next Steps, Critical Context
//! - File operation tracking (read_files, modified_files)

use serde::{Deserialize, Serialize};

/// Details tracked across compactions for cumulative file operations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionDetails {
    /// Files that were read (via tool calls)
    pub read_files: Vec<String>,
    /// Files that were modified (via tool calls)
    pub modified_files: Vec<String>,
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
/// Scans tool calls for `read_file`, `write_file`, `edit_file`, etc.
/// This is a best-effort heuristic — exact tracking depends on tool naming.
pub fn extract_file_ops_from_messages(
    messages: &[crate::common::types::message::LlmMessage],
) -> CompactionDetails {
    use crate::common::types::message::ContentBlock;
    use crate::providers::MessageRole;

    let mut read = Vec::new();
    let mut modified = Vec::new();

    for msg in messages {
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
    }
}

/// Try to extract a file path from tool call arguments.
fn extract_path_from_args(args: &serde_json::Value) -> Option<String> {
    // Common patterns: {"path": "..."}, {"file": "..."}, {"target": "..."}
    for key in &["path", "file", "target", "filepath", "filename"] {
        if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
            return Some(path.to_string());
        }
    }
    None
}

/// Build a cumulative details from previous details and new messages.
pub fn compute_cumulative_details(
    previous: Option<&CompactionDetails>,
    new_messages: &[crate::common::types::message::LlmMessage],
) -> CompactionDetails {
    let mut details = previous.cloned().unwrap_or_default();
    let new_ops = extract_file_ops_from_messages(new_messages);
    details.merge(&new_ops);
    details
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::message::ContentBlock;
    use crate::common::types::message::LlmMessage;
    use crate::providers::MessageRole;

    #[test]
    fn test_format_summary_with_file_ops() {
        let summary = "## Goal\nTest goal".to_string();
        let details = CompactionDetails {
            read_files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
            modified_files: vec!["src/main.rs".to_string()],
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
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                },
                ContentBlock::ToolCall {
                    id: "tc2".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({"path": "src/lib.rs", "content": "..."}),
                },
            ],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            tool_call_id: None,
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
        };

        let messages = vec![LlmMessage {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "tc1".to_string(),
                name: "Read".to_string(),
                arguments: serde_json::json!({"path": "c.rs"}),
            }],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            tool_call_id: None,
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
        };
        let d2 = CompactionDetails {
            read_files: vec!["a.rs".to_string(), "b.rs".to_string()],
            modified_files: vec![],
        };
        d1.merge(&d2);
        assert_eq!(d1.read_files.len(), 2);
        assert!(d1.read_files.contains(&"a.rs".to_string()));
        assert!(d1.read_files.contains(&"b.rs".to_string()));
    }
}
