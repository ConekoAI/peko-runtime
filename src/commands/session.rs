//! Session Management Commands
//!
//! These commands follow an offline-first design per ADR-013:
//! - Read operations (list, show) work directly from the filesystem
//! - File operations (branch, delete) modify files directly
//! - Only commands needing runtime coordination (send, switch on running instances) use the API
//!
//! Session storage layout:
//! - `{data_dir}/agents/{instance_id}/sessions/*.jsonl` - Session event logs
//! - `{data_dir}/agents/{instance_id}/sessions/*.index.json` - Metadata sidecars
//! - `{data_dir}/agents/{instance_id}/sessions/.active.json` - Preferred active session (CLI-managed)

use crate::commands::GlobalPaths;
use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use uuid::Uuid;

/// Session management subcommands
///
/// Sessions store the conversation history with an agent.
/// Most commands work offline by reading directly from the filesystem.
///
/// Examples:
///   # List sessions for an agent (offline)
///   pekobot session list myagent
///
///   # Show session details with history (offline)
///   pekobot session show myagent sess_xxx --history
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
        /// Agent name or instance ID
        agent: String,
        /// Show all sessions including inactive
        #[arg(long)]
        all: bool,
    },

    /// Show session details and history (offline)
    Show {
        /// Agent name or instance ID
        agent: String,
        /// Session ID
        session_id: String,
        /// Show full message history
        #[arg(long)]
        history: bool,
    },

    /// Branch a session (offline - copies session files)
    Branch {
        /// Agent name or instance ID
        agent: String,
        /// Session ID to branch from
        session_id: String,
        /// Optional label for the new session
        #[arg(short, long)]
        label: Option<String>,
    },

    /// Delete a session (offline - removes session files)
    Delete {
        /// Agent name or instance ID
        agent: String,
        /// Session ID to delete
        session_id: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Switch active session (offline - updates preference file)
    Switch {
        /// Agent name or instance ID
        agent: String,
        /// Session ID to activate
        session_id: String,
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
        SessionCommands::List { agent, all: _ } => list_sessions(paths, &agent, json).await,
        SessionCommands::Show {
            agent,
            session_id,
            history,
        } => show_session(paths, &agent, &session_id, history, json).await,
        SessionCommands::Branch {
            agent,
            session_id,
            label,
        } => branch_session(paths, &agent, &session_id, label, json).await,
        SessionCommands::Delete {
            agent,
            session_id,
            force,
        } => delete_session(paths, &agent, &session_id, force, json).await,
        SessionCommands::Switch { agent, session_id } => {
            switch_session(paths, &agent, &session_id, json).await
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

/// Result of locating an agent's workspace
struct AgentLocation {
    /// Directory containing the sessions/ subdirectory
    workspace: PathBuf,
    /// Sessions directory path
    sessions_dir: PathBuf,
    /// Whether this is a running instance (data dir) or config
    is_runtime: bool,
}

/// Locate an agent's sessions directory
///
/// Searches in order:
/// 1. Runtime data directory (for running instances)
/// 2. Team config directory
/// 3. Standalone agent config directory
fn locate_agent(paths: &GlobalPaths, name: &str) -> Option<AgentLocation> {
    // 1. Try runtime data directory (running instances)
    let runtime = paths.data_dir.join("agents").join(name);
    let runtime_sessions = runtime.join("sessions");
    if runtime_sessions.exists() {
        return Some(AgentLocation {
            workspace: runtime,
            sessions_dir: runtime_sessions,
            is_runtime: true,
        });
    }

    // 2. Try team config directory (default team)
    let team = paths
        .config_dir
        .join("teams")
        .join("default")
        .join("agents")
        .join(name);
    let team_sessions = team.join("sessions");
    if team_sessions.exists() {
        return Some(AgentLocation {
            workspace: team,
            sessions_dir: team_sessions,
            is_runtime: false,
        });
    }

    // 3. Try standalone agent config
    let standalone = paths.agents_dir(None).join(name);
    let standalone_sessions = standalone.join("sessions");
    if standalone_sessions.exists() {
        return Some(AgentLocation {
            workspace: standalone,
            sessions_dir: standalone_sessions,
            is_runtime: false,
        });
    }

    None
}

/// Ensure sessions directory exists
async fn ensure_sessions_dir(paths: &GlobalPaths, name: &str) -> Result<AgentLocation> {
    if let Some(loc) = locate_agent(paths, name) {
        Ok(loc)
    } else {
        // Create in runtime data directory
        let runtime = paths.data_dir.join("agents").join(name);
        let sessions = runtime.join("sessions");
        tokio::fs::create_dir_all(&sessions).await?;
        Ok(AgentLocation {
            workspace: runtime,
            sessions_dir: sessions,
            is_runtime: true,
        })
    }
}

// ================================================================================
// Session Index (Sidecar) Operations
// ================================================================================

/// Session index sidecar structure
#[derive(Debug, Clone, Deserialize, Serialize)]
struct SessionIndex {
    session_id: String,
    instance_id: String,
    created_at: String,
    updated_at: String,
    turn_count: u32,
    #[serde(default)]
    event_count: u64,
    #[serde(default)]
    total_tokens: u32,
    #[serde(default)]
    parent_session_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    ended: bool,
    #[serde(default)]
    trigger: String,
}

/// Load session index from sidecar file
async fn load_session_index(
    sessions_dir: &PathBuf,
    session_id: &str,
) -> Result<Option<SessionIndex>> {
    let path = sessions_dir.join(format!("{}.index.json", session_id));
    if !path.exists() {
        return Ok(None);
    }
    let content = tokio::fs::read_to_string(&path).await?;
    let index: SessionIndex = serde_json::from_str(&content)?;
    Ok(Some(index))
}

/// List all sessions from disk
async fn list_sessions_from_disk(sessions_dir: &PathBuf) -> Result<Vec<SessionIndex>> {
    let mut sessions = vec![];

    let mut entries = tokio::fs::read_dir(sessions_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if let Some(name) = entry.file_name().to_str() {
            if name.ends_with(".index.json") && !name.starts_with('.') {
                let session_id = name.trim_end_matches(".index.json").to_string();
                if let Ok(Some(index)) = load_session_index(sessions_dir, &session_id).await {
                    sessions.push(index);
                }
            }
        }
    }

    // Sort by updated_at descending (most recent first)
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    Ok(sessions)
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
async fn list_sessions(paths: &GlobalPaths, agent: &str, json: bool) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent) else {
        if json {
            println!("[]");
        } else {
            println!("📭 Agent '{}' not found or has no sessions.", agent);
            println!(
                "   Create the agent first with: pekobot agent create {}",
                agent
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
        println!("{}", serde_json::to_string_pretty(&sessions)?);
    } else if sessions.is_empty() {
        println!("📭 No sessions found for '{}'.", agent);
        println!("   Start chatting with the agent to create sessions.");
    } else {
        println!("📋 Sessions for '{}' ({} found):", agent, sessions.len());
        if let Some(ref pref) = active_pref {
            println!("   Preferred active: {}", pref);
        }
        println!();

        for session in &sessions {
            let title = session.title.as_deref().unwrap_or("(untitled)");
            let created = format_timestamp(&session.created_at);
            let updated = format_timestamp(&session.updated_at);

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
                "     Turns: {} | Events: {} | Tokens: {}",
                session.turn_count, session.event_count, session.total_tokens
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
    agent: &str,
    session_id: &str,
    show_history: bool,
    json: bool,
) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent) else {
        return Err(anyhow::anyhow!("Agent '{}' not found", agent));
    };

    // Load session index
    let Some(index) = load_session_index(&loc.sessions_dir, session_id).await? else {
        return Err(anyhow::anyhow!(
            "Session '{}' not found for agent '{}'",
            session_id,
            agent
        ));
    };

    // Load history if requested
    let history_events = if show_history {
        load_session_history(&loc.sessions_dir, session_id)
            .await
            .ok()
    } else {
        None
    };

    if json {
        let output = serde_json::json!({
            "session": index,
            "history": history_events,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("📊 Session Details");
        println!("   Agent: {}", agent);
        println!("   Session ID: {}", index.session_id);
        if let Some(ref title) = index.title {
            println!("   Title: {}", title);
        }
        println!(
            "   Status: {}",
            if index.ended {
                "Ended 🔴"
            } else {
                "Active 🟢"
            }
        );
        println!("   Trigger: {}", index.trigger);
        println!(
            "   Turns: {} | Events: {} | Tokens: {}",
            index.turn_count, index.event_count, index.total_tokens
        );
        println!("   Created: {}", format_timestamp(&index.created_at));
        println!("   Updated: {}", format_timestamp(&index.updated_at));

        if let Some(ref parent) = index.parent_session_id {
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

/// History event from JSONL
#[derive(Debug, Deserialize, Serialize)]
struct HistoryEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ts: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Value,
}

/// Load session history from JSONL file
async fn load_session_history(
    sessions_dir: &PathBuf,
    session_id: &str,
) -> Result<Vec<HistoryEvent>> {
    let path = sessions_dir.join(format!("{}.jsonl", session_id));
    if !path.exists() {
        return Ok(vec![]);
    }

    let content = tokio::fs::read_to_string(&path).await?;
    let mut events = vec![];

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<HistoryEvent>(line) {
            events.push(event);
        }
    }

    Ok(events)
}

/// Print a history event
fn print_history_event(index: usize, event: &HistoryEvent) {
    let event_type = &event.event_type;
    let timestamp = event
        .ts
        .as_deref()
        .map(format_timestamp)
        .unwrap_or_default();

    match event_type.as_str() {
        "user.message" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 👤 User ({}):", index, timestamp);
                println!("      {}", truncate(content, 200));
            }
        }
        "assistant.message" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 🤖 Assistant ({}):", index, timestamp);
                println!("      {}", truncate(content, 200));
            }
        }
        "thinking" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 💭 Thinking ({}):", index, timestamp);
                println!("      {}", truncate(content, 150));
            }
        }
        "tool.call" => {
            println!("  [{}] 🔧 Tool Call ({})", index, timestamp);
            if let Some(tool) = event.extra.get("tool").and_then(|v| v.as_str()) {
                println!("      Tool: {}", tool);
            }
        }
        "tool.result" => {
            println!("  [{}] ✅ Tool Result ({})", index, timestamp);
            if let Some(output) = event.extra.get("output").and_then(|v| v.as_str()) {
                println!("      Output: {}", truncate(output, 150));
            }
            if let Some(error) = event.extra.get("error").and_then(|v| v.as_str()) {
                println!("      Error: {}", error);
            }
        }
        "session.created" => {
            println!("  [{}] 🆕 Session Created ({})", index, timestamp);
        }
        "session.ended" => {
            println!("  [{}] 🏁 Session Ended ({})", index, timestamp);
        }
        _ => {
            println!("  [{}] {} ({})", index, event_type, timestamp);
        }
    }
}

/// Branch a session (offline)
async fn branch_session(
    paths: &GlobalPaths,
    agent: &str,
    session_id: &str,
    label: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let loc = ensure_sessions_dir(paths, agent).await?;

    // Verify parent session exists
    let parent_index = load_session_index(&loc.sessions_dir, session_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Parent session '{}' not found for agent '{}'",
                session_id,
                agent
            )
        })?;

    // Generate new session ID
    let new_session_id = format!("sess_{}", Uuid::new_v4().simple());

    // Copy parent JSONL file
    let parent_jsonl = loc.sessions_dir.join(format!("{}.jsonl", session_id));
    let new_jsonl = loc.sessions_dir.join(format!("{}.jsonl", new_session_id));

    if parent_jsonl.exists() {
        tokio::fs::copy(&parent_jsonl, &new_jsonl).await?;
    } else {
        // Create empty JSONL with just session.created event
        create_empty_session(&new_jsonl, &new_session_id, agent, Some(session_id)).await?;
    }

    // Create branched sidecar index
    let new_index = SessionIndex {
        session_id: new_session_id.clone(),
        instance_id: parent_index.instance_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        turn_count: parent_index.turn_count,
        event_count: parent_index.event_count,
        total_tokens: parent_index.total_tokens,
        parent_session_id: Some(session_id.to_string()),
        title: label.clone().or_else(|| {
            parent_index
                .title
                .as_ref()
                .map(|t| format!("Branch: {}", t))
        }),
        ended: false,
        trigger: "branch".to_string(),
    };

    let sidecar_path = loc
        .sessions_dir
        .join(format!("{}.index.json", new_session_id));
    let sidecar_json = serde_json::to_string_pretty(&new_index)?;

    // Atomic write
    let temp_path = sidecar_path.with_extension("tmp");
    tokio::fs::write(&temp_path, sidecar_json).await?;
    tokio::fs::rename(&temp_path, &sidecar_path).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&new_index)?);
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
            "   Switch to it with: pekobot session switch {} {}",
            agent, new_session_id
        );
    }

    Ok(())
}

/// Create an empty session JSONL with session.created event
async fn create_empty_session(
    path: &PathBuf,
    session_id: &str,
    agent: &str,
    parent_id: Option<&str>,
) -> Result<()> {
    use chrono::Utc;

    let created_event = serde_json::json!({
        "id": "evt_001",
        "type": "session.created",
        "session_id": session_id,
        "ts": Utc::now().to_rfc3339(),
        "seq": 1,
        "instance_id": agent,
        "parent_session_id": parent_id,
        "trigger": if parent_id.is_some() { "branch" } else { "user" }
    });

    let json = serde_json::to_string(&created_event)?;
    tokio::fs::write(path, json + "\n").await?;

    Ok(())
}

/// Delete a session (offline)
async fn delete_session(
    paths: &GlobalPaths,
    agent: &str,
    session_id: &str,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent) else {
        return Err(anyhow::anyhow!("Agent '{}' not found", agent));
    };

    // Verify session exists
    let jsonl_path = loc.sessions_dir.join(format!("{}.jsonl", session_id));
    let sidecar_path = loc.sessions_dir.join(format!("{}.index.json", session_id));

    if !jsonl_path.exists() && !sidecar_path.exists() {
        return Err(anyhow::anyhow!(
            "Session '{}' not found for agent '{}'",
            session_id,
            agent
        ));
    }

    // Load metadata for display
    let metadata = load_session_index(&loc.sessions_dir, session_id)
        .await
        .ok()
        .flatten();

    if !force {
        println!("⚠️  This will permanently delete session '{}'.", session_id);
        if let Some(ref meta) = metadata {
            if let Some(ref title) = meta.title {
                println!("   Title: {}", title);
            }
            println!(
                "   Turns: {} | Events: {}",
                meta.turn_count, meta.event_count
            );
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

    // Delete files
    let mut deleted = vec![];
    if jsonl_path.exists() {
        tokio::fs::remove_file(&jsonl_path).await?;
        deleted.push("jsonl");
    }
    if sidecar_path.exists() {
        tokio::fs::remove_file(&sidecar_path).await?;
        deleted.push("index");
    }

    // Check if this was the preferred active session
    if let Ok(Some(pref)) = load_active_preference(&loc.sessions_dir).await {
        if pref == session_id {
            let _ = tokio::fs::remove_file(active_session_path(&loc.sessions_dir)).await;
        }
    }

    if json {
        println!("{{\"success\": true, \"deleted\": {:?}}}", deleted);
    } else {
        println!("✅ Deleted session '{}'", session_id);
        if !deleted.is_empty() {
            println!("   Removed: {}", deleted.join(", "));
        }
    }

    Ok(())
}

/// Switch active session (offline - updates preference file)
async fn switch_session(
    paths: &GlobalPaths,
    agent: &str,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let loc = ensure_sessions_dir(paths, agent).await?;

    // Verify session exists
    let sidecar_path = loc.sessions_dir.join(format!("{}.index.json", session_id));
    if !sidecar_path.exists() {
        return Err(anyhow::anyhow!(
            "Session '{}' not found for agent '{}'",
            session_id,
            agent
        ));
    }

    // Check if session is ended
    let is_ended = load_session_index(&loc.sessions_dir, session_id)
        .await?
        .map(|i| i.ended)
        .unwrap_or(false);

    if is_ended {
        println!("⚠️  Warning: Session '{}' is ended.", session_id);
        println!("   Switching to an ended session will start a new conversation");
        println!("   with the same history available for reference.");
    }

    // Save preference
    save_active_preference(&loc.sessions_dir, session_id).await?;

    if json {
        println!(
            "{{\"success\": true, \"session_id\": \"{}\", \"agent\": \"{}\"}}",
            session_id, agent
        );
    } else {
        println!(
            "✅ Set preferred active session for '{}' to '{}'",
            agent, session_id
        );
        println!();
        if loc.is_runtime {
            println!("   💡 The agent is currently running.");
            println!("      Restart the agent to use this session, or use the API directly.");
        } else {
            println!("   This session will be activated when the agent starts.");
        }
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
