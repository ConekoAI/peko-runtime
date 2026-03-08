//! CLI channel - Interactive terminal interface
//!
//! Presentation layer for CLI output. Separated from agent/engine logic
//! to allow easy extension to other channels (Discord, WhatsApp, etc.)

use super::{Channel, StreamingConfig};
use anyhow::Result;
use async_trait::async_trait;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::engine::SimpleSession;
use crate::session::index::SessionIndex;

/// Command line interface channel with interactive input
pub struct CliChannel {
    name: String,
    stdin_tx: mpsc::Sender<String>,
    stdin_rx: mpsc::Receiver<String>,
    _input_handle: tokio::task::JoinHandle<()>,
    streaming_config: StreamingConfig,
}

impl CliChannel {
    /// Create a new CLI channel with the given name
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_config(name, StreamingConfig::default())
    }

    /// Create a new CLI channel with custom streaming configuration
    pub fn with_config(name: impl Into<String>, streaming_config: StreamingConfig) -> Self {
        let name = name.into();
        let (stdin_tx, stdin_rx) = mpsc::channel::<String>(100);

        // Spawn stdin reader task
        let tx = stdin_tx.clone();
        let _input_handle = tokio::spawn(async move {
            let stdin = tokio::io::stdin();
            let reader = BufReader::new(stdin);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).await.is_err() {
                    break;
                }
            }
        });

        Self {
            name,
            stdin_tx,
            stdin_rx,
            _input_handle,
            streaming_config,
        }
    }

    /// Print a styled banner
    pub fn print_banner(&self) {
        println!("\n╔════════════════════════════════════════╗");
        println!("║     🐱 Pekobot Agent Interface         ║");
        println!("╚════════════════════════════════════════╝");
        println!("   Channel: {}\n", self.name);
    }

    /// Print a prompt for user input
    pub fn print_prompt(&self) {
        print!("\n💬 You: ");
        std::io::stdout().flush().unwrap();
    }

    /// Print error
    pub fn print_error(&self, error: &str) {
        eprintln!("\n❌ Error: {error}");
    }
}

#[async_trait]
impl Channel for CliChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&mut self, message: &str) -> Result<()> {
        println!("{}", message);
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<String>> {
        match self.stdin_rx.try_recv() {
            Ok(line) => Ok(Some(line)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!("Input channel disconnected"))
            }
        }
    }

    fn streaming_config(&self) -> StreamingConfig {
        self.streaming_config.clone()
    }
}

/// Process events and return final answer
/// 
/// Unified event handling for both interactive and non-interactive modes.
/// All output uses the same format: {agent_name}: {content}
pub async fn process_events(
    mut event_rx: tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>,
    agent_name: &str,
) -> Result<String> {
    use crate::engine::{AgenticEvent, LifecyclePhase};
    
    let mut final_answer = String::new();
    let mut has_started_line = false;
    let mut last_was_thinking = false;

    while let Some(event) = event_rx.recv().await {
        match event {
            AgenticEvent::Lifecycle { phase, .. } => match phase {
                LifecyclePhase::End => {
                    if has_started_line {
                        println!();
                    }
                    break;
                }
                LifecyclePhase::Error => {
                    return Err(anyhow::anyhow!("Agent encountered an error"));
                }
                _ => {}
            },
            AgenticEvent::Thinking { text, .. } => {
                // Thinking/reasoning before tool calls
                if !text.is_empty() {
                    if !has_started_line {
                        // First thinking of this turn
                        print!("\n{}: ", agent_name);
                        has_started_line = true;
                    } else if last_was_thinking {
                        // Continuing from previous thinking - add space
                        print!(" ");
                    }
                    // Replace newlines with spaces for clean output
                    let single_line = text.replace('\n', " ");
                    print!("{}", single_line);
                    std::io::stdout().flush().unwrap();
                    last_was_thinking = true;
                }
            }
            AgenticEvent::Assistant { text, is_final, .. } => {
                last_was_thinking = false;
                if !text.is_empty() {
                    if is_final {
                        // Final answer - ensure newline and finish
                        if !has_started_line {
                            print!("\n{}: ", agent_name);
                        }
                        println!("{}", text);
                        final_answer = text;
                        has_started_line = false;
                    } else {
                        // Streaming delta - continue inline
                        if !has_started_line {
                            print!("\n{}: ", agent_name);
                            has_started_line = true;
                        }
                        print!("{}", text);
                        std::io::stdout().flush().unwrap();
                    }
                }
            }
            AgenticEvent::ToolStart { name, .. } => {
                // Tool execution starts - end current line so next thinking starts fresh
                if has_started_line {
                    println!();
                    has_started_line = false;
                }
                last_was_thinking = false;
            }
            AgenticEvent::ToolEnd { .. } => {}
            _ => {}
        }
    }

    Ok(final_answer)
}

/// Run interactive loop with streaming support
pub async fn run_interactive_loop(
    mut channel: CliChannel,
    agent: std::sync::Arc<std::sync::Mutex< crate::agent::Agent>>,
) -> Result<()> {
    use tokio::task::LocalSet;

    channel.print_banner();
    
    // Get agent name and session key for this interactive session
    let agent_name = {
        let agent_lock = agent.lock().unwrap();
        agent_lock.name().to_string()
    };
    let session_key = format!("agent:{}:cli:default", agent_name);
    
    // Open or create session once at startup (not on every message)
    let (mut current_session, mut history) = match SimpleSession::open_by_key(&agent_name, &session_key).await {
        Ok(Some(session)) => {
            info!("Resuming existing CLI session with key: {}", session_key);
            match session.load_history().await {
                Ok(hist) => {
                    if !hist.is_empty() {
                        info!("📂 Resumed session with {} previous messages", hist.len());
                    }
                    (Some(session), Some(hist))
                }
                Err(e) => {
                    warn!("Failed to load session history: {}", e);
                    (Some(session), None)
                }
            }
        }
        Ok(None) => {
            info!("No existing CLI session found, creating new one with key: {}", session_key);
            match SimpleSession::open_or_create_by_key(&agent_name, &session_key).await {
                Ok(session) => {
                    info!("🆕 Started new session");
                    (Some(session), None)
                }
                Err(e) => {
                    warn!("Failed to create session: {}", e);
                    (None, None)
                }
            }
        }
        Err(e) => {
            warn!("Failed to open session: {}", e);
            (None, None)
        }
    };
    
    channel.print_prompt();

    loop {
        // Check for input
        match channel.stdin_rx.try_recv() {
            Ok(line) => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    channel.print_prompt();
                    continue;
                }

                // Handle special commands
                if trimmed.eq_ignore_ascii_case("exit")
                    || trimmed.eq_ignore_ascii_case("quit")
                {
                    println!("\n👋 Goodbye!");
                    break;
                }

                if trimmed.eq_ignore_ascii_case("status") {
                    let agent = agent.lock().unwrap();
                    println!("\n📊 Agent Status: {:?}", agent.state());
                    channel.print_prompt();
                    continue;
                }

                // Handle /new command - start fresh session
                if trimmed.eq_ignore_ascii_case("/new") {
                    println!("\n🆕 Starting new session...");
                    if let Err(e) = reset_cli_session_by_key(&agent_name, &session_key).await {
                        eprintln!("❌ Failed to reset session: {}", e);
                    } else {
                        println!("✅ Session reset. Next message will start fresh.");
                        // Reset local session tracking
                        match SimpleSession::open_or_create_by_key(&agent_name, &session_key).await {
                            Ok(session) => {
                                current_session = Some(session);
                                history = None;
                            }
                            Err(e) => {
                                warn!("Failed to create new session: {}", e);
                                current_session = None;
                                history = None;
                            }
                        }
                    }
                    channel.print_prompt();
                    continue;
                }

                // Handle /sessions command - list sessions
                if trimmed.eq_ignore_ascii_case("/sessions") {
                    if let Err(e) = list_cli_sessions().await {
                        eprintln!("❌ Failed to list sessions: {}", e);
                    }
                    channel.print_prompt();
                    continue;
                }

                // Process the message with session persistence
                // Use the session we opened at startup (or reload if needed)
                let local = LocalSet::new();
                let result = local
                    .run_until(async {
                        let agent_lock = agent.lock().unwrap();
                        let event_rx = agent_lock.execute_streaming_with_session(
                            trimmed,
                            current_session.take(),
                            history.take(),
                        ).await?;
                        process_events(event_rx, &agent_name).await
                    })
                    .await;

                match result {
                    Ok(_answer) => {
                        // Response already printed by process_events
                    }
                    Err(e) => {
                        error!("Error in streaming: {}", e);
                        channel.print_error(&format!("Error: {}", e));
                    }
                }
                
                // Reload session from disk for next message
                // (execute_streaming_with_session takes ownership)
                match SimpleSession::open_by_key(&agent_name, &session_key).await {
                    Ok(Some(session)) => {
                        current_session = Some(session);
                        history = current_session.as_ref().unwrap().load_history().await.ok();
                    }
                    Ok(None) => {
                        // Session was removed, create new
                        match SimpleSession::open_or_create_by_key(&agent_name, &session_key).await {
                            Ok(session) => {
                                current_session = Some(session);
                                history = None;
                            }
                            Err(e) => {
                                warn!("Failed to recreate session: {}", e);
                                current_session = None;
                                history = None;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to reload session: {}", e);
                        current_session = None;
                        history = None;
                    }
                }

                // Reset agent state to Idle for next message
                {
                    let agent_lock = agent.lock().unwrap();
                    agent_lock.set_state(crate::types::agent::AgentState::Idle);
                }
                
                // Print new prompt after response
                channel.print_prompt();
            }
            Err(mpsc::error::TryRecvError::Disconnected) => {
                break;
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                // No input available, just wait
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
        }
    }

    Ok(())
}

/// Send a single message to the agent and get a response (non-interactive)
/// Uses the same process_events as interactive mode
pub async fn send_single_message(
    agent: &crate::agent::Agent,
    message: &str,
) -> Result<String> {
    send_single_message_with_session(agent, message, false).await
}

/// Send a single message with session persistence support
/// 
/// If `new_session` is true, creates a new session.
/// Otherwise, tries to resume the existing CLI session for this agent.
pub async fn send_single_message_with_session(
    agent: &crate::agent::Agent,
    message: &str,
    new_session: bool,
) -> Result<String> {
    use tokio::task::LocalSet;

    let agent_name = agent.name().to_string();
    
    // CLI uses a consistent session key for persistence
    // OpenClaw-compatible format: agent:{agent}:cli:default
    let session_key = format!("agent:{}:cli:default", agent_name);
    
    // Create a LocalSet for the streaming execution
    let local = LocalSet::new();

    local
        .run_until(async {
            let (existing_session, history) = if new_session {
                info!("Starting new CLI session (explicit --new flag)");
                // For --new, we want to create a fresh session even if one exists
                // Remove the old index entry (file stays on disk for reference)
                let storage_dir = SimpleSession::storage_dir(&agent_name);
                let mut index = SessionIndex::open(&storage_dir);
                if let Err(e) = index.remove(&session_key).await {
                    debug!("Failed to remove old session index entry: {}", e);
                }
                // Now create new session
                match SimpleSession::open_or_create_by_key(&agent_name, &session_key).await {
                    Ok(session) => {
                        println!("🆕 Created new session");
                        (Some(session), None)
                    }
                    Err(e) => {
                        warn!("Failed to create session: {}", e);
                        (None, None)
                    }
                }
            } else {
                // Try to open existing session by key
                match SimpleSession::open_by_key(&agent_name, &session_key).await {
                    Ok(Some(session)) => {
                        info!("Resuming existing CLI session with key: {}", session_key);
                        match session.load_history().await {
                            Ok(hist) => {
                                if !hist.is_empty() {
                                    println!("📂 Resumed session with {} previous messages", hist.len());
                                }
                                (Some(session), Some(hist))
                            }
                            Err(e) => {
                                warn!("Failed to load session history: {}", e);
                                (Some(session), None)
                            }
                        }
                    }
                    Ok(None) => {
                        info!("No existing CLI session found, creating new one with key: {}", session_key);
                        // Create new session
                        match SimpleSession::open_or_create_by_key(&agent_name, &session_key).await {
                            Ok(session) => (Some(session), None),
                            Err(e) => {
                                warn!("Failed to create session: {}", e);
                                (None, None)
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to open session: {}", e);
                        (None, None)
                    }
                }
            };

            let event_rx = agent.execute_streaming_with_session(
                message,
                existing_session,
                history,
            ).await?;
            process_events(event_rx, &agent_name).await
        })
        .await
}

/// Reset the CLI session for an agent (delete the current session)
async fn reset_cli_session(agent: &crate::agent::Agent) -> Result<()> {
    use crate::engine::SimpleSession;
    use crate::session::index::SessionIndex;

    let agent_name = agent.name();
    // OpenClaw-compatible format: agent:{agent}:cli:default
    let session_key = format!("agent:{}:cli:default", agent_name);

    reset_cli_session_by_key(agent_name, &session_key).await
}

/// Reset the CLI session by key (delete the current session)
async fn reset_cli_session_by_key(agent_name: &str, session_key: &str) -> Result<()> {
    use crate::session::index::SessionIndex;

    // Get storage directory
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let storage_dir = home
        .join(".pekobot")
        .join("agents")
        .join(agent_name)
        .join("sessions");

    let mut index = SessionIndex::open(&storage_dir);

    // Find and remove the session entry from index
    if let Some(entry) = index.get(session_key).await? {
        let session_path = storage_dir.join(&entry.transcript_file);

        if session_path.exists() {
            tokio::fs::remove_file(&session_path).await?;
            info!("Deleted session file: {:?}", session_path);
        }

        // Remove from index
        index.remove(session_key).await?;
        info!("Removed session key {} from index", session_key);
    }

    Ok(())
}

/// List CLI sessions for all agents or a specific agent
async fn list_cli_sessions() -> Result<()> {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let agents_dir = home.join(".pekobot").join("agents");
    
    let mut all_sessions = Vec::new();
    
    // List all agents
    match tokio::fs::read_dir(&agents_dir).await {
        Ok(mut entries) => {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(metadata) = entry.metadata().await {
                    if metadata.is_dir() {
                        if let Some(agent_name) = entry.file_name().to_str() {
                            let sessions_dir = entry.path().join("sessions");
                            
                            match tokio::fs::read_dir(&sessions_dir).await {
                                Ok(mut session_entries) => {
                                    while let Ok(Some(session_entry)) = session_entries.next_entry().await {
                                        let path = session_entry.path();
                                        if path.extension().map_or(false, |e| e == "jsonl") {
                                            if let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) {
                                                if let Ok(metadata) = session_entry.metadata().await {
                                                    if let Ok(modified) = metadata.modified() {
                                                        let size = metadata.len();
                                                        all_sessions.push((
                                                            agent_name.to_string(),
                                                            session_id.to_string(),
                                                            modified,
                                                            size,
                                                        ));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(_) => continue,
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to read agents directory: {}", e));
        }
    }
    
    // Sort by modification time (newest first)
    all_sessions.sort_by(|a, b| b.2.cmp(&a.2));
    
    if all_sessions.is_empty() {
        println!("\n📭 No sessions found.");
    } else {
        println!("\n📋 Sessions ({} found):", all_sessions.len());
        println!();
        
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
