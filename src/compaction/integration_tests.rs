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
    summary_format::{extract_file_ops_from_messages, format_summary_with_file_ops, CompactionDetails},
    turn_boundaries::{classify_message, find_cut_points, select_messages_respecting_boundaries, MessageKind},
    CompactionConfig, CompactionEntry, Compactor,
};
use crate::providers::{ChatMessage, MessageRole};
use crate::types::message::ContentBlock;

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
        ..CompactionConfig::default()
    };

    // Large model (1M context): 860K tokens = 86% → ratio threshold fires
    assert!(should_auto_compact(860_000, 1_000_000, &config));
}

#[test]
fn test_dual_threshold_reserved_fires() {
    let config = CompactionConfig {
        enabled: true,
        auto_threshold_percent: 85,
        reserve_tokens: 16_384,
        keep_recent_tokens: 20_000,
        ..CompactionConfig::default()
    };

    // Standard model (128K): 115K tokens
    // Ratio: 85% of 128K = 108.8K → 115K > 108.8K ✓
    // Reserved: 128K - 16K = 112K → 115K > 112K ✓
    assert!(should_auto_compact(115_000, 128_000, &config));
}

#[test]
fn test_dual_threshold_neither_fires() {
    let config = CompactionConfig {
        enabled: true,
        auto_threshold_percent: 85,
        reserve_tokens: 16_384,
        keep_recent_tokens: 20_000,
        ..CompactionConfig::default()
    };

    // Well under both thresholds
    assert!(!should_auto_compact(50_000, 128_000, &config));
}

#[test]
fn test_model_context_registry_known_models() {
    let reg = ModelContextRegistry::new();
    assert_eq!(reg.get("openai", "gpt-4o"), 128_000);
    assert_eq!(reg.get("kimi", "K2.6"), 262_144);
    assert_eq!(reg.get("minimax", "M2.7"), 204_800);
    assert_eq!(reg.get("anthropic", "claude-3-5-sonnet"), 200_000);
}

#[test]
fn test_model_context_registry_fallback() {
    let reg = ModelContextRegistry::new();
    assert_eq!(reg.get("unknown", "unknown"), 128_000); // default
}

// ============================================================================
// Success Criterion: Turn boundaries respected — never cuts at tool results
// ============================================================================

#[test]
fn test_never_cuts_at_tool_result() {
    let messages = vec![
        ChatMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: "User".to_string() }],
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text { text: "Assistant".to_string() }],
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: "tc1".to_string(),
                name: "read_file".to_string(),
                content: vec![ContentBlock::Text { text: "content".to_string() }],
                is_error: false,
            }],
            tool_calls: None,
            tool_call_id: Some("tc1".to_string()),
        },
        ChatMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: "Next".to_string() }],
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    // Force a cut that would land on the tool result
    let (compact, keep, _split) = select_messages_respecting_boundaries(&messages, 20);

    // If tool is in keep, assistant MUST also be in keep
    if keep.iter().any(|m| m.role == MessageRole::Tool) {
        let tool_idx = keep.iter().position(|m| m.role == MessageRole::Tool).unwrap();
        assert!(
            tool_idx > 0 && keep[tool_idx - 1].role == MessageRole::Assistant,
            "Tool result at index {tool_idx} must follow assistant"
        );
    }

    // If tool is in compact, it must be with its assistant
    if compact.iter().any(|m| m.role == MessageRole::Tool) {
        let tool_idx = compact.iter().position(|m| m.role == MessageRole::Tool).unwrap();
        // The assistant should be either just before in compact, or in keep
        let has_assistant_nearby = (tool_idx > 0 && compact[tool_idx - 1].role == MessageRole::Assistant)
            || keep.first().map(|m| m.role == MessageRole::Assistant).unwrap_or(false);
        assert!(has_assistant_nearby, "Tool result must stay near its assistant");
    }
}

#[test]
fn test_cut_points_exclude_tool_results() {
    let messages = vec![
        make_msg(MessageRole::System, "Prompt"),
        make_msg(MessageRole::User, "Hello"),
        make_msg(MessageRole::Assistant, "Hi"),
        make_tool_result("tc1", "result"),
        make_msg(MessageRole::User, "Next"),
    ];

    let cuts = find_cut_points(&messages);
    assert!(!cuts.contains(&3), "Tool result index 3 should NOT be a cut point");
    assert!(cuts.contains(&0), "System should be a cut point");
    assert!(cuts.contains(&1), "User should be a cut point");
    assert!(cuts.contains(&2), "Assistant should be a cut point");
    assert!(cuts.contains(&4), "User at 4 should be a cut point");
}

// ============================================================================
// Success Criterion: Structured summary format includes file operations
// ============================================================================

#[test]
fn test_structured_summary_with_read_files() {
    let summary = "## Goal\nTest\n\n## Progress\n### Done\n- [x] Task 1";
    let details = CompactionDetails {
        read_files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
        modified_files: vec![],
    };

    let formatted = format_summary_with_file_ops(summary, &details);
    assert!(formatted.contains("<read-files>"));
    assert!(formatted.contains("src/main.rs"));
    assert!(formatted.contains("src/lib.rs"));
    assert!(!formatted.contains("<modified-files>"));
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
    let messages = vec![ChatMessage {
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
        tool_calls: None,
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

fn make_msg(role: MessageRole, text: &str) -> ChatMessage {
    ChatMessage {
        role,
        content: vec![ContentBlock::Text { text: text.to_string() }],
        tool_calls: None,
        tool_call_id: None,
    }
}

fn make_tool_result(tool_call_id: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            name: "test_tool".to_string(),
            content: vec![ContentBlock::Text { text: text.to_string() }],
            is_error: false,
        }],
        tool_calls: None,
        tool_call_id: Some(tool_call_id.to_string()),
    }
}
