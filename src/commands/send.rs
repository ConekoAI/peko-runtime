//! Send Command - Unified message sending to agents
//!
//! This command replaces the deprecated `agent start --message` and `session send` commands
//! with a unified, top-level interface for sending messages to agents.
//!
//! Per ADR-013, agents are stateless and cold-start on every request.
//! Per ADR-021 Phase 2, the CLI is a thin client that forwards requests to the daemon
//! over HTTP. All execution happens in the daemon.
//!
//! Examples:
//!   pekobot send myagent "What is the weather?"
//!   pekobot send myteam/myagent "Hello"
//!   pekobot send myagent --team myteam "Hello"
//!   pekobot send myagent "Hello" --session `sess_xxx`
//!   pekobot send myagent --new "Start fresh"
//!   pekobot send myagent "Hello" --no-stream        # Wait for full response before output
//!   echo "Hello" | pekobot send myagent --stdin
//!   pekobot send myagent --file prompt.txt

use crate::api::client::{ApiClient, ChatSseEvent, ClientError};
use crate::commands::GlobalPaths;
use crate::common::identifiers::parse_agent_identifier_with_override;
use anyhow::Result;
use clap::Args;
use futures::StreamExt;
use tracing::info;

/// Send a message to an agent (unified command)
///
/// This is the primary way to interact with agents. The CLI forwards the
/// request to the daemon over HTTP. The daemon cold-starts the agent,
/// processes the message, and streams back the response.
///
/// The message can be provided as an argument, from a file, or via stdin.
/// Agent can be specified as just "name" (uses default team) or "team/name".
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

    // Create API client (connects to daemon)
    let client = ApiClient::new()
        .map_err(|e| anyhow::anyhow!("Failed to create API client: {e}"))?;

    // Check daemon is running
    if let Err(ClientError::DaemonNotRunning { addr }) = client.health_check().await {
        anyhow::bail!(
            "Daemon is not running at {addr}. Start it with: pekobot daemon start"
        );
    }

    if args.no_stream {
        // Non-streaming: use the execute endpoint (blocking)
        // For now, fall back to streaming but collect output
        // TODO: Add dedicated blocking endpoint when available
        let mut output = String::new();
        let mut stream = client
            .chat_stream(&agent_name, &message, args.session.as_deref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start chat stream: {e}"))?;

        while let Some(event) = stream.next().await {
            match event {
                Ok(ChatSseEvent::Delta { text }) => {
                    output.push_str(&text);
                }
                Ok(ChatSseEvent::Done { .. }) => break,
                Ok(ChatSseEvent::Error { message, .. }) => {
                    anyhow::bail!("Agent execution error: {message}");
                }
                Ok(_) => {} // Ignore other events in blocking mode
                Err(e) => {
                    anyhow::bail!("Stream error: {e}");
                }
            }
        }

        println!("{output}");
    } else {
        // Streaming mode: connect to SSE endpoint and print events as they arrive
        let mut stream = client
            .chat_stream(&agent_name, &message, args.session.as_deref())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start chat stream: {e}"))?;

        while let Some(event) = stream.next().await {
            match event {
                Ok(ChatSseEvent::Delta { text }) => {
                    print!("{text}");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
                Ok(ChatSseEvent::ToolCall { tool, .. }) => {
                    eprintln!("\n🔧 Calling tool: {tool}");
                }
                Ok(ChatSseEvent::ToolResult { output, error, .. }) => {
                    if let Some(err) = error {
                        eprintln!("\n❌ Tool error: {err}");
                    } else {
                        eprintln!("\n✅ Tool result: {output}");
                    }
                }
                Ok(ChatSseEvent::Thinking { text }) => {
                    eprintln!("\n💭 {text}");
                }
                Ok(ChatSseEvent::Done { .. }) => {
                    println!();
                    break;
                }
                Ok(ChatSseEvent::Error { message, .. }) => {
                    anyhow::bail!("\n❌ Agent execution error: {message}");
                }
                Err(e) => {
                    anyhow::bail!("Stream error: {e}");
                }
            }
        }
    }

    Ok(())
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
             pekobot send myagent \"Hello\"\n  \
             pekobot send myagent --file prompt.txt\n  \
             echo \"Hello\" | pekobot send myagent --stdin"
        );
    }
}

#[cfg(test)]
mod tests {}
