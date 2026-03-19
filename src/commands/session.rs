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

use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::commands::GlobalPaths;
use crate::session::index::{IndexEntry, SessionIndex};
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
        SessionCommands::List { agent, team, all: _ } => {
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
        SessionCommands::Switch { agent, session_id, team } => {
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
/// 2. Team config directory (specified team or default)
/// 3. Standalone agent config directory
fn locate_agent(paths: &GlobalPaths, name: &str, team: &str) -> Option<AgentLocation> {
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

    // 2. Try team config directory (specified team)
    let team_path = paths.team_dir(team).join("agents").join(name);
    let team_sessions = team_path.join("sessions");
    if team_sessions.exists() {
        return Some(AgentLocation {
            workspace: team_path,
            sessions_dir: team_sessions,
            is_runtime: false,
        });
    }

    // 3. Try standalone agent config (for backward compatibility)
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
async fn ensure_sessions_dir(paths: &GlobalPaths, name: &str, team: &str) -> Result<AgentLocation> {
    if let Some(loc) = locate_agent(paths, name, team) {
        Ok(loc)
    } else {
        // Create in team config directory
        let team_dir = paths.team_dir(team).join("agents").join(name);
        let sessions = team_dir.join("sessions");
        tokio::fs::create_dir_all(&sessions).await?;
        Ok(AgentLocation {
            workspace: team_dir,
            sessions_dir: sessions,
            is_runtime: false,
        })
    }
}

// ================================================================================
// Session Index (Centralized) Operations
// ================================================================================

/// Load session entry from centralized index
async fn load_session_entry(
    sessions_dir: &PathBuf,
    session_id: &str,
) -> Result<Option<IndexEntry>> {
    let mut index = SessionIndex::open(sessions_dir);
    index.find_by_session_id(session_id).await
}

/// List all sessions from centralized index
async fn list_sessions_from_disk(sessions_dir: &PathBuf) -> Result<Vec<IndexEntry>> {
    let mut index = SessionIndex::open(sessions_dir);
    let entries = index.list().await?;
    
    // Sort by updated_at descending (most recent first)
    let mut entries: Vec<_> = entries.into_iter().collect();
    entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    
    Ok(entries)
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
async fn list_sessions(paths: &GlobalPaths, team: &str, agent: &str, json: bool) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent, team) else {
        if json {
            println!("[]");
        } else {
            println!("📭 Agent '{}' not found in team '{}' or has no sessions.", agent, team);
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
        println!("📋 Sessions for '{}'/{} ({} found):", team, agent, sessions.len());
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
            let tokens = session.total_tokens.unwrap_or(0);
            println!(
                "     Messages: {} | Tokens: {}",
                session.message_count, tokens
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
        return Err(anyhow::anyhow!("Agent '{}' not found in team '{}'", agent, team));
    };

    // Load session entry from centralized index
    let Some(entry) = load_session_entry(&loc.sessions_dir, session_id).await? else {
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
        if let Some(ref trigger) = entry.trigger {
            println!("   Trigger: {}", trigger);
        }
        let tokens = entry.total_tokens.unwrap_or(0);
        println!(
            "   Messages: {} | Tokens: {}",
            entry.message_count, tokens
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
    team: &str,
    agent: &str,
    session_id: &str,
    label: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let loc = ensure_sessions_dir(paths, agent, team).await?;

    // Verify parent session exists in centralized index
    let mut index = SessionIndex::open(&loc.sessions_dir);
    let parent_entry = index.find_by_session_id(session_id).await?.ok_or_else(|| {
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

    // Create new index entry for branched session
    let new_entry = IndexEntry {
        session_id: new_session_id.clone(),
        agent_name: parent_entry.agent_name.clone(),
        session_key: parent_entry.session_key.clone(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        updated_at: std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        message_count: parent_entry.message_count,
        total_tokens: parent_entry.total_tokens,
        input_tokens: parent_entry.input_tokens,
        output_tokens: parent_entry.output_tokens,
        transcript_file: format!("{}.jsonl", new_session_id),
        cwd: parent_entry.cwd.clone(),
        provider: parent_entry.provider.clone(),
        model: parent_entry.model.clone(),
        channel: parent_entry.channel.clone(),
        recipient: parent_entry.recipient.clone(),
        account_id: parent_entry.account_id.clone(),
        last_error: None,
        parent_session_id: Some(session_id.to_string()),
        title: label.clone().or_else(|| {
            parent_entry
                .title
                .as_ref()
                .map(|t| format!("Branch: {}", t))
        }),
        ended: false,
        trigger: Some("branch".to_string()),
    };

    // Add to centralized index
    let index_key = parent_entry
        .session_key
        .clone()
        .unwrap_or_else(|| format!("agent:{}:session:{}", agent, new_session_id));
    index.insert(index_key, new_entry.clone()).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&new_entry)?);
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
    team: &str,
    agent: &str,
    session_id: &str,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent, team) else {
        return Err(anyhow::anyhow!("Agent '{}' not found in team '{}'", agent, team));
    };

    // Verify session exists
    let jsonl_path = loc.sessions_dir.join(format!("{}.jsonl", session_id));
    
    // Load metadata from centralized index
    let mut index = SessionIndex::open(&loc.sessions_dir);
    let metadata = index.find_by_session_id(session_id).await?;

    if !jsonl_path.exists() && metadata.is_none() {
        return Err(anyhow::anyhow!(
            "Session '{}' not found for agent '{}'",
            session_id,
            agent
        ));
    }

    if !force {
        println!("⚠️  This will permanently delete session '{}'.", session_id);
        if let Some(ref meta) = metadata {
            if let Some(ref title) = meta.title {
                println!("   Title: {}", title);
            }
            println!(
                "   Messages: {}",
                meta.message_count
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

    // Remove from centralized index
    if let Some(ref meta) = metadata {
        let index_key = meta
            .session_key
            .clone()
            .unwrap_or_else(|| format!("agent:{}:session:{}", agent, session_id));
        if index.remove(&index_key).await?.is_some() {
            deleted.push("index");
        }
        
        // Also try to remove by alternate key format
        let alt_key = format!("agent:{}:session:{}", agent, session_id);
        let _ = index.remove(&alt_key).await;
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
    team: &str,
    agent: &str,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let loc = ensure_sessions_dir(paths, agent, team).await?;

    // Verify session exists in centralized index
    let mut index = SessionIndex::open(&loc.sessions_dir);
    let entry = index.find_by_session_id(session_id).await?.ok_or_else(|| {
        anyhow::anyhow!(
            "Session '{}' not found for agent '{}'",
            session_id,
            agent
        )
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
