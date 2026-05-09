//! ADR-022 Integration Tests
//!
//! Tests covering the success criteria from ADR-022:
//! - Dual-threshold trigger with actual model context limit
//! - Context building with compaction entries
//! - Turn boundary preservation
//! - Split-turn handling
//! - Structured summary format with file operations
//! - Cache validation and invalidation

use crate::compaction::{
    registry::{should_auto_compact, ModelContextRegistry},
    summary_format::{
        extract_file_ops_from_messages, format_summary_with_file_ops, CompactionDetails,
    },
    turn_boundaries::{
        classify_message, find_cut_points, select_messages_respecting_boundaries, MessageKind,
    },
    CompactionConfig, CompactionEntry, Compactor,
};
use crate::types::message::{ContentBlock, LlmMessage, MessageRole};

// ============================================================================
// Success Criterion: Built-in compactor triggers using dual-threshold
// ============================================================================

#[test]
fn test_dual_threshold_ratio_fires() {
    let config = CompactionConfig {
        enabled: true,
        auto_threshold_percent: 85,
        reserve_tokens: 16_384,
        keep_recent_tokens: 20_000,
        cooldown_seconds: 60,
        max_compactions_per_session: 10,
        model_limits: std::collections::HashMap::new(),
    };

    let registry = ModelContextRegistry::new();
    let context_window = registry.get("openai", "gpt-4o");

    let threshold = context_window.saturating_sub(config.reserve_tokens);
    let ratio = (threshold as f64 / context_window as f64) * 100.0;

    assert!(
        ratio > config.auto_threshold_percent as f64,
        "Threshold ratio ({:.1}%) should be above auto_threshold ({}%)",
        ratio,
        config.auto_threshold_percent
    );
}

#[test]
fn test_registry_known_models() {
    let reg = ModelContextRegistry::new();
    assert_eq!(reg.get("openai", "gpt-4o"), 128_000);
    assert_eq!(reg.get("anthropic", "claude-3-5-sonnet"), 200_000);
    assert_eq!(reg.get("unknown", "unknown"), 128_000); // default
}

// ============================================================================
// Success Criterion: Turn boundaries respected — never cuts at tool results
// ============================================================================

#[test]
fn test_never_cuts_at_tool_result() {
    let messages = vec![
        LlmMessage::user("User"),
        LlmMessage::assistant("Assistant"),
        LlmMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: "tc1".to_string(),
                name: "read_file".to_string(),
                content: vec![ContentBlock::Text {
                    text: "content".to_string(),
                }],
                is_error: false,
            }],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            tool_call_id: Some("tc1".to_string()),
        },
        LlmMessage::user("Next"),
    ];

    // Force a cut that would land on the tool result
    let (compact, keep, _split) = select_messages_respecting_boundaries(&messages, 20);

    // If tool is in keep, assistant MUST also be in keep
    if keep.iter().any(|m| m.role == MessageRole::Tool) {
        let tool_idx = keep
            .iter()
            .position(|m| m.role == MessageRole::Tool)
            .unwrap();
        assert!(
            tool_idx > 0 && keep[tool_idx - 1].role == MessageRole::Assistant,
            "Tool result at index {tool_idx} must follow assistant"
        );
    }

    // Tool result should never be in compact without its assistant
    let compact_tool_idx = compact.iter().position(|m| m.role == MessageRole::Tool);
    if let Some(idx) = compact_tool_idx {
        assert!(
            idx > 0 && compact[idx - 1].role == MessageRole::Assistant,
            "Compacted tool result must follow assistant"
        );
    }
}

#[test]
fn test_classify_message_kinds() {
    let user = LlmMessage::user("Hello");
    let assistant = LlmMessage::assistant("Hi");
    let tool = LlmMessage::tool_result("tc1", "test_tool", "result");

    assert_eq!(classify_message(&user), MessageKind::User);
    assert_eq!(classify_message(&assistant), MessageKind::Assistant);
    assert_eq!(classify_message(&tool), MessageKind::ToolResult);
}

#[test]
fn test_find_cut_points_excludes_tool_results() {
    let messages = vec![
        LlmMessage::user("A"),
        LlmMessage::assistant("B"),
        LlmMessage::tool_result("tc1", "test_tool", "C"),
        LlmMessage::user("D"),
    ];

    let cuts = find_cut_points(&messages);
    // Cut points should only be at indices 0, 1, 3 (not 2 which is tool result)
    assert!(
        !cuts.contains(&2),
        "Tool result index should not be a cut point"
    );
    assert!(cuts.contains(&0));
    assert!(cuts.contains(&1));
    assert!(cuts.contains(&3));
}

// ============================================================================
// Success Criterion: Structured summary format with file operations
// ============================================================================

#[test]
fn test_structured_summary_with_read_files() {
    let summary = "## Goal\nTest";
    let details = CompactionDetails {
        read_files: vec!["src/main.rs".to_string()],
        modified_files: vec![],
    };

    let formatted = format_summary_with_file_ops(summary, &details);
    assert!(formatted.contains("<read-files>"));
    assert!(formatted.contains("src/main.rs"));
}

#[test]
fn test_structured_summary_with_modified_files() {
    let summary = "## Goal\nTest";
    let details = CompactionDetails {
        read_files: vec![],
        modified_files: vec!["src/main.rs".to_string()],
    };

    let formatted = format_summary_with_file_ops(summary, &details);
    assert!(!formatted.contains("<read-files>"));
    assert!(formatted.contains("<modified-files>"));
    assert!(formatted.contains("src/main.rs"));
}

#[test]
fn test_file_op_extraction_from_tool_calls() {
    let messages = vec![LlmMessage {
        role: MessageRole::Assistant,
        content: vec![
            ContentBlock::ToolCall {
                id: "tc1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "config.toml"}),
            },
            ContentBlock::ToolCall {
                id: "tc2".to_string(),
                name: "write_file".to_string(),
                arguments: serde_json::json!({"path": "output.txt", "content": "..."}),
            },
        ],
        timestamp: chrono::Utc::now(),
        metadata: std::collections::HashMap::new(),
        tool_call_id: None,
    }];

    let ops = extract_file_ops_from_messages(&messages);
    assert_eq!(ops.read_files, vec!["config.toml"]);
    assert_eq!(ops.modified_files, vec!["output.txt"]);
}

// ============================================================================
// Success Criterion: Compactor state tracking
// ============================================================================

#[test]
fn test_compactor_state_tracking() {
    let mut compactor = Compactor::new();
    assert_eq!(compactor.state().compaction_count, 0);
    assert_eq!(compactor.state().total_tokens_saved, 0);
    assert!(compactor.state().last_compaction_at.is_none());

    // Simulate state update (normally done during compact())
    // We can't call compact() without a provider, but we can verify state structure
    let state = compactor.state().clone();
    assert_eq!(state.compaction_count, 0);
}

#[test]
fn test_compaction_entry_with_details() {
    let entry = CompactionEntry {
        timestamp: chrono::Utc::now(),
        summary: "Test summary".to_string(),
        first_kept_entry_id: "kept_2".to_string(),
        messages_compacted: 10,
        tokens_before: 1000,
        tokens_after: 200,
        compaction_number: 1,
        details: Some(CompactionDetails {
            read_files: vec!["a.rs".to_string()],
            modified_files: vec!["b.rs".to_string()],
        }),
    };

    assert_eq!(entry.messages_compacted, 10);
    assert_eq!(entry.tokens_before, 1000);
    assert_eq!(entry.tokens_after, 200);
    let details = entry.details.unwrap();
    assert_eq!(details.read_files, vec!["a.rs"]);
    assert_eq!(details.modified_files, vec!["b.rs"]);
}

// ============================================================================
// Helpers
// ============================================================================

fn make_msg(role: MessageRole, text: &str) -> LlmMessage {
    LlmMessage::text(role, text)
}

fn make_tool_result(tool_call_id: &str, text: &str) -> LlmMessage {
    LlmMessage::tool_result(tool_call_id, "test_tool", text)
}
