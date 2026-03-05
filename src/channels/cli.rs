//! CLI channel - Interactive terminal interface

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

    /// Print agent response
    pub fn print_agent_response(&self, response: &str) {
        println!("\n🐱 Agent: {response}");
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
/// Only shows: thinking 💭, agent response 🐱, and errors ❌
/// Tool usage is logged at DEBUG level (visible with -v)
async fn process_events(
    mut event_rx: tokio::sync::mpsc::Receiver<crate::engine::AgenticEvent>,
) -> Result<String> {
    use crate::engine::{AgenticEvent, LifecyclePhase};
    
    let mut final_answer = String::new();
    let mut has_printed_thinking = false;
    let mut in_thinking = false;

    while let Some(event) = event_rx.recv().await {
        match event {
            AgenticEvent::Lifecycle { phase, .. } => match phase {
                LifecyclePhase::End => {
                    if in_thinking {
                        println!();
                    }
                    break;
                }
                LifecyclePhase::Error => {
                    return Err(anyhow::anyhow!("Agent encountered an error"));
                }
                _ => {}
            },
            AgenticEvent::Thinking { text, is_delta, .. } => {
                if is_delta && !text.is_empty() {
                    if !has_printed_thinking {
                        print!("\n💭 ");
                        has_printed_thinking = true;
                        in_thinking = true;
                    }
                    print!("{}", text);
                    std::io::stdout().flush().unwrap();
                }
            }
            AgenticEvent::Assistant { text, is_delta, is_final, .. } => {
                if is_final && !text.is_empty() {
                    if in_thinking {
                        println!();
                        in_thinking = false;
                    }
                    println!("\n🐱 Agent: {}", text);
                    final_answer = text;
                } else if is_delta && in_thinking {
                    // Stream reasoning tokens
                    print!("{}", text);
                    std::io::stdout().flush().unwrap();
                }
            }
            AgenticEvent::ToolStart { name, .. } => {
                if in_thinking {
                    println!();
                    in_thinking = false;
                }
                debug!("Using tool: {}", name);
            }
            AgenticEvent::ToolEnd { tool_id, success, .. } => {
                debug!("Tool '{}' completed (success: {})", tool_id, success);
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
                let agent = agent.lock().unwrap();
                
                // Create a LocalSet for the streaming execution
                let local = LocalSet::new();
                let result = local
                    .run_until(async {
                        let event_rx = agent.execute_streaming(trimmed).await?;
                        process_events(event_rx).await
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
                agent.set_state(crate::types::agent::AgentState::Idle);
                
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

    // Create a LocalSet for the streaming execution
    let local = LocalSet::new();

    local
        .run_until(async {
            let event_rx = agent.execute_streaming(message).await?;
            process_events(event_rx).await
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
