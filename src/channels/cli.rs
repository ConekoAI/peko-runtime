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
use tracing::{debug, error};

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

/// Presentation state for streaming output
struct PresentationState {
    agent_name: String,
    has_started_response: bool,
    in_streaming: bool,
    printed_content: String, // Track what we've already printed
}

impl PresentationState {
    fn new(agent_name: String) -> Self {
        Self {
            agent_name,
            has_started_response: false,
            in_streaming: false,
            printed_content: String::new(),
        }
    }

    /// Start a new response line if not already started
    fn start_response(&mut self) {
        if !self.has_started_response {
            print!("\n{}: ", self.agent_name);
            self.has_started_response = true;
            self.in_streaming = true;
        }
    }

    /// Print text content (streaming)
    fn print_text(&mut self, text: &str) {
        self.start_response();
        print!("{}", text);
        self.printed_content.push_str(text);
        std::io::stdout().flush().unwrap();
    }

    /// Print final content and add to tracking
    fn print_final(&mut self, text: &str) {
        if !text.is_empty() {
            print!("{}", text);
            self.printed_content.push_str(text);
        }
        println!();
        std::io::stdout().flush().unwrap();
        self.in_streaming = false;
        self.has_started_response = false;
    }

    /// Check if content was already printed
    fn is_duplicate(&self, text: &str) -> bool {
        // If we've already printed this exact text
        if text == self.printed_content {
            return true;
        }
        // If what we have is a prefix of the new text, it's a continuation not a duplicate
        if text.starts_with(&self.printed_content) && !self.printed_content.is_empty() {
            return false;
        }
        // If new text is contained in what we printed
        self.printed_content.contains(text)
    }

    /// Get only the new part of text that hasn't been printed
    fn get_new_content(&self, text: &str) -> String {
        if self.printed_content.is_empty() {
            return text.to_string();
        }
        // If text starts with what we printed, return the remainder
        if text.starts_with(&self.printed_content) {
            return text[self.printed_content.len()..].to_string();
        }
        // Otherwise return full text (might be different content)
        text.to_string()
    }

    /// End the current response stream
    fn end_stream(&mut self) {
        if self.in_streaming {
            println!();
            self.in_streaming = false;
            self.has_started_response = false;
        }
    }
}

/// Process events and return final answer
/// 
/// Presentation-layer function - handles how events are displayed to the user.
/// Other channels (Discord, WhatsApp) can implement their own version.
async fn process_events(
    mut event_rx: tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>,
    agent_name: &str,
) -> Result<String> {
    use crate::engine::{AgenticEvent, LifecyclePhase};
    
    let mut final_answer = String::new();
    let mut state = PresentationState::new(agent_name.to_string());

    while let Some(event) = event_rx.recv().await {
        match event {
            AgenticEvent::Lifecycle { phase, .. } => match phase {
                LifecyclePhase::End => {
                    state.end_stream();
                    break;
                }
                LifecyclePhase::Error => {
                    return Err(anyhow::anyhow!("Agent encountered an error"));
                }
                _ => {}
            },
            AgenticEvent::Thinking { text, .. } => {
                // Treat reasoning/thinking as normal agent text
                // Get only the new content that hasn't been printed
                let new_content = state.get_new_content(&text);
                if !new_content.is_empty() {
                    // Replace newlines with spaces for clean single-line output
                    let single_line = new_content.replace('\n', " ");
                    // Ensure space before new content if already printing
                    let spacer = if state.in_streaming { " " } else { "" };
                    state.print_text(&format!("{}{}", spacer, single_line));
                }
            }
            AgenticEvent::Assistant { text, is_final, .. } => {
                if !text.is_empty() {
                    // Get only the new content that hasn't been printed
                    let new_content = state.get_new_content(&text);
                    
                    if is_final {
                        // Final answer - print any remaining content and finish
                        if !new_content.is_empty() {
                            if !state.has_started_response {
                                print!("\n{}: ", agent_name);
                            } else if state.in_streaming {
                                // Already streaming, add space if needed
                                print!(" ");
                            }
                            println!("{}", new_content);
                        } else if state.has_started_response {
                            // Already printed everything, just end the line
                            println!();
                        }
                        final_answer = text;
                        state.in_streaming = false;
                        state.has_started_response = false;
                    } else if !new_content.is_empty() {
                        // Streaming delta - print new content
                        if !state.has_started_response {
                            print!("\n{}: ", agent_name);
                        }
                        print!("{}", new_content);
                        std::io::stdout().flush().unwrap();
                    }
                }
            }
            AgenticEvent::ToolStart { name, .. } => {
                // Tools are working in the background - logged at DEBUG only
                debug!("{} using tool: {}", agent_name, name);
            }
            AgenticEvent::ToolEnd { tool_id, success, .. } => {
                debug!("{} tool '{}' completed (success: {})", agent_name, tool_id, success);
            }
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

                // Process the message
                let agent_lock = agent.lock().unwrap();
                let agent_name = agent_lock.name().to_string();
                
                // Create a LocalSet for the streaming execution
                let local = LocalSet::new();
                let result = local
                    .run_until(async {
                        let event_rx = agent_lock.execute_streaming(trimmed).await?;
                        process_events(event_rx, &agent_name).await
                    })
                    .await;

                match result {
                    Ok(answer) => {
                        if answer.is_empty() {
                            println!("\n⚠️  No response received");
                        }
                    }
                    Err(e) => {
                        error!("Error in streaming: {}", e);
                        channel.print_error(&format!("Error: {}", e));
                    }
                }

                // Reset agent state to Idle for next message
                agent_lock.set_state(crate::types::agent::AgentState::Idle);
                
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
pub async fn send_single_message(
    agent: &crate::agent::Agent,
    message: &str,
) -> Result<String> {
    use tokio::task::LocalSet;

    let agent_name = agent.name().to_string();
    
    // Create a LocalSet for the streaming execution
    let local = LocalSet::new();

    local
        .run_until(async {
            let event_rx = agent.execute_streaming(message).await?;
            process_events(event_rx, &agent_name).await
        })
        .await
}

/// Send a single message with tools and get a response
pub async fn send_single_message_with_tools(
    agent: &crate::agent::Agent,
    message: &str,
) -> Result<String> {
    // For now, use the same implementation
    send_single_message(agent, message).await
}
