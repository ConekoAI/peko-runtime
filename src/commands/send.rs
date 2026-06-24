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

    // Routing (issue: generalize the per-session inbox to also carry
    // user steering messages):
    //
    // 1. `--new` is explicit → fresh session, use `Execute`.
    // 2. `--session X` → user knows the session; route to `SessionSteer`
    //    so a queued message either joins an in-flight run (next
    //    iteration) or auto-triggers a new one if idle. Same UX as
    //    `peko send` against the active session, but explicit.
    // 3. No session specified → fall back to `Execute`. The daemon
    //    resolves the active session, creates a new one if none,
    //    and runs as before. This preserves the "no flags → just
    //    send a message" path for first-time users.
    let stream = if args.new {
        // Explicit new-session: must go through Execute; SessionSteer
        // expects an existing session id.
        client
            .execute(
                agent_name,
                team,
                message.clone(),
                None,
                true,
                !args.no_stream,
                _paths.user(),
            )
            .await?
    } else if let Some(session_id) = args.session.clone() {
        client.steer_session(session_id, message.clone()).await?
    } else {
        // Default path: send as a fresh execute. The daemon resolves
        // the active session or creates one.
        client
            .execute(
                agent_name,
                team,
                message.clone(),
                None,
                false,
                !args.no_stream,
                _paths.user(),
            )
            .await?
    };

    process_response_stream(stream, &args, _json).await
}

/// Process the response stream from either an `Execute` or a
/// `SessionSteer` request. Both use the same wire: the first packet
/// may be a `MessageQueued` confirmation (steering only) followed by
/// `Text` chunks and a terminal `Done` or `Error`.
async fn process_response_stream(
    mut stream: crate::ipc::PacketStream,
    args: &SendArgs,
    json: bool,
) -> Result<()> {
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
                ResponsePacket::MessageQueued {
                    run_triggered, ..
                } => {
                    // SessionSteer confirmation. The actual response
                    // (or `Done`) follows on the same stream.
                    if !run_triggered && json {
                        println!(
                            "{{\"status\":\"queued\",\"run_triggered\":false}}"
                        );
                    }
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
            ResponsePacket::MessageQueued {
                run_triggered, ..
            } => {
                // Briefly acknowledge the queued-and-triggered state
                // before the run's text chunks arrive on the same
                // stream.
                if !run_triggered {
                    eprintln!("(queued — will run when session is idle)");
                }
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
