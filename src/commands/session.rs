//! Session Management Commands
//!
//! These commands follow an offline-first design per ADR-013:
//! - Read operations (list, show) work directly from the filesystem
//! - File operations (branch, delete) modify files directly
//! - Only commands needing runtime coordination (send, switch on running instances) use the API
//!
//! Session storage layout:
//! - `{data_dir}/agents/{instance_id}/sessions/*.jsonl` - Session event logs
//! - `{data_dir}/agents/{instance_id}/sessions/sessions.json` - Centralized session index
//! - `{data_dir}/agents/{instance_id}/sessions/.active.json` - Preferred active session (CLI-managed)

use crate::commands::GlobalPaths;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::services::session_service::{HistoryEvent, HistoryQuery, SessionService};
use crate::session::metadata_controller::MetadataController;
use crate::session::SessionEntry;
use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Session management subcommands
///
/// Sessions store the conversation history with an agent.
/// Most commands work offline by reading directly from the filesystem.
///
/// Examples:
///   # List sessions for an agent (offline)
///   pekobot session list myagent
///   pekobot session list myteam/myagent
///
///   # Show session details with history (offline)
///   pekobot session show myagent sess_xxx --history
///   pekobot session show myteam/myagent sess_xxx --history
///
///   # Create a branch of a session (offline)
///   pekobot session branch myagent sess_xxx --label "experiment"
///
///   # Delete a session (offline)
///   pekobot session delete myagent sess_xxx
///
///   # Switch active session (offline, updates preference)
///   pekobot session switch myagent sess_xxx
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum SessionCommands {
    /// List sessions for an agent (offline)
    List {
        /// Agent name or team/agent format
        agent: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Show all sessions including inactive
        #[arg(long)]
        all: bool,
    },

    /// Show session details and history (offline)
    Show {
        /// Agent name or team/agent format
        agent: String,
        /// Session ID
        session_id: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Show full message history
        #[arg(long)]
        history: bool,
    },

    /// Branch a session (offline - copies session files)
    Branch {
        /// Agent name or team/agent format
        agent: String,
        /// Session ID to branch from
        session_id: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Optional label for the new session
        #[arg(short, long)]
        label: Option<String>,
    },

    /// Delete a session (offline - removes session files)
    Delete {
        /// Agent name or team/agent format
        agent: String,
        /// Session ID to delete
        session_id: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Switch active session (offline - updates preference file)
    Switch {
        /// Agent name or team/agent format
        agent: String,
        /// Session ID to activate
        session_id: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
    },

    /// Send message to a session (requires daemon)
    ///
    /// **DEPRECATED:** Use `pekobot send <agent> <message>` instead.
    Send {
        /// Instance ID (must be running)
        instance_id: String,
        /// Session ID (optional - uses active session if not provided)
        #[arg(short, long)]
        session_id: Option<String>,
        /// Message content
        message: String,
    },
}

/// Handle session commands
pub async fn handle_session(
    cmd: SessionCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        SessionCommands::List {
            agent,
            team,
            all: _,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            list_sessions(paths, team, agent_name, json).await
        }
        SessionCommands::Show {
            agent,
            session_id,
            team,
            history,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            show_session(paths, team, agent_name, &session_id, history, json).await
        }
        SessionCommands::Branch {
            agent,
            session_id,
            team,
            label,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            branch_session(paths, team, agent_name, &session_id, label, json).await
        }
        SessionCommands::Delete {
            agent,
            session_id,
            team,
            force,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            delete_session(paths, team, agent_name, &session_id, force, json).await
        }
        SessionCommands::Switch {
            agent,
            session_id,
            team,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            switch_session(paths, team, agent_name, &session_id, json).await
        }
        SessionCommands::Send {
            instance_id,
            session_id,
            message,
        } => send_message(&instance_id, session_id, &message).await,
    }
}

// ================================================================================
// Path Resolution
// ================================================================================

/// Result of locating an agent's sessions
struct AgentLocation {
    /// Sessions directory path
    sessions_dir: PathBuf,
}

/// Locate an agent's sessions directory
///
/// Uses PathResolver to get the correct sessions directory.
/// Structure: `{data_dir}/sessions/{team}/{agent}/`
fn locate_agent(paths: &GlobalPaths, name: &str, team: &str) -> Option<AgentLocation> {
    let sessions_dir = paths.resolver().agent_sessions_dir(name, Some(team));

    if sessions_dir.exists() {
        return Some(AgentLocation { sessions_dir });
    }

    None
}

/// Ensure sessions directory exists
async fn ensure_sessions_dir(paths: &GlobalPaths, name: &str, team: &str) -> Result<AgentLocation> {
    if let Some(loc) = locate_agent(paths, name, team) {
        Ok(loc)
    } else {
        // Create using PathResolver (in data_dir)
        let sessions_dir = paths.resolver().agent_sessions_dir(name, Some(team));
        tokio::fs::create_dir_all(&sessions_dir).await?;
        Ok(AgentLocation { sessions_dir })
    }
}

// ================================================================================
// Session Index (Centralized) Operations
// ================================================================================

/// List all sessions from centralized index
/// Syncs message counts from JSONL (source of truth) before returning
async fn list_sessions_from_disk(sessions_dir: &PathBuf) -> Result<Vec<SessionEntry>> {
    let mut controller = MetadataController::new(sessions_dir);
    // Sync from JSONL to ensure message counts are accurate
    let metadata_list = controller.list_metadata(true).await?;
    Ok(metadata_list.into_iter().map(|m| m.to_entry()).collect())
}

// ================================================================================
// Active Session Preference
// ================================================================================

/// Active session preference file structure
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ActiveSessionPreference {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    set_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    set_by: Option<String>,
}

/// Path to the active session preference file
fn active_session_path(sessions_dir: &PathBuf) -> PathBuf {
    sessions_dir.join(".active.json")
}

/// Load preferred active session
async fn load_active_preference(sessions_dir: &PathBuf) -> Result<Option<String>> {
    let path = active_session_path(sessions_dir);
    if !path.exists() {
        return Ok(None);
    }
    let content = tokio::fs::read_to_string(&path).await?;
    let pref: ActiveSessionPreference = serde_json::from_str(&content)?;
    Ok(Some(pref.session_id))
}

/// Save preferred active session
async fn save_active_preference(sessions_dir: &PathBuf, session_id: &str) -> Result<()> {
    let path = active_session_path(sessions_dir);
    let pref = ActiveSessionPreference {
        session_id: session_id.to_string(),
        set_at: Some(chrono::Utc::now().to_rfc3339()),
        set_by: Some("cli".to_string()),
    };
    let json = serde_json::to_string_pretty(&pref)?;

    // Atomic write
    let temp_path = path.with_extension("tmp");
    tokio::fs::write(&temp_path, json).await?;
    tokio::fs::rename(&temp_path, &path).await?;

    Ok(())
}

// ================================================================================
// Command Implementations
// ================================================================================

/// List sessions for an agent (offline)
async fn list_sessions(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    json: bool,
) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent, team) else {
        if json {
            println!("[]");
        } else {
            println!(
                "📭 Agent '{}' not found in team '{}' or has no sessions.",
                agent, team
            );
            println!(
                "   Create the agent first with: pekobot agent create {}/{}",
                team, agent
            );
        }
        return Ok(());
    };

    let sessions = list_sessions_from_disk(&loc.sessions_dir).await?;
    let active_pref = load_active_preference(&loc.sessions_dir)
        .await
        .ok()
        .flatten();

    if json {
        let output = serde_json::json!({
            "team": team,
            "agent": agent,
            "sessions": sessions,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if sessions.is_empty() {
        println!("📭 No sessions found for '{}'.", agent);
        println!("   Start chatting with the agent to create sessions.");
    } else {
        println!(
            "📋 Sessions for {}/{} ({} found):",
            team,
            agent,
            sessions.len()
        );
        if let Some(ref pref) = active_pref {
            println!("   Preferred active: {}", pref);
        }
        println!();

        for session in &sessions {
            let title = session.title.as_deref().unwrap_or("(untitled)");
            let created = format_timestamp_ms(session.created_at);
            let updated = format_timestamp_ms(session.updated_at);

            // Status indicators
            let is_preferred = active_pref.as_ref() == Some(&session.session_id);
            let status_icon = if session.ended {
                "🔴"
            } else if is_preferred {
                "⭐"
            } else {
                "🟢"
            };

            println!("  {} {}", status_icon, session.session_id);
            println!("     Title: {}", title);
            println!(
                "     Messages: {} | Window: {} | In: {} | Out: {}",
                session.message_count,
                session.context_window,
                session.total_input_tokens,
                session.total_output_tokens
            );
            println!("     Created: {} | Updated: {}", created, updated);

            if let Some(ref parent) = session.parent_session_id {
                println!("     Branched from: {}", parent);
            }
            println!();
        }
    }

    Ok(())
}

/// Show session details (offline)
async fn show_session(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    session_id: &str,
    show_history: bool,
    json: bool,
) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent, team) else {
        return Err(anyhow::anyhow!(
            "Agent '{}' not found in team '{}'",
            agent,
            team
        ));
    };

    // Load session entry from centralized index with sync from JSONL
    let mut controller = MetadataController::new(&loc.sessions_dir);
    let Some(metadata) = controller.get_metadata(session_id, true).await? else {
        return Err(anyhow::anyhow!(
            "Session '{}' not found for agent '{}'",
            session_id,
            agent
        ));
    };
    let entry = metadata.to_entry();

    // Load history if requested
    let history_events = if show_history {
        load_session_history(paths, team, agent, session_id)
            .await
            .ok()
    } else {
        None
    };

    if json {
        let output = serde_json::json!({
            "session": entry,
            "history": history_events,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("📊 Session Details");
        println!("   Team: {}", team);
        println!("   Agent: {}", agent);
        println!("   Session ID: {}", entry.session_id);
        if let Some(ref title) = entry.title {
            println!("   Title: {}", title);
        }
        println!(
            "   Status: {}",
            if entry.ended {
                "Ended 🔴"
            } else {
                "Active 🟢"
            }
        );
        if !entry.trigger.is_empty() {
            println!("   Trigger: {}", entry.trigger);
        }
        println!(
            "   Messages: {} | Window: {} | In: {} | Out: {}",
            entry.message_count,
            entry.context_window,
            entry.total_input_tokens,
            entry.total_output_tokens
        );
        println!("   Created: {}", format_timestamp_ms(entry.created_at));
        println!("   Updated: {}", format_timestamp_ms(entry.updated_at));

        if let Some(ref parent) = entry.parent_session_id {
            println!("   Parent Session: {}", parent);
        }

        if let Some(events) = history_events {
            println!();
            println!("📜 Message History ({} events):", events.len());
            println!();

            for (i, event) in events.iter().enumerate() {
                print_history_event(i, event);
            }
        } else if show_history {
            println!();
            println!("   ⚠️  Failed to load history from JSONL file");
        } else {
            println!();
            println!("   💡 Use --history to see full message history");
        }
    }

    Ok(())
}

/// History display entry - represents a parsed JSONL entry for display
#[derive(Debug, Serialize)]
enum HistoryDisplayEntry {
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

/// Load session history using shared backend service
///
/// Uses SessionService for consistent parsing with API layer.
/// Presentation layer converts service DTOs to CLI display format.
async fn load_session_history(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    session_id: &str,
) -> Result<Vec<HistoryDisplayEntry>> {
    let service = SessionService::new(paths.resolver().clone());
    let result = service
        .get_history(agent, Some(team), session_id, HistoryQuery::default())
        .await?;

    // Convert service DTOs to CLI display format (presentation layer)
    let display_entries: Vec<HistoryDisplayEntry> = result
        .events
        .into_iter()
        .filter_map(history_event_to_display)
        .collect();

    Ok(display_entries)
}

/// Convert service HistoryEvent to CLI display format
///
/// This is presentation layer logic - different channels (CLI, API, Web)
/// can format the same backend data differently.
fn history_event_to_display(event: HistoryEvent) -> Option<HistoryDisplayEntry> {
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
        HistoryEvent::ToolCall { tool_name, args, .. } => {
            // Display tool calls with their arguments
            let content = format!("{}", args);
            Some(HistoryDisplayEntry::ToolResult {
                tool_name,
                content,
            })
        }
        HistoryEvent::Thinking { content } => Some(HistoryDisplayEntry::Message {
            role: "thinking".to_string(),
            content,
            timestamp: String::new(), // Thinking events don't have timestamp in current format
        }),
        HistoryEvent::Compaction { summary } => Some(HistoryDisplayEntry::Compaction { summary }),
        HistoryEvent::Custom { custom_type } => {
            Some(HistoryDisplayEntry::Custom { custom_type })
        }
    }
}

/// Print a history event
fn print_history_event(index: usize, event: &HistoryDisplayEntry) {
    match event {
        HistoryDisplayEntry::Session { timestamp } => {
            println!(
                "  [{}] 🆕 Session Created ({})",
                index,
                format_timestamp(timestamp)
            );
        }
        HistoryDisplayEntry::Message {
            role,
            content,
            timestamp,
        } => {
            let time_str = format_timestamp(timestamp);
            match role.as_str() {
                "system" => {
                    println!("  [{}] ⚙️  System ({}):", index, time_str);
                    println!("      {}", truncate(content, 200));
                }
                "user" => {
                    println!("  [{}] 👤 User ({}):", index, time_str);
                    println!("      {}", truncate(content, 200));
                }
                "assistant" => {
                    println!("  [{}] 🤖 Assistant ({}):", index, time_str);
                    println!("      {}", truncate(content, 200));
                }
                "tool" => {
                    println!("  [{}] 🔧 Tool ({}):", index, time_str);
                    println!("      {}", truncate(content, 200));
                }
                _ => {
                    println!("  [{}] {} ({}):", index, role, time_str);
                    println!("      {}", truncate(content, 200));
                }
            }
        }
        HistoryDisplayEntry::ToolResult { tool_name, content } => {
            println!("  [{}] ✅ Tool Result:", index);
            println!("      Tool: {}", tool_name);
            if !content.is_empty() {
                println!("      Output: {}", truncate(content, 150));
            }
        }
        HistoryDisplayEntry::ModelChange { provider, model_id } => {
            println!("  [{}] 🔄 Model Change:", index);
            println!("      Provider: {}, Model: {}", provider, model_id);
        }
        HistoryDisplayEntry::Compaction { summary } => {
            println!("  [{}] 📦 Compaction:", index);
            println!("      {}", truncate(summary, 150));
        }
        HistoryDisplayEntry::Custom { custom_type } => {
            println!("  [{}] 📎 Custom ({})", index, custom_type);
        }
    }
}

/// Branch a session (offline)
async fn branch_session(
    _paths: &GlobalPaths,
    team: &str,
    agent: &str,
    session_id: &str,
    label: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    // Use SessionManager for the branch operation
    let mut manager =
        crate::session::SessionManager::for_cli(_paths.resolver.clone(), agent, Some(team));

    // Verify parent session exists
    let _parent_metadata = manager
        .get_session_metadata(session_id)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Parent session '{}' not found for agent '{}'",
                session_id,
                agent
            )
        })?;

    // Perform the branch using SessionManager
    let new_session_id = manager
        .branch_session_by_id(session_id, label.clone())
        .await?;

    // Get the new session metadata for display
    let new_metadata = manager.get_session_metadata(&new_session_id).await?;
    let entry = new_metadata.to_entry();

    if json {
        println!("{}", serde_json::to_string_pretty(&entry)?);
    } else {
        println!("✅ Branched session '{}'", session_id);
        println!("   New Session ID: {}", new_session_id);
        println!("   Parent Session: {}", session_id);
        if let Some(ref lbl) = label {
            println!("   Label: {}", lbl);
        }
        println!();
        println!("   The branched session contains a copy of the parent's history.");
        println!(
            "   Switch to it with: pekobot session switch {}/{} {}",
            team, agent, new_session_id
        );
    }

    Ok(())
}

/// Delete a session (offline)
async fn delete_session(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    session_id: &str,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent, team) else {
        return Err(anyhow::anyhow!(
            "Agent '{}' not found in team '{}'",
            agent,
            team
        ));
    };

    // Use MetadataController for delete operation
    let mut controller = crate::session::MetadataController::new(&loc.sessions_dir);

    // Check if session exists and get metadata for confirmation
    let metadata = match controller.get_metadata_fast(session_id).await? {
        Some(m) => Some(m),
        None => {
            // Check if JSONL file exists without metadata (orphaned)
            let jsonl_path = loc.sessions_dir.join(format!("{}.jsonl", session_id));
            if !jsonl_path.exists() {
                return Err(anyhow::anyhow!(
                    "Session '{}' not found for agent '{}'",
                    session_id,
                    agent
                ));
            }
            None
        }
    };

    if !force {
        println!("⚠️  This will permanently delete session '{}'.", session_id);
        if let Some(ref meta) = metadata {
            if let Some(ref title) = meta.title {
                println!("   Title: {}", title);
            }
            println!("   Messages: {}", meta.message_count);
        }
        println!("   This action cannot be undone.");
        print!("   Continue? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Delete via MetadataController (handles both file and metadata)
    let deleted = controller.delete_session(session_id).await?;

    // Check if this was the preferred active session
    if let Ok(Some(pref)) = load_active_preference(&loc.sessions_dir).await {
        if pref == session_id {
            let _ = tokio::fs::remove_file(active_session_path(&loc.sessions_dir)).await;
        }
    }

    if json {
        let deleted_items = if deleted {
            vec!["jsonl", "index"]
        } else {
            vec![]
        };
        println!("{{\"success\": true, \"deleted\": {:?}}}", deleted_items);
    } else {
        println!("✅ Deleted session '{}'", session_id);
        if deleted {
            println!("   Removed: jsonl, index");
        }
    }

    Ok(())
}

/// Switch active session (offline - updates preference file)
async fn switch_session(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let loc = ensure_sessions_dir(paths, agent, team).await?;

    // Verify session exists in centralized index
    let mut controller = MetadataController::new(&loc.sessions_dir);
    let entry = controller
        .get_entry_from_index(session_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("Session '{}' not found for agent '{}'", session_id, agent)
        })?;

    // Check if session is ended
    if entry.ended {
        println!("⚠️  Warning: Session '{}' is ended.", session_id);
        println!("   Switching to an ended session will start a new conversation");
        println!("   with the same history available for reference.");
    }

    // Save preference
    save_active_preference(&loc.sessions_dir, session_id).await?;

    if json {
        println!(
            "{{\"success\": true, \"session_id\": \"{}\", \"agent\": \"{}\", \"team\": \"{}\"}}",
            session_id, agent, team
        );
    } else {
        println!(
            "✅ Set preferred active session for '{}'/{} to '{}'",
            team, agent, session_id
        );
        println!();
        println!("   This session will be activated when the agent starts.");
    }

    Ok(())
}

/// Send message to a session (requires daemon)
///
/// **DEPRECATED:** `session send` is deprecated. Use `pekobot send <agent> <message>` instead.
async fn send_message(
    instance_id: &str,
    session_id: Option<String>,
    message: &str,
) -> anyhow::Result<()> {
    // Show deprecation warning
    eprintln!("⚠️  Warning: 'pekobot session send' is deprecated.");
    eprintln!("   Use 'pekobot send <agent> \"<message>\"' instead.");
    eprintln!();

    println!(
        "📤 Sending message to instance '{}': {}",
        instance_id, message
    );
    if let Some(sid) = session_id {
        println!("   Session: {}", sid);
    } else {
        println!("   (Using active session)");
    }

    println!();
    println!("   💡 Message sending requires the daemon to be running.");
    println!("      Start the daemon with: pekobot daemon start --foreground");
    println!();
    println!(
        "      Or use: pekobot agent start {} --message \"{}\"",
        instance_id, message
    );

    Ok(())
}

// ================================================================================
// Utilities
// ================================================================================

/// Format an ISO timestamp for display
fn format_timestamp(ts: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        dt.format("%Y-%m-%d %H:%M").to_string()
    } else {
        ts.to_string()
    }
}

/// Format a millisecond timestamp (unix epoch) for display
fn format_timestamp_ms(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as i64;
    let nanos = ((ts_ms % 1000) * 1_000_000) as u32;
    if let Some(dt) = chrono::DateTime::from_timestamp(secs, nanos) {
        dt.format("%Y-%m-%d %H:%M").to_string()
    } else {
        format!("{}", ts_ms)
    }
}

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
    fn test_format_timestamp() {
        assert_eq!(
            format_timestamp("2026-03-17T10:30:00+00:00"),
            "2026-03-17 10:30"
        );
        assert_eq!(format_timestamp("invalid"), "invalid");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a very long string", 10), "this is a ...");
    }
}
