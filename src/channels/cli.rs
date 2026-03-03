//! CLI channel - Interactive terminal interface

use super::{Channel, StreamingConfig};
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

    /// Handle an agentic event (single event display)
    /// 
    /// Note: For streaming, use handle_stream instead which includes chunking.
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
                // Deltas are handled by the chunker in handle_stream
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

    fn streaming_config(&self) -> StreamingConfig {
        self.streaming_config.clone()
    }

    async fn handle_stream(
        &mut self,
        mut event_rx: mpsc::Receiver<crate::engine::AgenticEvent>,
    ) -> Result<()> {
        use crate::engine::AgenticEvent;
        
        let config = self.streaming_config.clone();
        let mut coalesce_buffer = String::new();

        while let Some(event) = event_rx.recv().await {
            match event {
                AgenticEvent::Lifecycle { phase, error, .. } => {
                    match phase {
                        crate::engine::LifecyclePhase::Start => {
                            // Silent - banner already shown
                        }
                        crate::engine::LifecyclePhase::Running => {
                            // Start inline streaming - print prefix once
                            if config.show_status {
                                print!("\n🤔 ");
                                std::io::stdout().flush().unwrap();
                            }
                        }
                        crate::engine::LifecyclePhase::End => {
                            // Flush any remaining coalesced content
                            if config.coalesce && !coalesce_buffer.is_empty() {
                                println!(); // Newline before final response
                                self.print_agent_response(&coalesce_buffer);
                                coalesce_buffer.clear();
                            }
                            break;
                        }
                        crate::engine::LifecyclePhase::Error => {
                            if let Some(err) = error {
                                self.print_error(&err);
                            } else {
                                self.print_error("Agent encountered an error");
                            }
                            break;
                        }
                        crate::engine::LifecyclePhase::Aborted => {
                            self.print_system("Agent aborted");
                            break;
                        }
                    }
                }
                AgenticEvent::Assistant { text, is_delta, is_final, .. } => {
                    if is_final {
                        // Final response - print with agent prefix
                        if !coalesce_buffer.is_empty() {
                            println!(); // Newline after reasoning
                            self.print_agent_response(&coalesce_buffer);
                            coalesce_buffer.clear();
                        }
                        if !text.is_empty() {
                            println!();
                            self.print_agent_response(&text);
                        }
                    } else if is_delta {
                        // Stream reasoning inline immediately (no chunking/delay)
                        print!("{}", text);
                        std::io::stdout().flush().unwrap();
                    }
                }
                AgenticEvent::ToolStart { name, .. } => {
                    if config.show_tools {
                        println!(); // Newline after reasoning stream
                        self.print_tool_start(&name);
                    }
                }
                AgenticEvent::ToolUpdate { tool_id, output, progress_percent, .. } => {
                    if config.show_tools {
                        if let Some(percent) = progress_percent {
                            self.print_system(&format!("🔧 {}: {}% - {}", tool_id, percent, output));
                        } else {
                            self.print_system(&format!("🔧 {}: {}", tool_id, output));
                        }
                    }
                }
                AgenticEvent::ToolEnd { tool_id, success, .. } => {
                    if config.show_tools {
                        self.print_tool_result(&tool_id, success);
                    }
                }
                AgenticEvent::Status { message, .. } => {
                    if config.show_status {
                        self.print_system(&message);
                    }
                }
                _ => {
                    // Other events not displayed
                }
            }
        }

        // Final flush of any remaining content
        if config.coalesce && !coalesce_buffer.is_empty() {
            self.print_agent_response(&coalesce_buffer);
        }

        Ok(())
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
///
/// Uses streaming to show real-time reasoning and tool usage.
pub async fn send_single_message(
    agent: &crate::agent::Agent,
    message: &str,
) -> Result<String> {
    use crate::engine::AgenticEvent;
    use tokio::task::LocalSet;

    // Create a LocalSet for the streaming execution (required for non-Send types)
    let local = LocalSet::new();

    let result = local
        .run_until(async {
            // Start streaming
            let mut event_rx = agent.execute_streaming(message).await?;

            let mut final_answer = String::new();
            let mut reasoning_started = false;

            // Process events as they arrive
            while let Some(event) = event_rx.recv().await {
                match event {
                    AgenticEvent::Lifecycle { phase, .. } => match phase {
                        crate::engine::LifecyclePhase::Running => {
                            // Only print reasoning indicator once
                            if !reasoning_started {
                                print!("\n🤔 ");
                                std::io::stdout().flush().unwrap();
                                reasoning_started = true;
                            }
                        }
                        crate::engine::LifecyclePhase::End => {
                            // End of execution - flush any pending reasoning
                            if reasoning_started {
                                println!();
                            }
                            break;
                        }
                        crate::engine::LifecyclePhase::Error => {
                            return Err(anyhow::anyhow!("Agent encountered an error"));
                        }
                        _ => {}
                    },
                    AgenticEvent::Assistant {
                        text,
                        is_delta,
                        is_final,
                        ..
                    } => {
                        if is_final {
                            // Final response - print with agent prefix
                            if !text.is_empty() {
                                println!("\n🐱 Agent: {}", text);
                                final_answer = text;
                            }
                        } else if is_delta && reasoning_started {
                            // Stream reasoning tokens inline
                            print!("{}", text);
                            std::io::stdout().flush().unwrap();
                        }
                    }
                    AgenticEvent::ToolStart { name, .. } => {
                        // End reasoning stream before showing tool
                        if reasoning_started {
                            println!();
                            reasoning_started = false;
                        }
                        println!("\n🔧 Using tool: {}", name);
                    }
                    AgenticEvent::ToolEnd {
                        tool_id, success, ..
                    } => {
                        let icon = if success { "✅" } else { "❌" };
                        println!("{} Tool '{}' completed", icon, tool_id);
                    }
                    AgenticEvent::ToolUpdate {
                        tool_id,
                        output,
                        progress_percent,
                        ..
                    } => {
                        if let Some(percent) = progress_percent {
                            println!("  📊 {}: {}% - {}", tool_id, percent, output);
                        } else {
                            println!("  📊 {}: {}", tool_id, output);
                        }
                    }
                    _ => {}
                }
            }

            Ok(final_answer)
        })
        .await;

    result
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
                                Ok(event_rx) => {
                                    // Use channel's handle_stream for proper chunking
                                    if let Err(e) = channel.handle_stream(event_rx).await {
                                        channel.print_error(&format!("Streaming error: {e}"));
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
