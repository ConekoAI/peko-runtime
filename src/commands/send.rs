//! Send Command - Unified message sending to agents
//!
//! This command replaces the deprecated `agent start --message` and `session send` commands
//! with a unified, top-level interface for sending messages to agents.
//!
//! Per ADR-013, agents are stateless and cold-start on every request. This command
//! performs a cold-start sequence: load config, load session, instantiate tools,
//! run agentic loop, then exit and free resources.
//!
//! Per ADR-015, this command supports both streaming (default) and blocking modes.
//!
//! Examples:
//!   pekobot send myagent "What is the weather?"
//!   pekobot send myteam/myagent "Hello"
//!   pekobot send myagent --team myteam "Hello"
//!   pekobot send myagent "Hello" --session sess_xxx
//!   pekobot send myagent --new "Start fresh"
//!   pekobot send myagent "Hello" --no-stream        # Wait for full response before output
//!   echo "Hello" | pekobot send myagent --stdin
//!   pekobot send myagent --file prompt.txt

use crate::channels::{Channel, CliChannel, CliMode};
use crate::commands::GlobalPaths;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::services::{AgentConfigService, AgentValidator};
use crate::agent::stateless_service::{MessageRequest, StatelessAgentService};
use anyhow::Result;
use clap::Args;
use std::sync::Arc;
use tracing::info;

/// Send a message to an agent (unified command)
///
/// This is the primary way to interact with agents. Agents cold-start on each
/// request, load their configuration and session history, process the message,
/// then exit and free all resources.
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
pub async fn handle_send(args: SendArgs, paths: &GlobalPaths, _json: bool) -> Result<()> {
    // Resolve the message content
    let message = resolve_message(&args).await?;

    // Parse agent identifier to extract team and agent name
    let (team, agent_name) =
        parse_agent_identifier_with_override(&args.agent, args.team.as_deref())?;

    info!(
        "Sending message to agent '{}' in team '{}'",
        agent_name, team
    );

    // Use StatelessAgentService directly (ADR-016: bypass MessageService)
    let path_resolver = paths.resolver.clone();
    let config_service = Arc::new(AgentConfigService::new(path_resolver.clone()));

    // EARLY VALIDATION: Check agent exists BEFORE creating session infrastructure
    // This prevents creating session directories for non-existent agents
    let validator = AgentValidator::new(config_service.clone());
    let agent_entry = validator.validate_exists(agent_name, Some(team)).await?;
    info!(
        "Validated agent '{}' in team '{}'",
        agent_entry.name, agent_entry.team
    );

    let agent_service = Arc::new(
        StatelessAgentService::new(config_service, path_resolver.clone()).await?,
    );

    // Build message request (same logic as HTTP API)
    let request = MessageRequest::new(agent_name, message)
        .with_team(team)
        .with_session_opt(args.session.clone())
        .with_new_session(args.new);

    // Create CLI channel with appropriate mode (streaming is default)
    let channel = CliChannel::new(&args.agent)
        .with_mode(if args.no_stream {
            CliMode::Blocking
        } else {
            CliMode::Streaming
        });

    // Send message using StatelessAgentService directly (ADR-016)
    let event_stream = agent_service.execute_message_streaming(request).await?;

    // Process events through channel (handles both blocking and streaming)
    let output = channel.process_stream(event_stream).await?;

    // Output response (CLI presentation layer)
    if args.no_stream {
        // In blocking mode, output the collected final text
        println!("{}", output.final_text);
    }
    // In streaming mode (default), the channel already printed tokens as they arrived

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
                anyhow::bail!("Failed to read message file '{}': {}", file_path, e);
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
