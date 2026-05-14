//! Send Command - Unified message sending to agents
//!
//! This command replaces the deprecated `agent start --message` and `session send` commands
//! with a unified, top-level interface for sending messages to agents.
//!
//! Per ADR-021, the CLI is now a thin client. All execution happens in the daemon.
//! This command sends an IPC request and streams the response.
//!
//! Examples:
//!   peko send myagent "What is the weather?"
//!   peko send myteam/myagent "Hello"
//!   peko send myagent --team myteam "Hello"
//!   peko send myagent "Hello" --session `sess_xxx`
//!   peko send myagent --new "Start fresh"
//!   peko send myagent "Hello" --no-stream        # Wait for full response before output
//!   echo "Hello" | peko send myagent --stdin
//!   peko send myagent --file prompt.txt

use crate::commands::GlobalPaths;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::ipc::{DaemonClient, ResponsePacket};
use anyhow::Result;
use clap::Args;
use std::io::Write;
use tracing::info;

/// Send a message to an agent (unified command)
///
/// This is the primary way to interact with agents. The daemon handles all
/// execution; the CLI just forwards the request and prints the response.
#[derive(Args, Clone, Debug)]
#[command(disable_version_flag = true)]
pub struct SendArgs {
    /// Agent name or team/agent format (e.g., "myagent" or "myteam/myagent")
    pub agent: String,

    /// Message to send (optional if --file or --stdin is used)
    pub message: Option<String>,

    /// Team to look in (overrides team/ prefix if both provided)
    #[arg(short, long)]
    pub team: Option<String>,

    /// Specific session ID to use (optional, uses active session if not provided)
    #[arg(short, long, value_name = "SESSION")]
    pub session: Option<String>,

    /// Start a new session (don't resume existing)
    #[arg(short, long)]
    pub new: bool,

    /// Read message from file
    #[arg(short, long, value_name = "PATH", conflicts_with = "stdin")]
    pub file: Option<String>,

    /// Read message from stdin
    #[arg(long, conflicts_with = "file")]
    pub stdin: bool,

    /// Disable streaming, wait for full response before output
    #[arg(long)]
    pub no_stream: bool,
}

/// Handle the send command
pub async fn handle_send(args: SendArgs, _paths: &GlobalPaths, _json: bool) -> Result<()> {
    // Resolve the message content
    let message = resolve_message(&args).await?;

    // Parse agent identifier to extract team and agent name
    let (team, agent_name) =
        parse_agent_identifier_with_override(&args.agent, args.team.as_deref())?;

    info!(
        "Sending message to agent '{}' in team '{}'",
        agent_name, team
    );

    // Connect to daemon (fails if not running — start it with: peko daemon start)
    let client = DaemonClient::connect().await?;

    // Send execute request to daemon
    let mut stream = client
        .execute(
            agent_name,
            team,
            message,
            args.session.clone(),
            args.new,
            !args.no_stream,
            _paths.user(),
        )
        .await?;

    // Process response stream
    if args.no_stream {
        // Blocking mode: collect all text and print at the end
        let mut final_text = String::new();
        while let Some(packet) = stream.next().await {
            match packet {
                ResponsePacket::Text { chunk, .. } => {
                    final_text.push_str(&chunk);
                }
                ResponsePacket::Done { success, error, .. } => {
                    if success {
                        println!("{}", final_text.trim());
                        return Ok(());
                    }
                    anyhow::bail!(
                        "Agent execution failed{}",
                        error.map(|e| format!(": {e}")).unwrap_or_default()
                    );
                }
                ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("Agent execution failed: {message}");
                }
                ResponsePacket::Heartbeat { .. } => {
                    // Ignore heartbeats in blocking mode
                }
                _ => {}
            }
        }
        anyhow::bail!("Stream closed unexpectedly");
    }
    // Streaming mode: print chunks as they arrive
    let mut has_started_line = false;
    while let Some(packet) = stream.next().await {
        match packet {
            ResponsePacket::Text { chunk, .. } => {
                if !has_started_line {
                    print!("\n{}: ", args.agent);
                    std::io::stdout().flush()?;
                    has_started_line = true;
                }
                print!("{}", chunk);
                std::io::stdout().flush()?;
            }
            ResponsePacket::Done { success, error, .. } => {
                if has_started_line {
                    println!();
                }
                if success {
                    return Ok(());
                }
                anyhow::bail!(
                    "Agent execution failed{}",
                    error.map(|e| format!(": {e}")).unwrap_or_default()
                );
            }
            ResponsePacket::Error { message, .. } => {
                anyhow::bail!("Agent execution failed: {message}");
            }
            ResponsePacket::Heartbeat { .. } => {
                // Ignore heartbeats — they just keep the connection alive
            }
            _ => {}
        }
    }
    anyhow::bail!("Stream closed unexpectedly");
}

/// Resolve message from various sources (argument, file, or stdin)
async fn resolve_message(args: &SendArgs) -> Result<String> {
    // Priority: --stdin > --file > positional argument
    if args.stdin {
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer)?;
        Ok(buffer.trim().to_string())
    } else if let Some(ref file_path) = args.file {
        match std::fs::read_to_string(file_path) {
            Ok(content) => Ok(content),
            Err(e) => {
                anyhow::bail!("Failed to read message file '{file_path}': {e}");
            }
        }
    } else if let Some(ref message) = args.message {
        Ok(message.clone())
    } else {
        anyhow::bail!(
            "Message is required. Provide it as an argument, use --file, or --stdin.\n\
             Examples:\n  \
             peko send myagent \"Hello\"\n  \
             peko send myagent --file prompt.txt\n  \
             echo \"Hello\" | peko send myagent --stdin"
        )
    }
}

#[cfg(test)]
mod tests {}
