//! Send Command - Send a message to a Principal
//!
//! The top-level `peko send` command targets a Principal by name and
//! streams the response from the daemon.
//!
//! Examples:
//!   peko send myprincipal "What is the weather?"
//!   peko send myprincipal --file prompt.txt
//!   echo "Hello" | peko send myprincipal --stdin
//!   peko send myprincipal "Hello" --no-stream

use crate::commands::GlobalPaths;
use crate::ipc::{DaemonClient, ResponsePacket};
use anyhow::Result;
use clap::Args;
use std::io::Write;
use tracing::info;

/// Send a message to a Principal
#[derive(Args, Clone, Debug)]
#[command(disable_version_flag = true)]
pub struct SendArgs {
    /// Principal name
    pub principal: String,

    /// Message to send (optional if --file or --stdin is used)
    pub message: Option<String>,

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
    let message = resolve_message(&args).await?;

    info!("Sending message to principal '{}'", args.principal);

    let client = DaemonClient::connect().await?;
    // Always use the streaming request. It emits `PrincipalSentChunk`
    // deltas as the supervisor produces text, which (a) lets us print
    // incrementally and (b) keeps the per-packet idle timeout
    // (`CLI_TIMEOUT_SECS`) from firing on long responses — the one-shot
    // `PrincipalSend` path emits nothing until completion, so any answer
    // taking longer than the idle window dies with "Stream closed
    // unexpectedly". `--no-stream` still buffers before printing.
    let stream = client
        .principal_send_stream(&args.principal, message, _paths.user())
        .await?;

    process_response_stream(stream, &args, _json).await
}

/// Process the response stream from a `PrincipalSend` request.
async fn process_response_stream(
    mut stream: crate::ipc::PacketStream,
    args: &SendArgs,
    json: bool,
) -> Result<()> {
    if args.no_stream {
        let mut final_text = String::new();
        while let Some(packet) = stream.next().await {
            match packet {
                ResponsePacket::PrincipalSentChunk { delta, .. } => {
                    final_text.push_str(&delta);
                }
                // Authoritative full answer — prefer it over the
                // accumulated deltas in case the orchestrator buffered.
                ResponsePacket::PrincipalSentDone { content, .. } => {
                    final_text = content;
                }
                ResponsePacket::PrincipalSent { content, .. }
                | ResponsePacket::Text { chunk: content, .. } => {
                    final_text.push_str(&content);
                }
                ResponsePacket::Done { success, error, .. } => {
                    if success {
                        println!("{}", final_text.trim());
                        return Ok(());
                    }
                    anyhow::bail!(
                        "Principal execution failed{}",
                        error.map(|e| format!(": {e}")).unwrap_or_default()
                    );
                }
                ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("Principal execution failed: {message}");
                }
                ResponsePacket::MessageQueued { run_triggered, .. } => {
                    if !run_triggered && json {
                        println!("{{\"status\":\"queued\",\"run_triggered\":false}}");
                    }
                }
                ResponsePacket::Heartbeat { .. } => {}
                _ => {}
            }
        }
        anyhow::bail!("Stream closed unexpectedly");
    }

    let mut has_started_line = false;
    while let Some(packet) = stream.next().await {
        match packet {
            ResponsePacket::PrincipalSentChunk { delta: content, .. }
            | ResponsePacket::PrincipalSent { content, .. }
            | ResponsePacket::Text { chunk: content, .. } => {
                if !has_started_line {
                    print!("\n{}: ", args.principal);
                    std::io::stdout().flush()?;
                    has_started_line = true;
                }
                print!("{}", content);
                std::io::stdout().flush()?;
            }
            // Final full answer. Chunks were already printed
            // incrementally; only fall back to printing it here if the
            // stream produced no deltas (e.g. a buffered orchestrator).
            ResponsePacket::PrincipalSentDone { content, .. } => {
                if !has_started_line && !content.is_empty() {
                    print!("\n{}: {}", args.principal, content);
                    std::io::stdout().flush()?;
                    has_started_line = true;
                }
            }
            ResponsePacket::Done { success, error, .. } => {
                if has_started_line {
                    println!();
                }
                if success {
                    return Ok(());
                }
                anyhow::bail!(
                    "Principal execution failed{}",
                    error.map(|e| format!(": {e}")).unwrap_or_default()
                );
            }
            ResponsePacket::Error { message, .. } => {
                anyhow::bail!("Principal execution failed: {message}");
            }
            ResponsePacket::MessageQueued { run_triggered, .. } => {
                if !run_triggered {
                    eprintln!("(queued — will run when session is idle)");
                }
            }
            ResponsePacket::Heartbeat { .. } => {}
            _ => {}
        }
    }
    anyhow::bail!("Stream closed unexpectedly");
}

/// Resolve message from various sources (argument, file, or stdin)
async fn resolve_message(args: &SendArgs) -> Result<String> {
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
             peko send myprincipal \"Hello\"\n  \
             peko send myprincipal --file prompt.txt\n  \
             echo \"Hello\" | peko send myprincipal --stdin"
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn send_parses_principal_and_message() {
        let cli = Cli::try_parse_from([
            "peko",
            "send",
            "myprincipal",
            "hello",
        ])
        .expect("should parse send command");

        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.principal, "myprincipal");
                assert_eq!(args.message, Some("hello".to_string()));
                assert!(!args.no_stream);
            }
            _other => panic!("expected Send command"),
        }
    }

    #[test]
    fn send_parses_file_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "send",
            "myprincipal",
            "--file",
            "prompt.txt",
        ])
        .expect("should parse send command with file");

        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.principal, "myprincipal");
                assert_eq!(args.file, Some("prompt.txt".to_string()));
                assert!(args.message.is_none());
            }
            _other => panic!("expected Send command"),
        }
    }
}
