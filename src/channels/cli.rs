//! CLI channel - Interactive terminal interface

use super::Channel;
use anyhow::Result;
use async_trait::async_trait;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// Command line interface channel with interactive input
pub struct CliChannel {
    name: String,
    stdin_tx: mpsc::Sender<String>,
    stdin_rx: mpsc::Receiver<String>,
    _input_handle: tokio::task::JoinHandle<()>,
}

impl CliChannel {
    /// Create a new CLI channel with the given name
    pub fn new(name: impl Into<String>) -> Self {
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

    /// Print system message
    pub fn print_system(&self, message: &str) {
        println!("\n⚡ {message}");
    }

    /// Print error
    pub fn print_error(&self, error: &str) {
        eprintln!("\n❌ Error: {error}");
    }

    /// Print tool start
    pub fn print_tool_start(&self, name: &str) {
        println!("\n🔧 Using tool: {name}");
    }

    /// Print tool result
    pub fn print_tool_result(&self, name: &str, success: bool) {
        let icon = if success { "✅" } else { "❌" };
        println!("{icon} Tool '{name}' completed");
    }

    /// Handle an agentic event
    pub fn handle_event(&self, event: &crate::engine::AgenticEvent) {
        use crate::engine::AgenticEvent;

        match event {
            AgenticEvent::Lifecycle { phase, .. } => {
                match phase {
                    crate::engine::LifecyclePhase::Start => {
                        // Silent - already printed banner
                    }
                    crate::engine::LifecyclePhase::Running => {
                        self.print_system("Thinking...");
                    }
                    crate::engine::LifecyclePhase::End => {
                        // Silent - response printed separately
                    }
                    crate::engine::LifecyclePhase::Error => {
                        self.print_error("Agent encountered an error");
                    }
                    crate::engine::LifecyclePhase::Aborted => {
                        self.print_system("Agent aborted");
                    }
                }
            }
            AgenticEvent::Assistant { text, is_final, .. } => {
                if *is_final {
                    self.print_agent_response(text);
                }
                // Deltas handled by streaming mode (not implemented yet)
            }
            AgenticEvent::ToolStart { name, .. } => {
                self.print_tool_start(name);
            }
            AgenticEvent::ToolEnd { tool_id, success, .. } => {
                self.print_tool_result(tool_id, *success);
            }
            AgenticEvent::Status { message, .. } => {
                self.print_system(message);
            }
            _ => {
                // Other events not displayed in CLI
            }
        }
    }
}

#[async_trait]
impl Channel for CliChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&mut self, message: &str) -> Result<()> {
        self.print_agent_response(message);
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<String>> {
        // Try to receive from stdin channel with timeout
        match tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            self.stdin_rx.recv(),
        )
        .await
        {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(line))
                }
            }
            Ok(None) => Ok(None), // Channel closed
            Err(_) => Ok(None),   // Timeout - no input available
        }
    }
}

/// Interactive conversation loop for CLI with provider support
pub async fn run_interactive_loop_with_agent(
    channel: &mut CliChannel,
    agent_name: &str,
    agent: &crate::agent::Agent,
) -> Result<()> {
    // Print welcome
    channel.print_banner();
    channel.print_system(&format!(
        "Agent '{agent_name}' is ready! Type 'exit' or 'quit' to stop."
    ));

    // Print initial prompt
    channel.print_prompt();

    loop {
        // Wait for input
        match channel.receive().await? {
            Some(input) => {
                let trimmed = input.trim();

                // Check for exit commands
                match trimmed.to_lowercase().as_str() {
                    "exit" | "quit" | "bye" => {
                        channel.print_system("Goodbye! 👋");
                        break;
                    }
                    "help" => {
                        channel.print_agent_response("Available commands:\n  help - Show this message\n  exit/quit/bye - Stop the agent");
                        channel.print_prompt();
                    }
                    _ => {
                        // Process with agent's execute method with tools
                        channel.print_system("Thinking...");
                        match agent.execute_with_tools(trimmed).await {
                            Ok(result) => {
                                channel.print_agent_response(&result.final_answer);
                            }
                            Err(e) => {
                                channel.print_error(&format!("Failed to get response: {e}"));
                            }
                        }
                        // Print new prompt after response
                        channel.print_prompt();
                    }
                }
            }
            None => {
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
    println!("⚡ Thinking...");
    match agent.execute_with_tools(message).await {
        Ok(result) => {
            println!("\n🐱 Agent: {}", result.final_answer);
            Ok(result.final_answer)
        }
        Err(e) => {
            eprintln!("\n❌ Error: {e}");
            Err(e)
        }
    }
}

/// Run interactive loop with streaming support
///
/// This version shows real-time progress including tool usage.
pub async fn run_interactive_loop_streaming(
    channel: &mut CliChannel,
    agent_name: &str,
    agent: &crate::agent::Agent,
) -> Result<()> {
    use tokio::task::LocalSet;

    // Print welcome
    channel.print_banner();
    channel.print_system(&format!(
        "Agent '{agent_name}' is ready (streaming mode)! Type 'exit' or 'quit' to stop."
    ));

    // Create a LocalSet for the streaming execution
    let local = LocalSet::new();

    // Print initial prompt
    channel.print_prompt();

    loop {
        // Wait for input
        match channel.receive().await? {
            Some(input) => {
                let trimmed = input.trim();

                // Check for exit commands
                match trimmed.to_lowercase().as_str() {
                    "exit" | "quit" | "bye" => {
                        channel.print_system("Goodbye! 👋");
                        break;
                    }
                    "help" => {
                        channel.print_agent_response("Available commands:\n  help - Show this message\n  exit/quit/bye - Stop the agent");
                        channel.print_prompt();
                    }
                    _ => {
                        // Run the streaming execution in the LocalSet
                        local.run_until(async {
                            match agent.execute_streaming(trimmed).await {
                                Ok(mut event_rx) => {
                                    // Process events as they arrive
                                    while let Some(event) = event_rx.recv().await {
                                        channel.handle_event(&event);

                                        // Check for end/error to break
                                        match &event {
                                            crate::engine::AgenticEvent::Lifecycle { 
                                                phase: crate::engine::LifecyclePhase::End, 
                                                .. 
                                            } => break,
                                            crate::engine::AgenticEvent::Lifecycle { 
                                                phase: crate::engine::LifecyclePhase::Error, 
                                                .. 
                                            } => break,
                                            _ => {}
                                        }
                                    }
                                }
                                Err(e) => {
                                    channel.print_error(&format!("Failed to start streaming: {e}"));
                                }
                            }
                        }).await;

                        // Print new prompt after response
                        channel.print_prompt();
                    }
                }
            }
            None => {
                // No input available, just wait
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cli_channel_name() {
        let channel = CliChannel::new("test");
        assert_eq!(channel.name(), "test");
    }

    #[tokio::test]
    async fn test_cli_channel_send() {
        let mut channel = CliChannel::new("test");
        // Should not panic
        let result = channel.send("Hello").await;
        assert!(result.is_ok());
    }
}
