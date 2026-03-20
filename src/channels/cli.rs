//! CLI channel - Non-interactive terminal interface
//!
//! Presentation layer for CLI output. This module provides non-interactive
//! message sending capabilities for the `pekobot send` command.
//!
//! Per ADR-013, the CLI is fully non-interactive. Interactive chat has been
//! moved to the TUI (pekobot-tui) and Web UI.

use super::{Channel, StreamingConfig};
use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::session::context::SessionContext;
use crate::session::types::{ChannelType, Peer};

/// Command line interface channel
///
/// Used for non-interactive message sending via the `pekobot send` command.
pub struct CliChannel {
    name: String,
    streaming_config: StreamingConfig,
}

impl CliChannel {
    /// Create a new CLI channel with the given name
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_config(name, StreamingConfig::default())
    }

    /// Create a new CLI channel with custom streaming configuration
    pub fn with_config(name: impl Into<String>, streaming_config: StreamingConfig) -> Self {
        Self {
            name: name.into(),
            streaming_config,
        }
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
        // Non-interactive - never receives input
        Ok(None)
    }

    fn streaming_config(&self) -> StreamingConfig {
        self.streaming_config.clone()
    }
}

/// Process events and return final answer
///
/// Unified event handling for streaming output.
/// All output uses the same format: {`agent_name}`: {content}
pub async fn process_events(
    mut event_rx: tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>,
    agent_name: &str,
    session_ctx: Option<&crate::session::context::SessionContext>,
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
                    std::io::Write::flush(&mut std::io::stdout()).unwrap();
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
                        std::io::Write::flush(&mut std::io::stdout()).unwrap();
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
            AgenticEvent::Usage {
                prompt_tokens,
                completion_tokens,
                ..
            } => {
                // Record token usage to session context
                if let Some(ctx) = session_ctx {
                    if let Err(e) = ctx
                        .record_usage(prompt_tokens as usize, completion_tokens as usize)
                        .await
                    {
                        warn!("Failed to record token usage: {}", e);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(final_answer)
}

/// Send a single message to the agent and get a response (non-interactive)
///
/// Uses the session overlay architecture.
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
    let agent_name = agent.name().to_string();

    // Get or create session context
    let session_ctx = if new_session {
        info!("Starting new CLI session (explicit --new flag)");
        // Create new context (replaces any existing)
        let peer = Peer::User("default".to_string());
        let manager = agent.session_manager();
        let mut manager_guard = manager.write().await;

        // Remove existing CLI overlay if present
        let base_key = crate::session::derive_base_session_key(&agent_name, &peer);
        let overlay_key = format!("{base_key}:overlay:channel:cli:default");
        manager_guard.remove_channel_overlay(&overlay_key);

        // Remove old base session from cache (if any) before creating new one
        manager_guard.remove_base_session(&agent_name, &peer);

        // Create a new session - this caches it in base_sessions
        let new_session_id = manager_guard.create_new_session(&peer).await.ok();
        if let Some(ref sid) = new_session_id {
            info!("Created new session via registry: {}", sid);
        }

        let hybrid = manager_guard
            .get_session_for_channel(&agent_name, &peer, ChannelType::Cli, "default")
            .await?;


        SessionContext::new(hybrid).await
    } else {
        // Use agent's method to get context
        match agent.get_default_session_context().await {
            Ok(ctx) => ctx,
            Err(e) => {
                warn!("Failed to get session context: {}. Starting fresh.", e);
                agent.get_default_session_context().await?
            }
        }
    };

    // Load history (will be empty for new sessions)
    let history = session_ctx.load_history().await.ok();

    // With UnifiedSession, we no longer need to sync between two session types
    // The session is managed consistently through SessionContext
    let base = session_ctx.hybrid.base.read().await;
    let _unified_session: Option<crate::session::UnifiedSession> = if new_session {
        info!("Creating new session: {}", base.id);
        None
    } else {
        info!("Resuming session: {}", base.id);
        // Clone the UnifiedSession for the engine
        // We can't easily clone the session, so we'll use the session from context
        // The engine will use the same session through the context
        None
    };
    drop(base);

    // For backward compatibility with engine API, we pass None
    // The engine now creates its own UnifiedSession internally
    let base_session: Option<crate::session::UnifiedSession> = None;

    // Execute without LocalSet - the main.rs uses #[tokio::main] which provides a runtime
    // execute_streaming_with_session uses spawn_local which requires LocalSet
    // We need to create a LocalSet at the handle_agent_start level, not here
    let event_rx = agent
        .execute_streaming_with_session(message, base_session, history)
        .await?;
    let result = process_events(event_rx, &agent_name, Some(&session_ctx)).await;

    // Note: The engine (AgenticLoopV4) already adds both user and assistant messages
    // to the session during execution, so we don't need to add them manually here.

    result
}
