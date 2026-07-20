//! Subagent Result Recovery
//!
//! Recovers meaningful output from subagent session history when the
//! agentic loop returns an empty final_answer.

use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::common::types::message::ContentBlock;
use crate::common::types::message::LlmMessage;
use crate::providers::MessageRole;

/// Service for recovering meaningful output from subagent session history.
pub struct ResultRecovery;

impl ResultRecovery {
    /// Attempt to recover a non-empty answer from session history.
    /// Returns `Some(recovered_text)` if recovery succeeds.
    pub async fn recover_from_session(
        session: &Arc<RwLock<crate::session::Session>>,
    ) -> Option<String> {
        match session.read().await.load_history().await {
            Ok(history) => {
                // First, try to find the last assistant message with non-empty text
                if let Some(text) = Self::extract_last_assistant(&history) {
                    info!(
                        "Recovered subagent answer from assistant message: {} chars",
                        text.len()
                    );
                    return Some(text);
                }

                // If no assistant text found, try to extract from tool results
                if let Some(text) = Self::extract_tool_results(&history) {
                    info!(
                        "Recovered subagent answer from tool results: {} chars",
                        text.len()
                    );
                    return Some(text);
                }

                info!("Subagent fallback: no tool results found in history");
                None
            }
            Err(e) => {
                warn!("Subagent fallback: failed to load history: {}", e);
                None
            }
        }
    }

    /// Extract the last assistant message text from history.
    fn extract_last_assistant(history: &[LlmMessage]) -> Option<String> {
        for msg in history.iter().rev() {
            if matches!(msg.role, MessageRole::Assistant) {
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::Text { text } = c {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }

    /// Extract and process tool results from history.
    fn extract_tool_results(history: &[LlmMessage]) -> Option<String> {
        let mut tool_results: Vec<String> = Vec::new();
        for msg in history.iter().rev() {
            if matches!(msg.role, MessageRole::Tool) {
                let mut msg_text = String::new();
                for c in &msg.content {
                    match c {
                        ContentBlock::Text { text } => {
                            msg_text.push_str(text);
                        }
                        ContentBlock::ToolResult { content, .. } => {
                            for nc in content {
                                if let ContentBlock::Text { text } = nc {
                                    msg_text.push_str(text);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !msg_text.trim().is_empty() {
                    tool_results.push(msg_text);
                }
            }
        }

        if tool_results.is_empty() {
            return None;
        }

        // Reverse to get chronological order and join
        tool_results.reverse();

        // Try to extract human-readable content from JSON tool results
        let processed_results: Vec<String> = tool_results
            .iter()
            .map(|raw| Self::process_tool_result(raw))
            .collect();

        let final_answer = processed_results.join("\n\n");
        if final_answer.trim().is_empty() {
            None
        } else {
            Some(final_answer)
        }
    }

    /// Try to extract human-readable content from a raw tool result string.
    fn process_tool_result(raw: &str) -> String {
        // Try to parse as JSON and extract common content fields
        if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(raw) {
            // For Read results: extract "content" field
            if let Some(content) = json_val.get("content").and_then(|v| v.as_str()) {
                return content.to_string();
            }
            // For shell results: extract "stdout" field
            if let Some(stdout) = json_val.get("stdout").and_then(|v| v.as_str()) {
                return stdout.to_string();
            }
            // For grep results: extract "matches" array
            if let Some(matches) = json_val.get("matches").and_then(|v| v.as_array()) {
                let lines: Vec<String> = matches
                    .iter()
                    .filter_map(|m| m.get("line").and_then(|v| v.as_str()).map(String::from))
                    .collect();
                if !lines.is_empty() {
                    return lines.join("\n");
                }
            }
        }
        // Fallback: return raw text
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_tool_result_read_file() {
        let raw = r#"{"content": "Hello world"}"#;
        assert_eq!(ResultRecovery::process_tool_result(raw), "Hello world");
    }

    #[test]
    fn test_process_tool_result_shell() {
        let raw = r#"{"stdout": "line1\nline2", "stderr": ""}"#;
        assert_eq!(ResultRecovery::process_tool_result(raw), "line1\nline2");
    }

    #[test]
    fn test_process_tool_result_grep() {
        let raw = r#"{"matches": [{"line": "fn main() {}"}, {"line": "fn foo() {}"}]}"#;
        assert_eq!(
            ResultRecovery::process_tool_result(raw),
            "fn main() {}\nfn foo() {}"
        );
    }

    #[test]
    fn test_process_tool_result_grep_empty_matches() {
        let raw = r#"{"matches": []}"#;
        assert_eq!(ResultRecovery::process_tool_result(raw), raw);
    }

    #[test]
    fn test_process_tool_result_fallback() {
        let raw = "plain text result";
        assert_eq!(ResultRecovery::process_tool_result(raw), raw);
    }

    #[test]
    fn test_process_tool_result_invalid_json() {
        let raw = "not json at all";
        assert_eq!(ResultRecovery::process_tool_result(raw), raw);
    }

    #[test]
    fn test_extract_last_assistant_found() {
        let history = vec![LlmMessage::user("hello"), LlmMessage::assistant("response")];
        assert_eq!(
            ResultRecovery::extract_last_assistant(&history),
            Some("response".to_string())
        );
    }

    #[test]
    fn test_extract_last_assistant_empty_text() {
        let history = vec![LlmMessage::assistant("   ")];
        assert_eq!(ResultRecovery::extract_last_assistant(&history), None);
    }

    #[test]
    fn test_extract_last_assistant_no_assistant() {
        let history = vec![LlmMessage::user("hello")];
        assert_eq!(ResultRecovery::extract_last_assistant(&history), None);
    }

    #[test]
    fn test_extract_last_assistant_multiple_blocks() {
        let history = vec![LlmMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "part1".into(),
                },
                ContentBlock::Text {
                    text: "part2".into(),
                },
            ],
            ..Default::default()
        }];
        assert_eq!(
            ResultRecovery::extract_last_assistant(&history),
            Some("part1part2".to_string())
        );
    }

    #[test]
    fn test_extract_last_assistant_skips_empty() {
        let history = vec![LlmMessage::assistant(""), LlmMessage::assistant("real")];
        assert_eq!(
            ResultRecovery::extract_last_assistant(&history),
            Some("real".to_string())
        );
    }

    #[test]
    fn test_extract_tool_results_found() {
        let history = vec![LlmMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::Text {
                text: "tool output".into(),
            }],
            ..Default::default()
        }];
        assert_eq!(
            ResultRecovery::extract_tool_results(&history),
            Some("tool output".to_string())
        );
    }

    #[test]
    fn test_extract_tool_results_with_tool_result_block() {
        let history = vec![LlmMessage::tool_result("id", "Read", "file content", false)];
        assert_eq!(
            ResultRecovery::extract_tool_results(&history),
            Some("file content".to_string())
        );
    }

    #[test]
    fn test_extract_tool_results_no_tool_messages() {
        let history = vec![LlmMessage::user("hello")];
        assert_eq!(ResultRecovery::extract_tool_results(&history), None);
    }

    #[test]
    fn test_extract_tool_results_chronological_order() {
        // History is iterated in reverse (newest first), then reversed back to chronological.
        // So if history = [newer, older], reverse iteration picks older first, then newer,
        // then .reverse() makes it [newer, older] again — which is chronological.
        let history = vec![
            LlmMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::Text {
                    text: "second".into(),
                }],
                ..Default::default()
            },
            LlmMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::Text {
                    text: "first".into(),
                }],
                ..Default::default()
            },
        ];
        assert_eq!(
            ResultRecovery::extract_tool_results(&history),
            Some("second\n\nfirst".to_string())
        );
    }

    #[test]
    fn test_extract_tool_results_json_processing() {
        let history = vec![LlmMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::Text {
                text: r#"{"content": "extracted"}"#.into(),
            }],
            ..Default::default()
        }];
        assert_eq!(
            ResultRecovery::extract_tool_results(&history),
            Some("extracted".to_string())
        );
    }
}
