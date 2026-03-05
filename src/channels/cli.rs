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
use tracing::error;

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
                // Thinking/reasoning before tool calls - print inline
                if !text.is_empty() {
                    if !has_started_line {
                        print!("\n{}: ", agent_name);
                        has_started_line = true;
                    }
                    // Replace newlines with spaces for clean output
                    let single_line = text.replace('\n', " ");
                    print!("{}", single_line);
                    std::io::stdout().flush().unwrap();
                }
            }
            AgenticEvent::Assistant { text, is_final, .. } => {
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
                    Ok(_answer) => {
                        // Response already printed by process_events
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
/// Uses the same process_events as interactive mode
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
