//! Session Management Commands

use crate::commands::GlobalPaths;
use crate::engine::SimpleSession;
use crate::session::index::{MaintenanceConfig, MaintenanceMode, SessionIndex};
use crate::session::key::cli_session_key;
use clap::Subcommand;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Session management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum SessionCommands {
    /// List active sessions
    List {
        /// Show all sessions (including inactive)
        #[arg(long)]
        all: bool,
        /// Filter by agent name
        #[arg(short, long)]
        agent: Option<String>,
    },

    /// Show session details and history
    Show {
        /// Session ID
        id: String,
        /// Show full message history
        #[arg(long)]
        history: bool,
    },

    /// Run maintenance on sessions (prune old, cap count)
    Maintenance {
        /// Agent to maintain (default: all)
        #[arg(short, long)]
        agent: Option<String>,
        /// Actually perform maintenance (default: dry run)
        #[arg(long)]
        execute: bool,
    },

    /// Send message to a session
    Send {
        /// Session ID
        id: String,
        /// Message content
        message: String,
    },

    /// Terminate a session
    Kill {
        /// Session ID
        id: String,
        /// Force immediate termination
        #[arg(short, long)]
        force: bool,
    },
}

/// Handle session commands
pub async fn handle_session(
    cmd: SessionCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        SessionCommands::List { all: _, agent } => list_sessions(agent, json).await,
        SessionCommands::Show { id, history } => show_session(&id, history, json).await,
        SessionCommands::Maintenance { agent, execute } => {
            run_maintenance(agent, execute, json).await
        }
        SessionCommands::Send { id, message } => {
            println!("📤 Sending to session '{id}': {message}");
            Ok(())
        }
        SessionCommands::Kill { id, force } => {
            if force {
                println!("💀 Force killing session '{id}'...");
            } else {
                println!("🛑 Stopping session '{id}'...");
            }
            Ok(())
        }
    }
}

/// List sessions for an agent or all agents using the index
async fn list_sessions(agent_filter: Option<String>, json: bool) -> anyhow::Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let agents_dir = home.join(".pekobot").join("agents");

    let mut all_sessions = Vec::new();

    // If agent filter specified, only check that agent
    let agents_to_check: Vec<String> = if let Some(agent) = agent_filter {
        vec![agent]
    } else {
        // List all agents
        match tokio::fs::read_dir(&agents_dir).await {
            Ok(mut entries) => {
                let mut agents = Vec::new();
                while let Ok(Some(entry)) = entries.next_entry().await {
                    if let Ok(metadata) = entry.metadata().await {
                        if metadata.is_dir() {
                            if let Some(name) = entry.file_name().to_str() {
                                agents.push(name.to_string());
                            }
                        }
                    }
                }
                agents
            }
            Err(_) => Vec::new(),
        }
    };

    for agent_name in agents_to_check {
        let sessions_dir = agents_dir.join(&agent_name).join("sessions");
        let mut index = SessionIndex::open(&sessions_dir);

        // Ensure index is migrated
        let _ = index.migrate_from_directory(&agent_name).await;

        // List from index
        match index.list().await {
            Ok(entries) => {
                for entry in entries {
                    let modified = SystemTime::UNIX_EPOCH + Duration::from_millis(entry.updated_at);

                    // Get transcript file size
                    let transcript_path = sessions_dir.join(&entry.transcript_file);
                    let size = if let Ok(metadata) = tokio::fs::metadata(&transcript_path).await {
                        metadata.len()
                    } else {
                        0
                    };

                    all_sessions.push((
                        agent_name.clone(),
                        entry.session_id,
                        entry.session_key,
                        modified,
                        size,
                        entry.message_count,
                        entry.total_tokens,
                    ));
                }
            }
            Err(_) => continue,
        }
    }

    // Sort by modification time (newest first)
    all_sessions.sort_by(|a, b| b.3.cmp(&a.3));

    if json {
        let sessions_json: Vec<_> = all_sessions
            .iter()
            .map(|(agent, session_id, session_key, modified, size, msg_count, tokens)| {
                serde_json::json!({
                    "agent": agent,
                    "session_id": session_id,
                    "session_key": session_key,
                    "modified": modified.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    "size_bytes": size,
                    "message_count": msg_count,
                    "total_tokens": tokens,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "sessions": sessions_json }));
    } else {
        if all_sessions.is_empty() {
            println!("📭 No sessions found.");
            println!("   Start chatting with an agent to create sessions.");
        } else {
            println!("📋 Sessions ({} found):", all_sessions.len());
            println!();

            // Group by agent
            let mut current_agent = String::new();
            for (agent, session_id, session_key, modified, size, msg_count, tokens) in all_sessions
            {
                if agent != current_agent {
                    println!("  🐱 {}", agent);
                    current_agent = agent;
                }

                let time_ago = format_time_ago(modified);
                let size_str = format_size(size);

                // Check if this is the CLI default session
                let is_cli_default = session_key
                    .as_ref()
                    .map(|k| k == &cli_session_key(&current_agent))
                    .unwrap_or(false);
                let indicator = if is_cli_default { "→ " } else { "   " };

                // Show session key if available, otherwise session_id
                let display_id = session_key
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or(&session_id);

                let msg_info = if msg_count > 0 {
                    format!(" | {} msgs", msg_count)
                } else {
                    String::new()
                };

                let token_info = if let Some(t) = tokens {
                    format!(" | {} tokens", t)
                } else {
                    String::new()
                };

                println!(
                    "{}   {}{} ({}{}{})",
                    indicator, display_id, time_ago, size_str, msg_info, token_info
                );
            }

            println!();
            println!("  → = CLI default session (persistent)");
        }
    }

    Ok(())
}

/// Run maintenance on sessions
async fn run_maintenance(
    agent_filter: Option<String>,
    execute: bool,
    json: bool,
) -> anyhow::Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let agents_dir = home.join(".pekobot").join("agents");

    let agents_to_check: Vec<String> = if let Some(agent) = agent_filter {
        vec![agent]
    } else {
        match tokio::fs::read_dir(&agents_dir).await {
            Ok(mut entries) => {
                let mut agents = Vec::new();
                while let Ok(Some(entry)) = entries.next_entry().await {
                    if let Ok(metadata) = entry.metadata().await {
                        if metadata.is_dir() {
                            if let Some(name) = entry.file_name().to_str() {
                                agents.push(name.to_string());
                            }
                        }
                    }
                }
                agents
            }
            Err(_) => Vec::new(),
        }
    };

    let mut total_pruned = 0;
    let mut total_capped = 0;
    let mut total_rotated = 0;
    let mut total_bytes = 0u64;

    for agent_name in agents_to_check {
        let sessions_dir = agents_dir.join(&agent_name).join("sessions");
        let mut index = SessionIndex::open(&sessions_dir);

        let _ = index.migrate_from_directory(&agent_name).await;

        let config = MaintenanceConfig {
            mode: if execute {
                MaintenanceMode::Auto
            } else {
                MaintenanceMode::Warn
            },
            ..Default::default()
        };

        match index.maintenance(&config).await {
            Ok(report) => {
                total_pruned += report.pruned;
                total_capped += report.capped;
                if report.rotated {
                    total_rotated += 1;
                }
                total_bytes += report.bytes_reclaimed;
            }
            Err(e) => {
                eprintln!("⚠️  Maintenance failed for {}: {}", agent_name, e);
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "mode": if execute { "executed" } else { "dry-run" },
                "pruned": total_pruned,
                "capped": total_capped,
                "rotated": total_rotated,
                "bytes_reclaimed": total_bytes,
            })
        );
    } else {
        let mode_str = if execute { "Executed" } else { "Would execute" };
        println!("🔧 {} maintenance:", mode_str);
        println!("   Sessions pruned: {}", total_pruned);
        println!("   Sessions capped: {}", total_capped);
        println!("   Indexes rotated: {}", total_rotated);
        println!("   Space reclaimed: {}", format_size(total_bytes));

        if !execute && (total_pruned > 0 || total_capped > 0) {
            println!();
            println!("   Run with --execute to actually perform maintenance.");
        }
    }

    Ok(())
}

/// Show session details
async fn show_session(session_id: &str, show_history: bool, json: bool) -> anyhow::Result<()> {
    // Find the session across all agents using index
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let agents_dir = home.join(".pekobot").join("agents");

    let mut found_entry = None;
    let mut found_agent = None;

    // Search through all agents
    if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let agent_dir = entry.path();
            if let Ok(metadata) = entry.metadata().await {
                if metadata.is_dir() {
                    let agent_name = agent_dir
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();

                    let sessions_dir = agent_dir.join("sessions");
                    let mut index = SessionIndex::open(&sessions_dir);
                    let _ = index.migrate_from_directory(&agent_name).await;

                    // Try to find by session_id
                    if let Ok(Some(idx_entry)) = index.find_by_session_id(session_id).await {
                        found_entry = Some(idx_entry);
                        found_agent = Some(agent_name);
                        break;
                    }
                }
            }
        }
    }

    let (entry, agent_name) = match (found_entry, found_agent) {
        (Some(e), Some(a)) => (e, a),
        _ => {
            eprintln!("❌ Session '{}' not found", session_id);
            return Ok(());
        }
    };

    let session_path = agents_dir
        .join(&agent_name)
        .join("sessions")
        .join(&entry.transcript_file);

    if json {
        println!(
            "{}",
            serde_json::json!({
                "session": {
                    "id": entry.session_id,
                    "agent": agent_name,
                    "session_key": entry.session_key,
                    "message_count": entry.message_count,
                    "total_tokens": entry.total_tokens,
                    "provider": entry.provider,
                    "model": entry.model,
                    "created_at": entry.created_at,
                    "updated_at": entry.updated_at,
                }
            })
        );
    } else {
        println!("📊 Session Details");
        println!("   Agent: {}", agent_name);
        println!("   Session ID: {}", entry.session_id);
        if let Some(ref key) = entry.session_key {
            println!("   Session Key: {}", key);
        }
        println!("   Messages: {}", entry.message_count);
        if let Some(tokens) = entry.total_tokens {
            println!("   Tokens: {}", tokens);
        }
        if let Some(ref provider) = entry.provider {
            if let Some(ref model) = entry.model {
                println!("   Model: {}/{}", provider, model);
            }
        }
        println!("   Path: {}", session_path.display());

        let modified = SystemTime::UNIX_EPOCH + Duration::from_millis(entry.updated_at);
        println!("   Last active: {}", format_time_ago(modified));

        if let Ok(metadata) = tokio::fs::metadata(&session_path).await {
            println!("   Size: {}", format_size(metadata.len()));
        }

        if show_history {
            println!();
            println!("📜 Message History:");

            match SimpleSession::open(&agent_name, session_id).await {
                Ok(Some(session)) => match session.load_history().await {
                    Ok(messages) => {
                        for (i, msg) in messages.iter().enumerate() {
                            let role = format!("{:?}", msg.role);
                            let preview: String = msg
                                .content
                                .iter()
                                .filter_map(|b| match b {
                                    crate::types::ContentBlock::Text { text } => Some(text.clone()),
                                    _ => None,
                                })
                                .collect::<String>()
                                .chars()
                                .take(100)
                                .collect();

                            if !preview.is_empty() {
                                println!("  [{}] {}: {}", i, role, preview);
                            }
                        }
                    }
                    Err(e) => println!("  Error loading history: {}", e),
                },
                _ => println!("  Could not load session"),
            }
        }
    }

    Ok(())
}

/// Format a duration as a human-readable string
fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Format a SystemTime as "X ago"
fn format_time_ago(time: SystemTime) -> String {
    match time.elapsed() {
        Ok(duration) => format!("{} ago", format_duration(duration)),
        Err(_) => "unknown".to_string(),
    }
}

/// Format bytes as human-readable string
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
