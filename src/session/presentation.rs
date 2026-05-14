//! Session presentation layer
//!
//! Converts session data into CLI display format.
//! Different channels (CLI, TUI, web) can reuse the same DTOs but format them differently.

use crate::common::services::session_service::{HistoryEvent, SessionInfo};
use crate::common::time::format_timestamp;
use serde::Serialize;

// ================================================================================
// History Display
// ================================================================================

/// History display entry - represents a parsed JSONL entry for display
#[derive(Debug, Serialize)]
pub enum HistoryDisplayEntry {
    Session {
        timestamp: String,
    },
    Message {
        role: String,
        content: String,
        timestamp: String,
    },
    ToolResult {
        tool_name: String,
        content: String,
    },
    ModelChange {
        provider: String,
        model_id: String,
    },
    Compaction {
        summary: String,
    },
    Custom {
        custom_type: String,
    },
}

/// Convert service `HistoryEvent` to CLI display format
#[must_use]
pub fn history_event_to_display(event: HistoryEvent) -> Option<HistoryDisplayEntry> {
    match event {
        HistoryEvent::Session { timestamp } => Some(HistoryDisplayEntry::Session { timestamp }),
        HistoryEvent::Message {
            role,
            content,
            timestamp,
        } => Some(HistoryDisplayEntry::Message {
            role,
            content,
            timestamp,
        }),
        HistoryEvent::ToolResult {
            tool_call_id,
            output,
            error,
        } => Some(HistoryDisplayEntry::ToolResult {
            tool_name: tool_call_id, // Use tool_call_id as identifier since name isn't in event
            content: output.unwrap_or_else(|| error.unwrap_or_default()),
        }),
        HistoryEvent::ModelChange { provider, model_id } => {
            Some(HistoryDisplayEntry::ModelChange { provider, model_id })
        }
        HistoryEvent::ToolCall {
            tool_name, args, ..
        } => {
            let content = format!("{args}");
            Some(HistoryDisplayEntry::ToolResult { tool_name, content })
        }
        HistoryEvent::Thinking { content } => Some(HistoryDisplayEntry::Message {
            role: "thinking".to_string(),
            content,
            timestamp: String::new(), // Thinking events don't have timestamp in current format
        }),
        HistoryEvent::Compaction { summary } => Some(HistoryDisplayEntry::Compaction { summary }),
        HistoryEvent::Custom { custom_type } => Some(HistoryDisplayEntry::Custom { custom_type }),
    }
}

/// Format a history event as a display string
#[must_use]
pub fn format_history_event(index: usize, event: &HistoryDisplayEntry) -> String {
    match event {
        HistoryDisplayEntry::Session { timestamp } => {
            format!(
                "  [{}] 🆕 Session Created ({})",
                index,
                format_timestamp(timestamp)
            )
        }
        HistoryDisplayEntry::Message {
            role,
            content,
            timestamp,
        } => {
            let time_str = format_timestamp(timestamp);
            let header = match role.as_str() {
                "system" => format!("  [{index}] ⚙️  System ({time_str}):"),
                "user" => format!("  [{index}] 👤 User ({time_str}):"),
                "assistant" => format!("  [{index}] 🤖 Assistant ({time_str}):"),
                "tool" => format!("  [{index}] 🔧 Tool ({time_str}):"),
                _ => format!("  [{index}] {role} ({time_str}):"),
            };
            let mut out = header;
            out.push_str(&format!("\n      {}", truncate(content, 200)));
            out
        }
        HistoryDisplayEntry::ToolResult { tool_name, content } => {
            let mut out = format!("  [{index}] ✅ Tool Result:\n      Tool: {tool_name}");
            if !content.is_empty() {
                out.push_str(&format!("\n      Output: {}", truncate(content, 150)));
            }
            out
        }
        HistoryDisplayEntry::ModelChange { provider, model_id } => {
            format!("  [{index}] 🔄 Model Change:\n      Provider: {provider}, Model: {model_id}")
        }
        HistoryDisplayEntry::Compaction { summary } => {
            format!(
                "  [{index}] 📦 Compaction:\n      {}",
                truncate(summary, 150)
            )
        }
        HistoryDisplayEntry::Custom { custom_type } => {
            format!("  [{index}] 📎 Custom ({custom_type})")
        }
    }
}

// ================================================================================
// Session List Rendering
// ================================================================================

/// Render session list output
pub fn render_session_list(
    sessions: &[SessionInfo],
    team: &str,
    agent: &str,
    active_session_id: Option<&str>,
) {
    if sessions.is_empty() {
        println!("📭 No sessions found for '{agent}'.");
        println!("   Start chatting with the agent to create sessions.");
        return;
    }

    println!(
        "📋 Sessions for {}/{} ({} found):",
        team,
        agent,
        sessions.len()
    );
    if let Some(active) = active_session_id {
        println!("   Active session: {active}");
    }
    println!();

    for session in sessions {
        let title = session.title.as_deref().unwrap_or("(untitled)");
        let created = crate::common::time::format_timestamp_ms(session.created_at);
        let updated = crate::common::time::format_timestamp_ms(session.updated_at);

        let is_active = active_session_id == Some(session.id.as_str());
        let status_icon = if is_active { "⭐ " } else { "   " };

        println!("{} {}", status_icon, session.id);
        println!("     Title: {title}");
        println!(
            "     Messages: {} | Window: {} | In: {} | Out: {}",
            session.message_count,
            session.context_window,
            session.total_input_tokens,
            session.total_output_tokens
        );
        println!("     Created: {created} | Updated: {updated}");

        if let Some(ref parent) = session.parent_session_id {
            println!("     Branched from: {parent}");
        }
        println!();
    }
}

/// Render session list as JSON
pub fn render_session_list_json(
    sessions: &[SessionInfo],
    team: &str,
    agent: &str,
    active_session_id: Option<&str>,
) -> anyhow::Result<()> {
    let output = serde_json::json!({
        "team": team,
        "agent": agent,
        "sessions": sessions,
        "active_session": active_session_id,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ================================================================================
// Session Show Rendering
// ================================================================================

/// Render session details output
pub fn render_session_details(entry: &SessionInfo, team: &str, agent: &str) {
    println!("📊 Session Details");
    println!("   Team: {team}");
    println!("   Agent: {agent}");
    println!("   Session ID: {}", entry.id);
    if let Some(ref title) = entry.title {
        println!("   Title: {title}");
    }
    println!(
        "   Messages: {} | Window: {} | In: {} | Out: {}",
        entry.message_count,
        entry.context_window,
        entry.total_input_tokens,
        entry.total_output_tokens
    );
    println!(
        "   Created: {}",
        crate::common::time::format_timestamp_ms(entry.created_at)
    );
    println!(
        "   Updated: {}",
        crate::common::time::format_timestamp_ms(entry.updated_at)
    );

    if let Some(ref parent) = entry.parent_session_id {
        println!("   Parent Session: {parent}");
    }
}

/// Render session history events
pub fn render_session_history(events: &[HistoryDisplayEntry]) {
    println!();
    println!("📜 Message History ({} events):", events.len());
    println!();
    for (i, event) in events.iter().enumerate() {
        println!("{}", format_history_event(i, event));
    }
}

/// Render session show as JSON
pub fn render_session_show_json(
    entry: &SessionInfo,
    history_events: Option<&[HistoryDisplayEntry]>,
) -> anyhow::Result<()> {
    let output = serde_json::json!({
        "session": entry,
        "history": history_events,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ================================================================================
// Branch / Delete / Switch / Compact Rendering
// ================================================================================

/// Render branch success
pub fn render_branch_success(
    team: &str,
    agent: &str,
    session_id: &str,
    new_session_id: &str,
    label: Option<&str>,
) {
    println!("✅ Branched session '{session_id}'");
    println!("   New Session ID: {new_session_id}");
    println!("   Parent Session: {session_id}");
    if let Some(lbl) = label {
        println!("   Label: {lbl}");
    }
    println!();
    println!("   The branched session contains a copy of the parent's history.");
    println!("   Switch to it with: peko session switch {team}/{agent} {new_session_id}");
}

/// Render delete confirmation prompt
pub fn render_delete_prompt(session_id: &str, metadata: Option<&SessionInfo>) {
    println!("⚠️  This will permanently delete session '{session_id}'.");
    if let Some(meta) = metadata {
        if let Some(ref title) = meta.title {
            println!("   Title: {title}");
        }
        println!("   Messages: {}", meta.message_count);
    }
    println!("   This action cannot be undone.");
    print!("   Continue? [y/N] ");
}

/// Render delete success
pub fn render_delete_success(session_id: &str, deleted: bool) {
    println!("✅ Deleted session '{session_id}'");
    if deleted {
        println!("   Removed: jsonl, index");
    }
}

/// Render switch success
pub fn render_switch_success(team: &str, agent: &str, session_id: &str) {
    println!("✅ Switched active session for '{team}/{agent}' to '{session_id}'");
    println!();
    println!("   Future 'peko send' commands will use this session.");
}

/// Render compact dry-run
pub fn render_compact_dry_run(session_id: &str, report: &crate::compaction::cli::DryRunReport) {
    println!("📊 Dry-run compaction for session '{session_id}'");
    println!(
        "   Estimated tokens: {} / {} ({}%)",
        report.estimated_tokens, report.context_window, report.percent
    );
    println!("   Messages: {}", report.message_count);
    println!();
    println!(
        "   Would compact {} messages into a summary.",
        report.messages_to_compact
    );
}

/// Render compact success
pub fn render_compact_success(
    session_id: &str,
    messages_compacted: usize,
    tokens_saved: usize,
    tokens_before: usize,
    tokens_after: usize,
    has_instruction: bool,
) {
    println!("✅ Compacted session '{session_id}'");
    println!(
        "   {messages_compacted} messages → summary, saved {tokens_saved} tokens ({tokens_before} → {tokens_after})"
    );
    if has_instruction {
        println!("   Custom instruction applied.");
    }
}

// ================================================================================
// Utilities
// ================================================================================

/// Truncate a string to max length with ellipsis
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a very long string", 10), "this is a ...");
    }

    #[test]
    fn test_history_event_to_display_message() {
        let event = HistoryEvent::Message {
            role: "user".to_string(),
            content: "Hello".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };
        let display = history_event_to_display(event).unwrap();
        match display {
            HistoryDisplayEntry::Message { role, content, .. } => {
                assert_eq!(role, "user");
                assert_eq!(content, "Hello");
            }
            _ => panic!("Expected Message variant"),
        }
    }

    #[test]
    fn test_format_history_event_session() {
        let event = HistoryDisplayEntry::Session {
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };
        let formatted = format_history_event(0, &event);
        assert!(formatted.contains("🆕 Session Created"));
    }
}
