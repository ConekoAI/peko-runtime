//! CLI channel - Non-interactive terminal interface
//!
//! Presentation layer for CLI output. This module provides non-interactive
//! message sending capabilities for the `pekobot send` command.
//!
//! Per ADR-013, the CLI is fully non-interactive. Interactive chat has been
//! moved to the TUI (pekobot-tui) and Web UI.
//!
//! Per ADR-015, this channel supports both blocking and streaming modes
//! through the unified `EventStream` interface.

use super::{Channel, ChannelOutput, EventStream, StreamingConfig};
use anyhow::Result;
use async_trait::async_trait;
use std::io::Write;
use std::time::Duration;
use tracing::{info, warn};

use crate::session::context::SessionContext;
use crate::session::types::{ChannelType, Peer};

/// CLI channel operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CliMode {
    /// Collect output and return final result (default for scripts)
    #[default]
    Blocking,
    /// Print tokens as they arrive (for interactive use)
    Streaming,
}

/// Command line interface channel
///
/// Used for non-interactive message sending via the `pekobot send` command.
/// Supports both blocking (default) and streaming modes per ADR-015.
pub struct CliChannel {
    name: String,
    streaming_config: StreamingConfig,
    mode: CliMode,
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
            mode: CliMode::Blocking,
        }
    }

    /// Set the operating mode (blocking or streaming)
    #[must_use] 
    pub fn with_mode(mut self, mode: CliMode) -> Self {
        self.mode = mode;
        self
    }

    /// Get the current mode
    #[must_use] 
    pub fn mode(&self) -> CliMode {
        self.mode
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

    /// Process event stream according to CLI mode
    ///
    /// - Blocking mode: Collect all events and return final output
    /// - Streaming mode: Print tokens as they arrive for real-time feedback
    async fn process_stream(&self, event_stream: EventStream) -> Result<ChannelOutput> {
        match self.mode {
            CliMode::Blocking => {
                // Use default implementation: collect events into output
                self.process_stream_blocking(event_stream).await
            }
            CliMode::Streaming => {
                // Stream tokens to stdout in real-time
                self.process_stream_streaming(event_stream).await
            }
        }
    }
}

impl CliChannel {
    /// Blocking mode: Collect events into `ChannelOutput`
    ///
    /// Uses the shared default implementation and adds CLI-specific formatting.
    async fn process_stream_blocking(&self, event_stream: EventStream) -> Result<ChannelOutput> {
        // Get the base output from the shared default implementation
        let mut output = crate::channels::default_process_stream(event_stream).await?;

        // Print error if the stream failed (blocking mode doesn't print inline)
        if !output.success {
            if let Some(ref err) = output.error {
                eprintln!("\n❌ Error: {err}");
            }
        }

        // Add CLI-specific formatting: agent name prefix
        if !output.final_text.is_empty() {
            output.final_text = format!("{}: {}", self.name, output.final_text);
        }

        Ok(output)
    }

    /// Streaming mode: Print tokens as they arrive
    ///
    /// ADR-016: Now uses completion signal instead of 500ms grace period
    async fn process_stream_streaming(&self, event_stream: EventStream) -> Result<ChannelOutput> {
        use crate::engine::{AgenticEvent, ChannelAction, EventProcessor, LifecyclePhase};

        let mut output = ChannelOutput::new(&event_stream.session_id);
        output.is_new_session = event_stream.is_new_session;

        let mut event_rx = event_stream.receiver;
        let completion = event_stream.completion;
        let mut processor = EventProcessor::for_agent(&self.name);
        let mut final_answer = String::new();
        let mut has_started_line = false;
        let mut end_received = false;

        while let Some(event) = event_rx.recv().await {
            // Process through EventProcessor for proper formatting
            let actions = processor.process(&event);

            for action in actions {
                match action {
                    ChannelAction::StartTurn(name) => {
                        if !has_started_line {
                            print!("\n{name}: ");
                            std::io::stdout().flush().unwrap();
                            has_started_line = true;
                        }
                    }
                    ChannelAction::Print(text) => {
                        print!("{text}");
                        std::io::stdout().flush().unwrap();
                    }
                    ChannelAction::Println(text) => {
                        if text.is_empty() {
                            println!();
                        } else {
                            println!("{text}");
                            final_answer = text;
                        }
                        has_started_line = false;
                    }
                    ChannelAction::Flush => {
                        std::io::stdout().flush().unwrap();
                    }
                    ChannelAction::EndTurn => {
                        has_started_line = false;
                    }
                    ChannelAction::Status(_) => {
                        // CLI doesn't show status messages inline
                    }
                }
            }

            // Collect usage info
            if let AgenticEvent::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } = &event
            {
                output.usage.input = u64::from(*prompt_tokens);
                output.usage.output = u64::from(*completion_tokens);
                output.usage.total = u64::from(*total_tokens);
            }

            // Handle lifecycle
            if let AgenticEvent::Lifecycle { phase, error, .. } = &event {
                match phase {
                    LifecyclePhase::End => {
                        if has_started_line {
                            println!();
                        }
                        end_received = true;
                        // Don't break yet - wait for receiver to close
                    }
                    LifecyclePhase::Error => {
                        output.success = false;
                        output.error = error.clone();
                        end_received = true;
                        if let Some(ref err) = error {
                            eprintln!("\n❌ Error: {err}");
                        }
                    }
                    _ => {}
                }
            }
        }

        // Receiver closed - CRITICAL: Wait for completion signal before returning
        // ADR-016: This ensures session persistence is complete (replaces 500ms sleep)
        if end_received {
            match tokio::time::timeout(std::time::Duration::from_secs(30), completion).await {
                Ok(Ok(Ok(()))) => {
                    // Session persistence complete
                }
                Ok(Ok(Err(e))) => {
                    // Only log if the stream itself didn't already report an error.
                    // Stream errors are communicated via LifecyclePhase::Error events.
                    if output.success {
                        warn!("Session persistence failed: {}", e);
                    }
                }
                Ok(Err(_)) => {
                    warn!("Completion sender dropped without signal");
                }
                Err(_) => {
                    warn!("Completion timeout - session persistence may be incomplete");
                }
            }
        }

        output.final_text = final_answer;
        Ok(output)
    }
}

/// Process events and return final answer
///
/// Unified event handling for streaming output using `EventProcessor`.
/// All output uses the same format: {`agent_name}`: {content}
pub async fn process_events(
    mut event_rx: tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>,
    agent_name: &str,
    session_ctx: Option<&crate::session::context::SessionContext>,
) -> Result<String> {
    use crate::engine::{AgenticEvent, ChannelAction, EventProcessor, LifecyclePhase};

    let mut processor = EventProcessor::for_agent(agent_name);
    let mut final_answer = String::new();
    let mut has_started_line = false;

    while let Some(event) = event_rx.recv().await {
        // Handle Usage event separately (needs async)
        if let AgenticEvent::Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            ..
        } = &event
        {
            if let Some(ctx) = session_ctx {
                if let Err(e) = ctx
                    .record_usage(
                        *total_tokens as usize,
                        *prompt_tokens as usize,
                        *completion_tokens as usize,
                    )
                    .await
                {
                    warn!("Failed to record token usage: {}", e);
                }
            }
        }

        // Process event through EventProcessor
        let actions = processor.process(&event);

        for action in actions {
            match action {
                ChannelAction::StartTurn(name) => {
                    if !has_started_line {
                        print!("\n{name}: ");
                        std::io::stdout().flush().unwrap();
                        has_started_line = true;
                    }
                }
                ChannelAction::Print(text) => {
                    print!("{text}");
                    std::io::stdout().flush().unwrap();
                }
                ChannelAction::Println(text) => {
                    if text.is_empty() {
                        println!();
                    } else {
                        println!("{text}");
                        final_answer = text;
                    }
                    has_started_line = false;
                }
                ChannelAction::Flush => {
                    std::io::stdout().flush().unwrap();
                }
                ChannelAction::EndTurn => {
                    has_started_line = false;
                }
                ChannelAction::Status(_) => {
                    // CLI doesn't show status messages inline
                }
            }
        }

        // Handle lifecycle events
        if let AgenticEvent::Lifecycle { phase, .. } = &event {
            match phase {
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
            }
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
        use crate::session::manager::SessionCreateOptions;
        let options = SessionCreateOptions::new().with_trigger("user");
        let new_session_id = manager_guard
            .create_session(&agent_name, &peer, options)
            .await
            .ok()
            .map(|h| h.session_id().to_string());
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
    // CRITICAL: Use filter to convert empty history to None, so the engine
    // knows to add the system prompt for fresh sessions
    let history = session_ctx
        .load_history()
        .await
        .ok()
        .filter(|h| !h.is_empty());

    // Get the session from context to pass to engine
    // The engine will use the same session through the Arc<RwLock<>>
    let base_session = session_ctx.hybrid.base.clone();

    // Execute with streaming - use channel to collect events
    // Create channel for events
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<crate::engine::AgenticEvent>(10000);

    // Execute directly - no LocalSet needed since execute_streaming_with_session
    // doesn't use spawn_local anymore (it runs synchronously)
    let on_event = move |event: crate::engine::AgenticEvent| {
        let _ = event_tx.try_send(event);
    };

    let _result = agent
        .execute_streaming_with_session(message, base_session, history, on_event)
        .await;

    // Process events from the channel
    let process_result = process_events(event_rx, &agent_name, Some(&session_ctx)).await;

    // Wait for any background async tasks to complete before returning.
    // This ensures task files are fully written and background work is not
    // dropped when the CLI process exits.
    let wait_timeout = std::time::Duration::from_secs(30);
    agent.wait_for_async_tasks(wait_timeout).await;

    // Note: The engine (AgenticLoopV4) already adds both user and assistant messages
    // to the session during execution, so we don't need to add them manually here.

    // Return the process result (contains the collected output)
    process_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::EventStream;
    use crate::engine::{AgenticEvent, LifecyclePhase};

    /// Test that `CliChannel` creation works
    #[test]
    fn test_cli_channel_creation() {
        let channel = CliChannel::new("test-agent");
        assert_eq!(channel.name(), "test-agent");
        assert_eq!(channel.mode(), CliMode::Blocking);
    }

    /// Test CLI channel mode switching
    #[test]
    fn test_cli_channel_mode() {
        let channel = CliChannel::new("test-agent").with_mode(CliMode::Streaming);

        assert_eq!(channel.mode(), CliMode::Streaming);
    }

    /// Test blocking mode with completion signal (ADR-016)
    ///
    /// Verifies that the channel properly awaits the completion signal
    /// instead of using the 500ms grace period.
    #[tokio::test]
    async fn test_cli_channel_blocking_with_completion_signal() {
        let channel = CliChannel::new("test-agent");

        // Create a mock EventStream with completion signal
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(100);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();

        let event_stream = EventStream {
            receiver: event_rx,
            completion: completion_rx,
            session_id: "test-session".to_string(),
            is_new_session: true,
        };

        // Spawn a task to send events and completion signal
        tokio::spawn(async move {
            // Send some text events
            let _ = event_tx
                .send(AgenticEvent::AssistantText {
                    run_id: "run_1".to_string(),
                    text: "Hello".to_string(),
                    sequence: 1,
                    is_interstitial: false,
                })
                .await;

            // Send End lifecycle event
            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: "run_1".to_string(),
                    phase: LifecyclePhase::End,
                    error: None,
                })
                .await;

            // Drop the sender to close the channel
            drop(event_tx);

            // Send completion signal after a short delay (simulating session write)
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = completion_tx.send(Ok(()));
        });

        // Process the stream
        let start = std::time::Instant::now();
        let result = channel.process_stream(event_stream).await;
        let elapsed = start.elapsed();

        // Should complete quickly (no 500ms grace period)
        assert!(result.is_ok());
        assert!(
            elapsed < Duration::from_millis(200),
            "Should complete in under 200ms without grace period, took {elapsed:?}"
        );

        let output = result.unwrap();
        assert_eq!(output.session_id, "test-session");
        assert!(output.is_new_session);
        assert!(output.final_text.contains("Hello"));
    }

    /// Test that completion signal error is handled gracefully
    #[tokio::test]
    async fn test_cli_channel_completion_error_handling() {
        let channel = CliChannel::new("test-agent");

        // Create a mock EventStream
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(100);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();

        let event_stream = EventStream {
            receiver: event_rx,
            completion: completion_rx,
            session_id: "test-session".to_string(),
            is_new_session: false,
        };

        // Spawn a task that sends an error completion
        tokio::spawn(async move {
            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: "run_1".to_string(),
                    phase: LifecyclePhase::End,
                    error: None,
                })
                .await;

            drop(event_tx);

            // Send error completion
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = completion_tx.send(Err(anyhow::anyhow!("Session write failed")));
        });

        // Should complete without panic even with completion error
        let result = channel.process_stream(event_stream).await;
        assert!(result.is_ok());
    }

    /// Test streaming mode with completion signal
    #[tokio::test]
    async fn test_cli_channel_streaming_mode() {
        let channel = CliChannel::new("test-agent").with_mode(CliMode::Streaming);

        assert_eq!(channel.mode(), CliMode::Streaming);

        // Create a mock EventStream
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(100);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();

        let event_stream = EventStream {
            receiver: event_rx,
            completion: completion_rx,
            session_id: "test-session".to_string(),
            is_new_session: true,
        };

        // Spawn a task to send events
        tokio::spawn(async move {
            let _ = event_tx
                .send(AgenticEvent::AssistantText {
                    run_id: "run_1".to_string(),
                    text: "Streaming".to_string(),
                    sequence: 1,
                    is_interstitial: false,
                })
                .await;

            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: "run_1".to_string(),
                    phase: LifecyclePhase::End,
                    error: None,
                })
                .await;

            drop(event_tx);
            let _ = completion_tx.send(Ok(()));
        });

        // Process in streaming mode
        let start = std::time::Instant::now();
        let result = channel.process_stream(event_stream).await;
        let elapsed = start.elapsed();

        // Should complete without 500ms grace period
        assert!(result.is_ok());
        assert!(
            elapsed < Duration::from_millis(200),
            "Streaming mode should complete without grace period"
        );
    }

    /// Test error lifecycle handling
    #[tokio::test]
    async fn test_cli_channel_error_lifecycle() {
        let channel = CliChannel::new("test-agent");

        let (event_tx, event_rx) = tokio::sync::mpsc::channel(100);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();

        let event_stream = EventStream {
            receiver: event_rx,
            completion: completion_rx,
            session_id: "test-session".to_string(),
            is_new_session: true,
        };

        tokio::spawn(async move {
            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: "run_1".to_string(),
                    phase: LifecyclePhase::Error,
                    error: Some("Test error".to_string()),
                })
                .await;

            drop(event_tx);
            let _ = completion_tx.send(Ok(()));
        });

        let result = channel.process_stream(event_stream).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.success);
        assert_eq!(output.error, Some("Test error".to_string()));
    }
}
