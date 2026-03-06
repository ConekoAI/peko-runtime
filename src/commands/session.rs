//! Session Management Commands

use crate::commands::GlobalPaths;
use crate::engine::SimpleSession;
use clap::Subcommand;
use std::path::PathBuf;

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
        SessionCommands::List { all: _, agent } => {
            list_sessions(agent, json).await
        }
        SessionCommands::Show { id, history } => {
            show_session(&id, history, json).await
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

/// List sessions for an agent or all agents
async fn list_sessions(
    agent_filter: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
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
        
        match tokio::fs::read_dir(&sessions_dir).await {
            Ok(mut entries) => {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    if path.extension().map_or(false, |e| e == "jsonl") {
                        let session_id = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();

                        if let Ok(metadata) = entry.metadata().await {
                            if let Ok(modified) = metadata.modified() {
                                let size = metadata.len();
                                all_sessions.push((agent_name.clone(), session_id, modified, size));
                            }
                        }
                    }
                }
            }
            Err(_) => continue,
        }
    }

    // Sort by modification time (newest first)
    all_sessions.sort_by(|a, b| b.2.cmp(&a.2));

    if json {
        let sessions_json: Vec<_> = all_sessions
            .iter()
            .map(|(agent, session_id, modified, size)| {
                serde_json::json!({
                    "agent": agent,
                    "session_id": session_id,
                    "modified": modified.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    "size_bytes": size,
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
            for (agent, session_id, modified, size) in all_sessions {
                if agent != current_agent {
                    println!("  🐱 {}", agent);
                    current_agent = agent;
                }
                
                let time_ago = format_time_ago(modified);
                let size_str = format_size(size);
                
                // Check if this is the CLI default session (OpenClaw format: agent:{agent}:cli:default)
                let is_cli_default = session_id.ends_with(":cli:default");
                let indicator = if is_cli_default { "→ " } else { "   " };
                
                println!("{}   {} {} ({})", indicator, session_id, time_ago, size_str);
            }
            
            println!();
            println!("  → = CLI default session (persistent)");
        }
    }

    Ok(())
}

/// Show session details
async fn show_session(
    session_id: &str,
    show_history: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Find the session across all agents
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let agents_dir = home.join(".pekobot").join("agents");
    
    let mut found_path = None;
    let mut found_agent = None;
    
    match tokio::fs::read_dir(&agents_dir).await {
        Ok(mut entries) => {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let agent_dir = entry.path();
                let session_path = agent_dir.join("sessions").join(format!("{}.jsonl", session_id));
                if session_path.exists() {
                    found_path = Some(session_path);
                    found_agent = agent_dir.file_name().map(|n| n.to_string_lossy().to_string());
                    break;
                }
            }
        }
        Err(_) => {}
    }
    
    let (session_path, agent_name) = match (found_path, found_agent) {
        (Some(p), Some(a)) => (p, a),
        _ => {
            eprintln!("❌ Session '{}' not found", session_id);
            return Ok(());
        }
    };
    
    if json {
        println!("{{\"session\": {{\"id\": \"{}\", \"agent\": \"{}\"}}}}", session_id, agent_name);
    } else {
        println!("📊 Session Details");
        println!("   Agent: {}", agent_name);
        println!("   Session ID: {}", session_id);
        println!("   Path: {}", session_path.display());
        
        if let Ok(metadata) = tokio::fs::metadata(&session_path).await {
            if let Ok(modified) = metadata.modified() {
                let time_ago = format_time_ago(modified);
                println!("   Last modified: {}", time_ago);
            }
            println!("   Size: {}", format_size(metadata.len()));
        }
        
        if show_history {
            println!();
            println!("📜 Message History:");
            
            // Load and display session history
            match SimpleSession::open(&agent_name, session_id).await {
                Ok(Some(session)) => {
                    match session.load_history().await {
                        Ok(messages) => {
                            for (i, msg) in messages.iter().enumerate() {
                                let role = format!("{:?}", msg.role);
                                let preview: String = msg.content.iter()
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
                    }
                }
                _ => println!("  Could not load session"),
            }
        }
    }
    
    Ok(())
}

/// Format duration as human-readable "time ago"
fn format_time_ago(time: std::time::SystemTime) -> String {
    let now = std::time::SystemTime::now();
    let duration = now.duration_since(time).unwrap_or_default();
    
    let secs = duration.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Format byte size
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
