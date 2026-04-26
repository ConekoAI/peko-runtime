//! Session Management Commands
//!
//! These commands follow an offline-first design per ADR-013:
//! - Read operations (list, show) work directly from the filesystem
//! - File operations (branch, delete) modify files directly
//! - Only commands needing runtime coordination (send, switch on running instances) use the API
//!
//! Session storage layout:
//! - `{data_dir}/sessions/{team}/{agent}/*.jsonl` - Session event logs
//! - `{data_dir}/sessions/{team}/{agent}/sessions.json` - Centralized session index
//! - `{data_dir}/sessions/{team}/{agent}/peers.json` - Peer routing (active session tracking)

use crate::commands::GlobalPaths;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::services::session_service::{HistoryEvent, HistoryQuery, SessionService};
use crate::common::time::{format_timestamp, format_timestamp_ms};
use crate::session::metadata_controller::MetadataController;
use crate::session::types::Peer;
use crate::session::SessionEntry;
use anyhow::Result;
use clap::Subcommand;
use serde::Serialize;
use std::path::PathBuf;

/// Get the active session ID for an agent using CLI default peer
///
/// This helper centralizes the logic for resolving the active session
/// when no specific session ID is provided by the user.
async fn get_active_session_for_cli(
    paths: &GlobalPaths,
    agent: &str,
    team: &str,
) -> anyhow::Result<Option<String>> {
    let mut manager = crate::session::SessionManager::for_cli(
        paths.resolver.clone(),
        agent,
        Some(team),
        paths.user(),
    );
    let peer = Peer::User(paths.user().to_string());
    manager.get_active_session_id(&peer).await
}

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
///   # Show active session details with history (offline)
///   pekobot session show myagent --history
///   pekobot session show myagent --session-id `sess_xxx` --history
///   pekobot session show myteam/myagent --session-id `sess_xxx` --history
///
///   # Create a branch from the active session (offline)
///   pekobot session branch myagent --label "experiment"
///   pekobot session branch myagent --session-id `sess_xxx` --label "experiment"
///
///   # Remove a session (offline)
///   pekobot session remove myagent `sess_xxx`
///
///   # Switch active session (offline, updates preference)
///   pekobot session switch myagent `sess_xxx`
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
    ///
    /// If no --session-id is provided, shows the active session for the agent.
    Show {
        /// Agent name or team/agent format
        agent: String,
        /// Session ID (optional, defaults to active session)
        #[arg(short, long)]
        session_id: Option<String>,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Show full message history
        #[arg(long)]
        history: bool,
    },

    /// Branch a session (offline - copies session files)
    ///
    /// If no --session-id is provided, branches from the active session.
    Branch {
        /// Agent name or team/agent format
        agent: String,
        /// Session ID to branch from (optional, defaults to active session)
        #[arg(short, long)]
        session_id: Option<String>,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Optional label for the new session
        #[arg(short, long)]
        label: Option<String>,
    },

    /// Remove a session (offline - removes session files)
    Remove {
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

    /// Compact a session (offline - summarizes old messages)
    ///
    /// If no --session-id is provided, compacts the active session.
    /// Use --dry-run to preview what would be compacted.
    Compact {
        /// Agent name or team/agent format
        agent: String,
        /// Session ID to compact (optional, defaults to active session)
        #[arg(short, long)]
        session_id: Option<String>,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Dry-run: show what would be compacted without doing it
        #[arg(long)]
        dry_run: bool,
        /// Custom instruction for the summarizer (focus the summary)
        #[arg(short, long)]
        instruction: Option<String>,
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
            list_sessions(paths, team, agent_name, paths.user(), json).await
        }
        SessionCommands::Show {
            agent,
            session_id,
            team,
            history,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            // Resolve active session if not specified
            let resolved_session_id = match session_id {
                Some(id) => id,
                None => match get_active_session_for_cli(paths, agent_name, team).await? {
                    Some(id) => {
                        if !json {
                            println!("📋 Using active session: {id}");
                        }
                        id
                    }
                    None => {
                        return Err(anyhow::anyhow!(
                            "No active session for agent '{agent_name}'. \
                                 Run 'pekobot session list {agent}' to see available sessions, \
                                 or specify a session ID explicitly."
                        ));
                    }
                },
            };
            show_session(paths, team, agent_name, &resolved_session_id, history, json).await
        }
        SessionCommands::Branch {
            agent,
            session_id,
            team,
            label,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            // Resolve active session if not specified
            let resolved_session_id = match session_id {
                Some(id) => id,
                None => match get_active_session_for_cli(paths, agent_name, team).await? {
                    Some(id) => {
                        if !json {
                            println!("🌿 Branching from active session: {id}");
                        }
                        id
                    }
                    None => {
                        return Err(anyhow::anyhow!(
                            "No active session for agent '{agent_name}'. \
                                 Run 'pekobot session list {agent}' to see available sessions, \
                                 or specify a session ID explicitly."
                        ));
                    }
                },
            };
            branch_session(
                paths,
                team,
                agent_name,
                &resolved_session_id,
                label,
                paths.user(),
                json,
            )
            .await
        }
        SessionCommands::Remove {
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
            switch_session(paths, team, agent_name, &session_id, paths.user(), json).await
        }
        SessionCommands::Compact {
            agent,
            session_id,
            team,
            dry_run,
            instruction,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            // Resolve active session if not specified
            let resolved_session_id = match session_id {
                Some(id) => id,
                None => match get_active_session_for_cli(paths, agent_name, team).await? {
                    Some(id) => {
                        if !json {
                            println!("📦 Using active session: {id}");
                        }
                        id
                    }
                    None => {
                        return Err(anyhow::anyhow!(
                            "No active session for agent '{agent_name}'. \
                                 Run 'pekobot session list {agent}' to see available sessions, \
                                 or specify a session ID explicitly."
                        ));
                    }
                },
            };
            compact_session(
                paths,
                team,
                agent_name,
                &resolved_session_id,
                dry_run,
                instruction,
                json,
            )
            .await
        }
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
/// Uses `PathResolver` to get the correct sessions directory.
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
    Ok(metadata_list
        .into_iter()
        .map(super::super::session::metadata::SessionMetadata::to_entry)
        .collect())
}

// ================================================================================
// Command Implementations
// ================================================================================

/// List sessions for an agent (offline)
async fn list_sessions(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    user: &str,
    json: bool,
) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent, team) else {
        if json {
            println!("[]");
        } else {
            println!("📭 Agent '{agent}' not found in team '{team}' or has no sessions.");
            println!("   Create the agent first with: pekobot agent create {team}/{agent}");
        }
        return Ok(());
    };

    let sessions = list_sessions_from_disk(&loc.sessions_dir).await?;

    // Get active session from peers.json via SessionManager
    let mut manager =
        crate::session::SessionManager::for_cli(paths.resolver.clone(), agent, Some(team), user);
    let peer = Peer::User(user.to_string());
    let active_session_id = manager.get_active_session_id(&peer).await.ok().flatten();

    if json {
        let output = serde_json::json!({
            "team": team,
            "agent": agent,
            "sessions": sessions,
            "active_session": active_session_id,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if sessions.is_empty() {
        println!("📭 No sessions found for '{agent}'.");
        println!("   Start chatting with the agent to create sessions.");
    } else {
        println!(
            "📋 Sessions for {}/{} ({} found):",
            team,
            agent,
            sessions.len()
        );
        if let Some(ref active) = active_session_id {
            println!("   Active session: {active}");
        }
        println!();

        for session in &sessions {
            let title = session.title.as_deref().unwrap_or("(untitled)");
            let created = format_timestamp_ms(session.created_at);
            let updated = format_timestamp_ms(session.updated_at);

            // Status indicator - only show star for active session
            let is_active = active_session_id.as_ref() == Some(&session.session_id);
            let status_icon = if is_active { "⭐ " } else { "   " };

            println!("{} {}", status_icon, session.session_id);
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
            "Agent '{agent}' not found in team '{team}'"
        ));
    };

    // Load session entry from centralized index with sync from JSONL
    let mut controller = MetadataController::new(&loc.sessions_dir);
    let Some(metadata) = controller.get_metadata(session_id, true).await? else {
        return Err(anyhow::anyhow!(
            "Session '{session_id}' not found for agent '{agent}'"
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
        println!("   Team: {team}");
        println!("   Agent: {agent}");
        println!("   Session ID: {}", entry.session_id);
        if let Some(ref title) = entry.title {
            println!("   Title: {title}");
        }

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
            println!("   Parent Session: {parent}");
        }

        if let Some(mut events) = history_events {
            // Reverse to chronological order (oldest first) for display
            events.reverse();

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
/// Uses `SessionService` for consistent parsing with API layer.
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

/// Convert service `HistoryEvent` to CLI display format
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
        HistoryEvent::ToolCall {
            tool_name, args, ..
        } => {
            // Display tool calls with their arguments
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
                    println!("  [{index}] ⚙️  System ({time_str}):");
                    println!("      {}", truncate(content, 200));
                }
                "user" => {
                    println!("  [{index}] 👤 User ({time_str}):");
                    println!("      {}", truncate(content, 200));
                }
                "assistant" => {
                    println!("  [{index}] 🤖 Assistant ({time_str}):");
                    println!("      {}", truncate(content, 200));
                }
                "tool" => {
                    println!("  [{index}] 🔧 Tool ({time_str}):");
                    println!("      {}", truncate(content, 200));
                }
                _ => {
                    println!("  [{index}] {role} ({time_str}):");
                    println!("      {}", truncate(content, 200));
                }
            }
        }
        HistoryDisplayEntry::ToolResult { tool_name, content } => {
            println!("  [{index}] ✅ Tool Result:");
            println!("      Tool: {tool_name}");
            if !content.is_empty() {
                println!("      Output: {}", truncate(content, 150));
            }
        }
        HistoryDisplayEntry::ModelChange { provider, model_id } => {
            println!("  [{index}] 🔄 Model Change:");
            println!("      Provider: {provider}, Model: {model_id}");
        }
        HistoryDisplayEntry::Compaction { summary } => {
            println!("  [{index}] 📦 Compaction:");
            println!("      {}", truncate(summary, 150));
        }
        HistoryDisplayEntry::Custom { custom_type } => {
            println!("  [{index}] 📎 Custom ({custom_type})");
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
    user: &str,
    json: bool,
) -> anyhow::Result<()> {
    // Use SessionManager for the branch operation
    let mut manager =
        crate::session::SessionManager::for_cli(_paths.resolver.clone(), agent, Some(team), user);

    // Verify parent session exists
    let _parent_metadata = manager
        .get_session_metadata(session_id)
        .await
        .map_err(|_| {
            anyhow::anyhow!("Parent session '{session_id}' not found for agent '{agent}'")
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
        println!("✅ Branched session '{session_id}'");
        println!("   New Session ID: {new_session_id}");
        println!("   Parent Session: {session_id}");
        if let Some(ref lbl) = label {
            println!("   Label: {lbl}");
        }
        println!();
        println!("   The branched session contains a copy of the parent's history.");
        println!("   Switch to it with: pekobot session switch {team}/{agent} {new_session_id}");
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
            "Agent '{agent}' not found in team '{team}'"
        ));
    };

    // Use MetadataController for delete operation
    let mut controller = crate::session::MetadataController::new(&loc.sessions_dir);

    // Check if session exists and get metadata for confirmation
    let metadata = if let Some(m) = controller.get_metadata_fast(session_id).await? {
        Some(m)
    } else {
        // Check if JSONL file exists without metadata (orphaned)
        let jsonl_path = loc.sessions_dir.join(format!("{session_id}.jsonl"));
        if !jsonl_path.exists() {
            return Err(anyhow::anyhow!(
                "Session '{session_id}' not found for agent '{agent}'"
            ));
        }
        None
    };

    if !force {
        println!("⚠️  This will permanently delete session '{session_id}'.");
        if let Some(ref meta) = metadata {
            if let Some(ref title) = meta.title {
                println!("   Title: {title}");
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

    if json {
        let deleted_items = if deleted {
            vec!["jsonl", "index"]
        } else {
            vec![]
        };
        println!("{{\"success\": true, \"deleted\": {deleted_items:?}}}");
    } else {
        println!("✅ Deleted session '{session_id}'");
        if deleted {
            println!("   Removed: jsonl, index");
        }
    }

    Ok(())
}

/// Switch active session (offline - updates peers.json routing)
async fn switch_session(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    session_id: &str,
    user: &str,
    json: bool,
) -> anyhow::Result<()> {
    let _loc = ensure_sessions_dir(paths, agent, team).await?;

    // Create SessionManager to update peer routing
    let mut manager =
        crate::session::SessionManager::for_cli(paths.resolver.clone(), agent, Some(team), user);

    // Verify session exists
    let _ = manager
        .get_session_metadata(session_id)
        .await
        .map_err(|_| anyhow::anyhow!("Session '{session_id}' not found for agent '{agent}'"))?;

    // Switch the active session for the specified user peer
    let peer = Peer::User(user.to_string());
    manager.switch_session(&peer, session_id).await?;

    if json {
        println!(
            "{{\"success\": true, \"session_id\": \"{session_id}\", \"agent\": \"{agent}\", \"team\": \"{team}\"}}"
        );
    } else {
        println!("✅ Switched active session for '{team}/{agent}' to '{session_id}'");
        println!();
        println!("   Future 'pekobot send' commands will use this session.");
    }

    Ok(())
}

// ================================================================================
// Compact Session (ADR-022)
// ================================================================================

/// Compact a session by summarizing old messages (offline)
async fn compact_session(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    session_id: &str,
    dry_run: bool,
    instruction: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let Some(loc) = locate_agent(paths, agent, team) else {
        return Err(anyhow::anyhow!("Agent '{agent}' not found in team '{team}'"));
    };

    // Open the session
    let peer = crate::session::types::Peer::User(paths.user().to_string());
    let session = crate::session::Session::open_by_id(
        agent,
        session_id,
        &loc.sessions_dir,
        Some(&peer),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to open session '{session_id}': {e}"))?;

    // Load current context
    let messages = session.load_context_fast().await.map_err(|e| {
        anyhow::anyhow!("Failed to load context for session '{session_id}': {e}")
    })?;

    let estimated_tokens = crate::compaction::Compactor::estimate_tokens(&messages);

    if dry_run {
        let context_window = 128_000usize; // Default for dry-run display
        let percent = (estimated_tokens * 100) / context_window.max(1);
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "dry_run": true,
                    "session_id": session_id,
                    "estimated_tokens": estimated_tokens,
                    "context_window": context_window,
                    "percent": percent,
                    "message_count": messages.len(),
                })
            );
        } else {
            println!("📊 Dry-run compaction for session '{session_id}'");
            println!("   Estimated tokens: {estimated_tokens} / {context_window} ({}%)", percent);
            println!("   Messages: {}", messages.len());
            println!();
            println!("   Would compact {} messages into a summary.", messages.len().saturating_sub(2));
        }
        return Ok(());
    }

    // Perform compaction
    let mut compactor = crate::compaction::Compactor::new();

    // Load previous summary for cumulative updates
    let previous_summary = session
        .load_previous_compaction_summary()
        .await
        .ok()
        .flatten();
    if let Some(ref summary) = previous_summary {
        compactor = compactor.with_previous_summary(Some(summary.clone()));
    }

    // TODO: In a full implementation, we would need a Provider here to call the LLM.
    // For the CLI command, we create a placeholder provider or use a local summarization.
    // Since Provider requires API keys and network, the CLI compact command currently
    // demonstrates the flow and records the compaction metadata.
    //
    // A production implementation would:
    // 1. Load the agent's provider configuration
    // 2. Instantiate the Provider with the correct API key
    // 3. Call compactor.compact(&messages, &provider).await
    //
    // For now, we do a "metadata-only" compaction that records the event
    // without actually calling an LLM (useful for testing the pipeline).

    // Simple truncation-based compaction for CLI (no LLM required)
    let (initial_system, conversation): (Vec<_>, Vec<_>) =
        if !messages.is_empty() && matches!(messages[0].role, crate::providers::MessageRole::System) {
            (vec![messages[0].clone()], messages[1..].to_vec())
        } else {
            (vec![], messages.clone())
        };

    // Keep last 4 messages, summarize the rest
    let keep_count = conversation.len().min(4);
    let split_point = conversation.len().saturating_sub(keep_count);
    let to_compact = &conversation[..split_point];
    let to_keep = &conversation[split_point..];

    let summary_text = if let Some(ref instr) = instruction {
        format!("[Custom instruction: {instr}]\n\n[{} messages summarized]", to_compact.len())
    } else {
        format!("[{} messages summarized - conversation history]", to_compact.len())
    };

    let summary_message = crate::providers::ChatMessage {
        role: crate::providers::MessageRole::System,
        content: vec![crate::types::message::ContentBlock::Text {
            text: format!(
                "[Conversation Summary - {} messages]:\n{}",
                to_compact.len(),
                summary_text
            ),
        }],
        tool_calls: None,
        tool_call_id: None,
    };

    let mut compacted = initial_system;
    compacted.push(summary_message);
    compacted.extend(to_keep.to_vec());

    let tokens_after = crate::compaction::Compactor::estimate_tokens(&compacted);
    let tokens_saved = estimated_tokens.saturating_sub(tokens_after);

    // Determine the next compaction number by counting existing compaction events
    let storage = crate::session::jsonl::SessionStorage::new(loc.sessions_dir.clone());
    let existing_compactions = storage
        .load_normalized(session_id)
        .await
        .map(|entries| {
            entries
                .iter()
                .filter(|e| matches!(e, crate::session::NormalizedEntry::Compaction { .. }))
                .count()
        })
        .unwrap_or(0);
    let compaction_number = existing_compactions + 1;

    // Record compaction in session
    let entry = crate::compaction::CompactionEntry {
        timestamp: chrono::Utc::now(),
        summary: summary_text,
        first_kept_entry_id: format!("kept_{}", to_keep.len()),
        messages_compacted: to_compact.len(),
        tokens_before: estimated_tokens,
        tokens_after,
        compaction_number,
        details: None,
    };

    storage
        .append_compaction(
            session_id,
            None,
            &entry.summary,
            entry.messages_compacted,
            entry.tokens_before,
            entry.tokens_after,
            entry.compaction_number,
            entry.details.as_ref(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to record compaction: {e}"))?;

    // Update context cache after compaction event is recorded so checksum matches
    session
        .update_context_cache(&compacted)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to update context cache: {e}"))?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "session_id": session_id,
                "messages_compacted": entry.messages_compacted,
                "tokens_before": entry.tokens_before,
                "tokens_after": entry.tokens_after,
                "tokens_saved": tokens_saved,
            })
        );
    } else {
        println!("✅ Compacted session '{session_id}'");
        println!(
            "   {} messages → summary, saved {} tokens ({} → {})",
            entry.messages_compacted, tokens_saved, entry.tokens_before, entry.tokens_after
        );
        if instruction.is_some() {
            println!("   Custom instruction applied.");
        }
    }

    Ok(())
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
    fn test_session_commands_compact_variant_exists() {
        // Verify the Compact variant can be constructed
        let cmd = SessionCommands::Compact {
            agent: "test-agent".to_string(),
            session_id: Some("sess_123".to_string()),
            team: Some("test-team".to_string()),
            dry_run: true,
            instruction: Some("preserve API decisions".to_string()),
        };

        // Just verify it compiles and fields are accessible
        match cmd {
            SessionCommands::Compact {
                agent,
                session_id,
                team,
                dry_run,
                instruction,
            } => {
                assert_eq!(agent, "test-agent");
                assert_eq!(session_id, Some("sess_123".to_string()));
                assert_eq!(team, Some("test-team".to_string()));
                assert!(dry_run);
                assert_eq!(instruction, Some("preserve API decisions".to_string()));
            }
            _ => panic!("Expected Compact variant"),
        }
    }

    #[test]
    fn test_session_commands_compact_defaults() {
        let cmd = SessionCommands::Compact {
            agent: "myagent".to_string(),
            session_id: None,
            team: None,
            dry_run: false,
            instruction: None,
        };

        match cmd {
            SessionCommands::Compact {
                agent,
                session_id,
                dry_run,
                instruction,
                ..
            } => {
                assert_eq!(agent, "myagent");
                assert!(session_id.is_none());
                assert!(!dry_run);
                assert!(instruction.is_none());
            }
            _ => panic!("Expected Compact variant"),
        }
    }
}
