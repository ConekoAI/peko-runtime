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

/// Interactive conversation loop for CLI
pub async fn run_interactive_loop<C: Channel + 'static>(
    channel: &mut C,
    agent_name: &str,
) -> Result<()> {
    use crate::channels::cli::CliChannel;

    // Print welcome
    if let Some(cli) = (channel as &dyn std::any::Any).downcast_ref::<CliChannel>() {
        cli.print_banner();
        cli.print_system(&format!(
            "Agent '{agent_name}' is ready! Type 'exit' or 'quit' to stop."
        ));
    }

    loop {
        // Print prompt
        if let Some(cli) = (channel as &dyn std::any::Any).downcast_ref::<CliChannel>() {
            cli.print_prompt();
        }

        // Wait for input
        match channel.receive().await? {
            Some(input) => {
                let trimmed = input.trim();

                // Check for exit commands
                match trimmed.to_lowercase().as_str() {
                    "exit" | "quit" | "bye" => {
                        if let Some(cli) =
                            (channel as &dyn std::any::Any).downcast_ref::<CliChannel>()
                        {
                            cli.print_system("Goodbye! 👋");
                        }
                        break;
                    }
                    "help" => {
                        if let Some(cli) =
                            (channel as &dyn std::any::Any).downcast_ref::<CliChannel>()
                        {
                            cli.print_agent_response("Available commands:\n  help - Show this message\n  exit/quit/bye - Stop the agent");
                        }
                    }
                    _ => {
                        // Echo back for now (would be processed by agent)
                        let response =
                            format!("Received: '{trimmed}' (agent processing not yet implemented)");
                        channel.send(&response).await?;
                    }
                }
            }
            None => {
                // No input available, continue loop
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
