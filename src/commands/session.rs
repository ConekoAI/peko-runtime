//! Session Management Commands
//!
//! These commands can work in two modes:
//! 1. Online: Communicate with the daemon via HTTP API (for active instances)
//! 2. Offline: Read directly from disk (for inactive instances or when daemon is down)

use crate::api::client::{ApiClient, ClientError};
use crate::commands::GlobalPaths;
use anyhow::Result;
use clap::Subcommand;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::debug;

/// Session management subcommands
///
/// Sessions store the conversation history with an agent.
/// Each instance can have multiple sessions, but only one active session at a time.
///
/// Examples:
///   # List sessions for an instance
///   pekobot session list INSTANCE_ID
///
///   # Show session details with history
///   pekobot session show INSTANCE_ID SESSION_ID --history
///
///   # Create a branch of a session
///   pekobot session branch INSTANCE_ID SESSION_ID --label "experiment"
///
///   # Send a message to a specific session
///   pekobot session send INSTANCE_ID "Hello" --session-id SESSION_ID
///
///   # Delete a session
///   pekobot session delete INSTANCE_ID SESSION_ID
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum SessionCommands {
    /// List sessions for an instance
    List {
        /// Instance ID (agent name for local agents)
        instance_id: String,
        /// Show all sessions including inactive
        #[arg(long)]
        all: bool,
    },

    /// Show session details and history
    Show {
        /// Instance ID (agent name for local agents)
        instance_id: String,
        /// Session ID
        session_id: String,
        /// Show full message history
        #[arg(long)]
        history: bool,
    },

    /// Branch a session (requires daemon)
    Branch {
        /// Instance ID
        instance_id: String,
        /// Session ID to branch from
        session_id: String,
        /// Optional label for the new session
        #[arg(short, long)]
        label: Option<String>,
    },

    /// Delete a session (requires daemon)
    Delete {
        /// Instance ID
        instance_id: String,
        /// Session ID to delete
        session_id: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Send message to a session (requires daemon)
    Send {
        /// Instance ID
        instance_id: String,
        /// Session ID (optional - uses active session if not provided)
        #[arg(short, long)]
        session_id: Option<String>,
        /// Message content
        message: String,
    },

    /// Switch active session for an instance (requires daemon)
    Switch {
        /// Instance ID
        instance_id: String,
        /// Session ID to activate
        session_id: String,
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
            instance_id,
            all: _,
        } => list_sessions(paths, &instance_id, json).await,
        SessionCommands::Show {
            instance_id,
            session_id,
            history,
        } => show_session(paths, &instance_id, &session_id, history, json).await,
        SessionCommands::Branch {
            instance_id,
            session_id,
            label,
        } => branch_session(&instance_id, &session_id, label, json).await,
        SessionCommands::Delete {
            instance_id,
            session_id,
            force,
        } => delete_session(&instance_id, &session_id, force, json).await,
        SessionCommands::Send {
            instance_id,
            session_id,
            message,
        } => {
            println!("📤 Sending message to instance '{}': {}", instance_id, message);
            if let Some(sid) = session_id {
                println!("   Session: {}", sid);
                println!();
                println!("   💡 Use 'pekobot agent start {} --message \"{}\"' to send a message", 
                    instance_id, message);
            } else {
                println!("   (Using active session)");
                println!("   💡 Use --session-id to target a specific session");
            }
            Ok(())
        }
        SessionCommands::Switch {
            instance_id,
            session_id,
        } => {
            println!("🔄 Switching active session for '{}' to '{}'", instance_id, session_id);
            println!();
            println!("   💡 This feature requires the session switch API endpoint.");
            println!("   For now, restart the agent with the desired session.");
            Ok(())
        }
    }
}

/// Get sessions directory for an instance
///
/// Checks multiple locations:
/// 1. Data directory: {data_dir}/agents/{instance_id}/sessions
/// 2. Config directory: {config_dir}/agents/{instance_id}/sessions
fn get_sessions_dir(paths: &GlobalPaths, instance_id: &str) -> Option<PathBuf> {
    // Try data directory first (where runtime instances store sessions)
    let data_sessions = paths.data_dir.join("agents").join(instance_id).join("sessions");
    if data_sessions.exists() {
        return Some(data_sessions);
    }

    // Try config directory (for agent definitions)
    let config_sessions = paths
        .agents_dir(None)
        .join(instance_id)
        .join("sessions");
    if config_sessions.exists() {
        return Some(config_sessions);
    }

    // Also check if instance_id is actually an agent name in default team
    let team_sessions = paths
        .config_dir
        .join("teams")
        .join("default")
        .join("agents")
        .join(instance_id)
        .join("sessions");
    if team_sessions.exists() {
        return Some(team_sessions);
    }

    None
}

/// Session index sidecar structure (subset of SessionSidecarIndex)
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
}

/// List sessions from disk by reading sidecar index files
async fn list_sessions_from_disk(sessions_dir: &PathBuf) -> Result<Vec<SessionIndex>> {
    let mut sessions = vec![];
    
    let mut entries = tokio::fs::read_dir(sessions_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if let Some(name) = entry.file_name().to_str() {
            if name.ends_with(".index.json") {
                let session_id = name.trim_end_matches(".index.json").to_string();
                let path = sessions_dir.join(name);
                
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
                    if let Ok(index) = serde_json::from_str::<SessionIndex>(&content) {
                        // Only include if session_id matches
                        if index.session_id == session_id {
                            sessions.push(index);
                        }
                    }
                }
            }
        }
    }
    
    // Sort by updated_at descending (most recent first)
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    
    Ok(sessions)
}

/// List sessions for an instance (works offline)
async fn list_sessions(
    paths: &GlobalPaths,
    instance_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    // Get sessions from disk (offline mode)
    let sessions_dir = get_sessions_dir(paths, instance_id);

    if let Some(dir) = sessions_dir {
        match list_sessions_from_disk(&dir).await {
            Ok(sessions) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&sessions)?);
                } else if sessions.is_empty() {
                    println!("📭 No sessions found for '{}'.", instance_id);
                    println!("   Start chatting with the agent to create sessions.");
                } else {
                    println!(
                        "📋 Sessions for '{}' ({} found):",
                        instance_id,
                        sessions.len()
                    );
                    println!();

                    for session in &sessions {
                        let title = session.title.as_deref().unwrap_or("(untitled)");
                        let created = format_timestamp(&session.created_at);
                        let updated = format_timestamp(&session.updated_at);
                        
                        let status = if session.ended { "🔴" } else { "🟢" };

                        println!("  {} {}", status, session.session_id);
                        println!("     Title: {}", title);
                        println!("     Turns: {}", session.turn_count);
                        println!("     Created: {}", created);
                        println!("     Updated: {}", updated);

                        if let Some(ref parent) = session.parent_session_id {
                            println!("     Parent: {}", parent);
                        }
                        println!();
                    }
                }
                return Ok(());
            }
            Err(e) => {
                debug!("Failed to read sessions from disk: {}", e);
            }
        }
    }
    
    // Fall back to API if disk read fails or directory not found
    debug!("Falling back to API for session list");
    let client = ApiClient::new()?;
    list_sessions_via_api(&client, instance_id, json).await
}

/// List sessions via API (online mode)
async fn list_sessions_via_api(
    client: &ApiClient,
    instance_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    match client.list_sessions(instance_id).await {
        Ok(response) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else if response.items.is_empty() {
                println!("📭 No sessions found for instance '{}'.", instance_id);
                println!("   Start chatting with the agent to create sessions.");
            } else {
                println!(
                    "📋 Sessions for instance '{}' ({} found):",
                    instance_id,
                    response.items.len()
                );
                println!();

                for session in &response.items {
                    let title = session.title.as_deref().unwrap_or("(untitled)");
                    let created = format_timestamp(&session.created_at);
                    let updated = format_timestamp(&session.updated_at);

                    println!("  📁 {}", session.id);
                    println!("     Title: {}", title);
                    println!("     Turns: {}", session.turn_count);
                    println!("     Created: {}", created);
                    println!("     Updated: {}", updated);

                    if let Some(ref parent) = session.parent_session_id {
                        println!("     Parent: {}", parent);
                    }
                    println!();
                }
            }
            Ok(())
        }
        Err(ClientError::DaemonNotRunning { .. }) => {
            Err(anyhow::anyhow!(
                "No sessions found for '{}'. The agent may not exist or has no sessions yet.",
                instance_id
            ))
        }
        Err(e) => Err(anyhow::anyhow!("Failed to list sessions: {}", e)),
    }
}

/// Show session details (works offline for basic info)
async fn show_session(
    paths: &GlobalPaths,
    instance_id: &str,
    session_id: &str,
    show_history: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Try to get session from disk first
    let sessions_dir = get_sessions_dir(paths, instance_id);
    
    if let Some(dir) = sessions_dir {
        let sidecar_path = dir.join(format!("{}.index.json", session_id));
        
        if sidecar_path.exists() {
            match tokio::fs::read_to_string(&sidecar_path).await {
                Ok(content) => {
                    match serde_json::from_str::<SessionIndex>(&content) {
                        Ok(index) => {
                            // If history is not requested, we can show just the metadata
                            if !show_history {
                                if json {
                                    println!("{}", serde_json::to_string_pretty(&index)?);
                                } else {
                                    println!("📊 Session Details");
                                    println!("   Instance ID: {}", instance_id);
                                    println!("   Session ID: {}", index.session_id);
                                    if let Some(ref title) = index.title {
                                        println!("   Title: {}", title);
                                    }
                                    println!("   Turns: {}", index.turn_count);
                                    println!("   Events: {}", index.event_count);
                                    println!("   Tokens: {}", index.total_tokens);
                                    println!("   Status: {}", if index.ended { "Ended" } else { "Active" });
                                    println!("   Created: {}", format_timestamp(&index.created_at));
                                    println!("   Updated: {}", format_timestamp(&index.updated_at));

                                    if let Some(ref parent) = index.parent_session_id {
                                        println!("   Parent Session: {}", parent);
                                    }
                                    
                                    println!();
                                    println!("   💡 Use --history to see full message history (requires daemon)");
                                }
                                return Ok(());
                            } else {
                                // History requested - need daemon
                                println!("📜 History requested, connecting to daemon...");
                            }
                        }
                        Err(e) => {
                            debug!("Failed to parse session index: {}", e);
                        }
                    }
                }
                Err(e) => {
                    debug!("Failed to read session file: {}", e);
                }
            }
        }
    }
    
    // Fall back to API for full details including history
    let client = ApiClient::new()?;
    show_session_via_api(&client, instance_id, session_id, show_history, json).await
}

/// Show session via API (online mode)
async fn show_session_via_api(
    client: &ApiClient,
    instance_id: &str,
    session_id: &str,
    show_history: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Get session metadata
    let session = match client.get_session(instance_id, session_id).await {
        Ok(s) => s,
        Err(ClientError::DaemonNotRunning { addr }) => {
            return Err(anyhow::anyhow!(
                "Daemon not running at {}. Start it with 'pekobot daemon start --foreground'",
                addr
            ))
        }
        Err(e) => return Err(anyhow::anyhow!("Failed to get session: {}", e)),
    };

    // Get history if requested
    let history = if show_history {
        match client
            .get_session_history(instance_id, session_id, true, false)
            .await
        {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("⚠️  Failed to load history: {}", e);
                None
            }
        }
    } else {
        None
    };

    if json {
        let output = serde_json::json!({
            "session": session,
            "history": history,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("📊 Session Details");
        println!("   Instance ID: {}", instance_id);
        println!("   Session ID: {}", session.id);
        if let Some(ref title) = session.title {
            println!("   Title: {}", title);
        }
        println!("   Turns: {}", session.turn_count);
        println!("   Created: {}", format_timestamp(&session.created_at));
        println!("   Updated: {}", format_timestamp(&session.updated_at));

        if let Some(ref parent) = session.parent_session_id {
            println!("   Parent Session: {}", parent);
        }

        if let Some(h) = history {
            if !h.items.is_empty() {
                println!();
                println!("📜 Message History ({} events):", h.items.len());
                println!();

                for (i, event) in h.items.iter().enumerate() {
                    print_history_event(i, event);
                }
            }
        }
    }

    Ok(())
}

/// Print a history event in human-readable format
fn print_history_event(index: usize, event: &crate::api::client::HistoryEventResponse) {
    let event_type = &event.event_type;
    let timestamp = format_timestamp(&event.created_at);

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
        "tool.call" => {
            if let Some(ref tool) = event.tool {
                println!("  [{}] 🔧 Tool Call: {} ({})", index, tool, timestamp);
                if let Some(ref args) = event.args {
                    println!(
                        "      Args: {}",
                        serde_json::to_string(args).unwrap_or_default()
                    );
                }
            }
        }
        "tool.result" => {
            println!("  [{}] ✅ Tool Result ({})", index, timestamp);
            if let Some(ref output) = event.output {
                println!("      Output: {}", truncate(output, 150));
            }
            if let Some(ref error) = event.error {
                println!("      Error: {}", error);
            }
        }
        "thinking" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 💭 Thinking ({}):", index, timestamp);
                println!("      {}", truncate(content, 150));
            }
        }
        "system" => {
            if let Some(ref content) = event.content {
                println!("  [{}] ⚙️  System ({}): {}", index, timestamp, content);
            }
        }
        "hook.trigger" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 🪝 Hook Trigger ({}): {}", index, timestamp, content);
            }
        }
        _ => {
            println!("  [{}] {} ({})", index, event_type, timestamp);
            if let Some(ref content) = event.content {
                println!("      {}", truncate(content, 150));
            }
        }
    }
    println!();
}

/// Branch a session (requires daemon)
async fn branch_session(
    instance_id: &str,
    session_id: &str,
    label: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let client = ApiClient::new()?;
    
    match client
        .branch_session(instance_id, session_id, label.as_deref())
        .await
    {
        Ok(response) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                println!("✅ Branched session '{}'", session_id);
                println!("   New Session ID: {}", response.session.id);
                println!("   Parent Session: {}", response.parent_session_id);
                if let Some(ref label) = label {
                    println!("   Label: {}", label);
                }
            }
            Ok(())
        }
        Err(ClientError::DaemonNotRunning { addr }) => {
            Err(anyhow::anyhow!(
                "Daemon not running at {}. Session branching requires the daemon to be active.",
                addr
            ))
        }
        Err(e) => Err(anyhow::anyhow!("Failed to branch session: {}", e)),
    }
}

/// Delete a session (requires daemon)
async fn delete_session(
    instance_id: &str,
    session_id: &str,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    if !force {
        println!("⚠️  This will delete session '{}'.", session_id);
        println!("   Session history will be permanently lost.");
        print!("   Continue? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let client = ApiClient::new()?;
    
    match client.delete_session(instance_id, session_id).await {
        Ok(()) => {
            if json {
                println!("{{\"success\": true, \"session_id\": \"{}\"}}", session_id);
            } else {
                println!("✅ Deleted session '{}'", session_id);
            }
            Ok(())
        }
        Err(ClientError::DaemonNotRunning { addr }) => {
            Err(anyhow::anyhow!(
                "Daemon not running at {}. Session deletion requires the daemon to be active.",
                addr
            ))
        }
        Err(e) => Err(anyhow::anyhow!("Failed to delete session: {}", e)),
    }
}



/// Format an ISO timestamp for display
fn format_timestamp(ts: &str) -> String {
    // Try to parse and format nicely
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
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
    fn test_session_commands_enum() {
        // Test List command
        let cmd = SessionCommands::List {
            instance_id: "inst_123".to_string(),
            all: true,
        };
        match cmd {
            SessionCommands::List { instance_id, all } => {
                assert_eq!(instance_id, "inst_123");
                assert!(all);
            }
            _ => panic!("Expected List command"),
        }

        // Test Show command
        let cmd = SessionCommands::Show {
            instance_id: "inst_123".to_string(),
            session_id: "sess_456".to_string(),
            history: true,
        };
        match cmd {
            SessionCommands::Show {
                instance_id,
                session_id,
                history,
            } => {
                assert_eq!(instance_id, "inst_123");
                assert_eq!(session_id, "sess_456");
                assert!(history);
            }
            _ => panic!("Expected Show command"),
        }

        // Test Switch command
        let cmd = SessionCommands::Switch {
            instance_id: "inst_123".to_string(),
            session_id: "sess_456".to_string(),
        };
        match cmd {
            SessionCommands::Switch {
                instance_id,
                session_id,
            } => {
                assert_eq!(instance_id, "inst_123");
                assert_eq!(session_id, "sess_456");
            }
            _ => panic!("Expected Switch command"),
        }
    }

    #[test]
    fn test_format_timestamp() {
        // Test RFC3339 timestamp
        let ts = "2026-03-17T10:30:00+00:00";
        let formatted = format_timestamp(ts);
        assert_eq!(formatted, "2026-03-17 10:30:00");

        // Test invalid timestamp (should return original)
        let ts = "invalid-timestamp";
        let formatted = format_timestamp(ts);
        assert_eq!(formatted, "invalid-timestamp");
    }

    #[test]
    fn test_truncate() {
        // Test string shorter than max
        let s = "short";
        assert_eq!(truncate(s, 10), "short");

        // Test string longer than max
        let s = "this is a very long string";
        assert_eq!(truncate(s, 10), "this is a ...");

        // Test exact length
        let s = "exactlyten";
        assert_eq!(truncate(s, 10), "exactlyten");
    }
}
