//! CLI channel - Interactive terminal interface
//!
//! Presentation layer for CLI output with session overlay support.
//! Uses the hybrid session model for cross-channel context sharing.
//!
//! # Session Commands
//!
//! The CLI supports built-in session management commands:
//! - `/new` - Create a new empty session
//! - `/branch [label]` - Branch (fork) the current session  
//! - `/sessions` - List all sessions
//! - `/switch <n|id>` - Switch to a different session
//! - `/help` - Show available commands

use super::{Channel, StreamingConfig};
use anyhow::Result;
use async_trait::async_trait;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::session::context::SessionContext;
use crate::session::types::{ChannelType, Peer};

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

    /// Create a default session context for this CLI channel using the agent's session manager
    ///
    /// Uses peer-based session key: agent:{agent}:peer:user:default
    pub async fn create_session_context(
        &self,
        agent: &crate::agent::Agent,
    ) -> Result<SessionContext> {
        self.create_session_context_for_user(agent, "default").await
    }

    /// Create a session context for a specific user
    ///
    /// This enables multi-user CLI scenarios (e.g., shared terminal with user identification)
    pub async fn create_session_context_for_user(
        &self,
        agent: &crate::agent::Agent,
        username: &str,
    ) -> Result<SessionContext> {
        let agent_name = agent.name().to_string();
        let peer = Peer::User(username.to_string());
        let manager = agent.session_manager();
        let mut manager_guard = manager.write().await;

        let hybrid = manager_guard
            .get_session_for_channel(&agent_name, &peer, ChannelType::Cli, "default")
            .await?;

        Ok(SessionContext::new(hybrid).await)
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
        println!("{message}");
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
/// All output uses the same format: {`agent_name}`: {content}
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
                        print!("\n{agent_name}: ");
                        has_started_line = true;
                    } else if last_was_thinking {
                        // Continuing from previous thinking - add space
                        print!(" ");
                    }
                    // Replace newlines with spaces for clean output
                    let single_line = text.replace('\n', " ");
                    print!("{single_line}");
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
                            print!("\n{agent_name}: ");
                        }
                        println!("{text}");
                        final_answer = text;
                        has_started_line = false;
                    } else {
                        // Streaming delta - continue inline
                        if !has_started_line {
                            print!("\n{agent_name}: ");
                            has_started_line = true;
                        }
                        print!("{text}");
                        std::io::stdout().flush().unwrap();
                    }
                }
            }
            AgenticEvent::ToolStart { name: _, .. } => {
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
///
/// Uses the new session overlay architecture with peer-based session keys.
pub async fn run_interactive_loop(
    mut channel: CliChannel,
    agent: std::sync::Arc<std::sync::Mutex<crate::agent::Agent>>,
) -> Result<()> {
    use tokio::task::LocalSet;

    channel.print_banner();

    // Get agent name for logging
    let agent_name = {
        let agent_guard = agent.lock().unwrap();
        agent_guard.name().to_string()
    };

    // Create session context for default user
    let session_result = {
        let agent_guard = agent.lock().unwrap();
        channel.create_session_context(&agent_guard).await
    };

    let mut session_ctx = match session_result {
        Ok(ctx) => {
            info!("Created CLI session context for agent: {}", agent_name);

            // Load existing history if available
            match ctx.load_history().await {
                Ok(history) if !history.is_empty() => {
                    info!(
                        "📂 Resumed session with {} previous messages",
                        history.len()
                    );
                }
                _ => {
                    info!("🆕 Started new session");
                }
            }

            ctx
        }
        Err(e) => {
            warn!(
                "Failed to create session context: {}. Continuing without persistence.",
                e
            );
            // Create a fallback context without persistence
            return run_interactive_loop_without_persistence(channel, agent).await;
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
                if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
                    println!("\n👋 Goodbye!");
                    break;
                }

                if trimmed.eq_ignore_ascii_case("status") {
                    let agent = agent.lock().unwrap();
                    println!("\n📊 Agent Status: {:?}", agent.state());
                    channel.print_prompt();
                    continue;
                }

                // Handle session management commands
                if let Some(cmd_result) = handle_cli_session_command(
                    trimmed,
                    &channel,
                    &agent,
                    &agent_name,
                    &mut session_ctx,
                )
                .await
                {
                    match cmd_result {
                        Ok(true) => {
                            channel.print_prompt();
                            continue;
                        }
                        Ok(false) => {
                            // Not a session command, continue to normal processing
                        }
                        Err(e) => {
                            eprintln!("\n❌ Session command error: {e}");
                            channel.print_prompt();
                            continue;
                        }
                    }
                }

                // Add user message to session
                if let Err(e) = session_ctx.add_user_message(trimmed).await {
                    warn!("Failed to add user message to session: {}", e);
                }

                // Load history for the agent
                let history = match session_ctx.load_history().await {
                    Ok(h) => Some(h),
                    Err(e) => {
                        warn!("Failed to load history: {}", e);
                        None
                    }
                };

                // Process the message with session persistence
                let local = LocalSet::new();
                let result = local
                    .run_until(async {
                        let agent_lock = agent.lock().unwrap();

                        // Get the base session for resume
                        let base_session = {
                            let base = session_ctx.hybrid.base.read().await;
                            // Convert to SimpleSession for compatibility with existing API
                            crate::engine::SimpleSession::open_by_key(
                                &agent_name,
                                &base.session_key,
                            )
                            .await
                            .ok()
                            .flatten()
                        };

                        let event_rx = agent_lock
                            .execute_streaming_with_session(trimmed, base_session, history)
                            .await?;
                        process_events(event_rx, &agent_name).await
                    })
                    .await;

                match result {
                    Ok(answer) => {
                        // Add assistant response to session
                        if let Err(e) = session_ctx.add_assistant_message(&answer, None).await {
                            warn!("Failed to add assistant message to session: {}", e);
                        }
                    }
                    Err(e) => {
                        error!("Error in streaming: {}", e);
                        channel.print_error(&format!("Error: {e}"));
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

/// Fallback interactive loop without persistence
///
/// Used when session context creation fails.
async fn run_interactive_loop_without_persistence(
    mut channel: CliChannel,
    agent: std::sync::Arc<std::sync::Mutex<crate::agent::Agent>>,
) -> Result<()> {
    use tokio::task::LocalSet;

    let agent_name = {
        let agent_lock = agent.lock().unwrap();
        agent_lock.name().to_string()
    };

    println!("⚠️  Running without session persistence\n");
    channel.print_prompt();

    loop {
        match channel.stdin_rx.try_recv() {
            Ok(line) => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    channel.print_prompt();
                    continue;
                }

                if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
                    println!("\n👋 Goodbye!");
                    break;
                }

                let local = LocalSet::new();
                let result = local
                    .run_until(async {
                        let agent_lock = agent.lock().unwrap();
                        let event_rx = agent_lock.execute_streaming(trimmed).await?;
                        process_events(event_rx, &agent_name).await
                    })
                    .await;

                if let Err(e) = result {
                    error!("Error in streaming: {}", e);
                    channel.print_error(&format!("Error: {e}"));
                }

                {
                    let agent_lock = agent.lock().unwrap();
                    agent_lock.set_state(crate::types::agent::AgentState::Idle);
                }

                channel.print_prompt();
            }
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
        }
    }

    Ok(())
}

/// Handle CLI session commands
///
/// Returns:
/// - `Some(Ok(true))` - Command was handled (continue loop)
/// - `Some(Ok(false))` - Not a session command (process normally)
/// - `Some(Err(_))` - Error handling command
/// - `None` - Should not happen (placeholder)
async fn handle_cli_session_command(
    trimmed: &str,
    channel: &CliChannel,
    agent: &std::sync::Arc<std::sync::Mutex<crate::agent::Agent>>,
    _agent_name: &str,
    session_ctx: &mut SessionContext,
) -> Option<Result<bool>> {
    // Quick check if it looks like a session command
    if !trimmed.starts_with('/') && trimmed != "help" {
        return Some(Ok(false));
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.is_empty() {
        return Some(Ok(false));
    }

    let cmd = parts[0].to_lowercase();
    let peer_key = "default".to_string();

    // Get session manager reference
    let session_manager = {
        let agent_guard = agent.lock().unwrap();
        agent_guard.session_manager().clone()
    };

    // Get registry manager reference
    let registry_manager = {
        let manager = session_manager.read().await;
        manager.registry().cloned()
    };

    // If no registry, we can't handle session commands
    let registry = match registry_manager {
        Some(r) => r,
        None => return Some(Ok(false)),
    };

    match cmd.as_str() {
        "/new" => {
            let new_session_id = match registry.create_new(&peer_key).await {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            println!(
                "\n✅ Created and switched to new session: {new_session_id}"
            );

            // Reload session context
            {
                let agent_guard = agent.lock().unwrap();
                match channel.create_session_context(&agent_guard).await {
                    Ok(new_ctx) => {
                        *session_ctx = new_ctx;
                        println!("🆕 New session started");
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to reload session context: {e}");
                    }
                }
            }
            Some(Ok(true))
        }
        "/branch" => {
            let label = parts.get(1..).map(|s| s.join(" "));

            let branch_id = match registry.branch(&peer_key, label.clone()).await {
                Ok(id) => id,
                Err(e) => {
                    if e.to_string().contains("No active session") {
                        println!("\n❌ No active session to branch from");
                        return Some(Ok(true));
                    }
                    return Some(Err(e));
                }
            };

            if let Some(lbl) = label {
                println!(
                    "\n✅ Branched to new session: {branch_id} (label: {lbl})"
                );
            } else {
                println!("\n✅ Branched to new session: {branch_id}");
            }

            // Reload session context
            {
                let agent_guard = agent.lock().unwrap();
                match channel.create_session_context(&agent_guard).await {
                    Ok(new_ctx) => {
                        *session_ctx = new_ctx;
                        println!("🌿 Branched session loaded");
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to reload session context: {e}");
                    }
                }
            }
            Some(Ok(true))
        }
        "/sessions" => {
            let sessions = match registry.list_sessions(&peer_key).await {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let active = match registry.get_active_session_id(&peer_key).await {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };

            println!("\n📁 Sessions:");
            if sessions.is_empty() {
                println!("   No sessions found.");
            } else {
                for (i, session) in sessions.iter().enumerate() {
                    let is_active = active.as_ref() == Some(&session.session_id);
                    let marker = if is_active { "▶" } else { " " };
                    let label_display = session
                        .label
                        .as_ref()
                        .map(|l| format!(" [{l}]"))
                        .unwrap_or_default();
                    let short_id = if session.session_id.len() > 8 {
                        &session.session_id[..8]
                    } else {
                        &session.session_id
                    };
                    println!(
                        "   [{}] {}{} {}{}",
                        i + 1,
                        marker,
                        short_id,
                        label_display,
                        if is_active { " (active)" } else { "" }
                    );
                }
            }
            println!();
            Some(Ok(true))
        }
        "/switch" => {
            if parts.len() < 2 {
                println!("\n❌ Usage: /switch <n|id> - switch to session by number or ID");
                return Some(Ok(true));
            }

            let target = parts[1];
            let sessions = match registry.list_sessions(&peer_key).await {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };

            // Try to parse as number first
            let session_id = if let Ok(num) = target.parse::<usize>() {
                if num == 0 || num > sessions.len() {
                    println!(
                        "\n❌ Invalid session number. Use /sessions to see available sessions."
                    );
                    return Some(Ok(true));
                }
                sessions[num - 1].session_id.clone()
            } else {
                // Use as UUID directly
                target.to_string()
            };

            if let Err(e) = registry.switch_session(&peer_key, &session_id).await {
                return Some(Err(e));
            }

            println!("\n✅ Switched to session: {session_id}");

            // Reload session context
            {
                let agent_guard = agent.lock().unwrap();
                match channel.create_session_context(&agent_guard).await {
                    Ok(new_ctx) => {
                        *session_ctx = new_ctx;
                        // Show session info
                        match session_ctx.load_history().await {
                            Ok(history) => {
                                if history.is_empty() {
                                    println!("📂 Empty session");
                                } else {
                                    println!("📂 Session with {} messages loaded", history.len());
                                }
                            }
                            Err(_) => {
                                println!("🆕 New session");
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to reload session context: {e}");
                    }
                }
            }
            Some(Ok(true))
        }
        "/help" => {
            println!("\n📖 Session Commands:");
            println!("   /new          - Create a new empty session");
            println!("   /branch [lbl] - Fork the current session with optional label");
            println!("   /sessions     - List all sessions");
            println!("   /switch <n>   - Switch to session #n from /sessions list");
            println!("   /help         - Show this help message\n");
            Some(Ok(true))
        }
        _ => {
            // Not a session command
            Some(Ok(false))
        }
    }
}

/// Send a single message to the agent and get a response (non-interactive)
///
/// Uses the new session overlay architecture.
pub async fn send_single_message(agent: &crate::agent::Agent, message: &str) -> Result<String> {
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

    // Get or create session context
    let session_ctx = if new_session {
        info!("Starting new CLI session (explicit --new flag)");
        // Create new context (replaces any existing)
        let peer = Peer::User("default".to_string());
        let manager = agent.session_manager();
        let mut manager_guard = manager.write().await;

        // Create a new session via registry (if available)
        let new_session_id = manager_guard.create_new_session(&peer).await.ok();
        if let Some(ref sid) = new_session_id {
            info!("Created new session via registry: {}", sid);
        }

        // Remove existing CLI overlay if present
        let base_key = crate::session::derive_base_session_key(&agent_name, &peer);
        let overlay_key = format!("{base_key}:overlay:channel:cli:default");
        manager_guard.remove_channel_overlay(&overlay_key);

        // Also remove from base_sessions cache to force re-creation
        manager_guard.remove_base_session(&agent_name, &peer);

        let hybrid = manager_guard
            .get_session_for_channel(&agent_name, &peer, ChannelType::Cli, "default")
            .await?;

        println!("🆕 Created new session");
        if let Some(sid) = new_session_id {
            println!("   Session ID: {sid}");
        }
        SessionContext::new(hybrid).await
    } else {
        // Use agent's method to get context
        match agent.get_default_session_context().await {
            Ok(ctx) => {
                // Check if we have history
                match ctx.load_history().await {
                    Ok(history) if !history.is_empty() => {
                        info!(
                            "📂 Resumed session with {} previous messages",
                            history.len()
                        );
                    }
                    _ => {}
                }
                ctx
            }
            Err(e) => {
                warn!("Failed to get session context: {}. Starting fresh.", e);
                agent.get_default_session_context().await?
            }
        }
    };

    // Load history BEFORE executing - the engine will add the new message
    let history = session_ctx.load_history().await.ok();

    // Create a LocalSet for the streaming execution
    let local = LocalSet::new();

    let result = local
        .run_until(async {
            // Get base session for resume
            let base_session = {
                let base = session_ctx.hybrid.base.read().await;
                crate::engine::SimpleSession::open_by_key(&agent_name, &base.session_key)
                    .await
                    .ok()
                    .flatten()
            };

            // The engine handles adding user message and assistant response
            // We don't need to manually add them here
            let event_rx = agent
                .execute_streaming_with_session(message, base_session, history)
                .await?;
            process_events(event_rx, &agent_name).await
        })
        .await?;

    // Note: The engine (AgenticLoopV4) already adds both user and assistant messages
    // to the session during execution, so we don't need to add them manually here.
    // This fixes the message duplication issue.

    Ok(result)
}

/// Reset the CLI session for an agent (create new session)
async fn reset_cli_session(agent: &crate::agent::Agent) -> Result<()> {
    let agent_name = agent.name();
    let peer = Peer::User("default".to_string());

    // Get session manager and remove the CLI overlay
    let manager = agent.session_manager();
    let mut manager_guard = manager.write().await;

    let base_key = crate::session::derive_base_session_key(agent_name, &peer);
    let overlay_key = format!("{base_key}:overlay:channel:cli:default");

    if manager_guard.remove_channel_overlay(&overlay_key).is_some() {
        info!("Removed CLI session overlay for agent: {}", agent_name);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires tokio runtime and filesystem access"]
    async fn test_cli_channel_creation() {
        let channel = CliChannel::new("test");
        assert_eq!(channel.name(), "test");
    }

    #[test]
    fn test_format_time_ago() {
        let now = std::time::SystemTime::now();
        assert_eq!(format_time_ago(now), "just now");

        let past = now - std::time::Duration::from_secs(120);
        assert_eq!(format_time_ago(past), "2m ago");

        let past = now - std::time::Duration::from_secs(7200);
        assert_eq!(format_time_ago(past), "2h ago");
    }
}
