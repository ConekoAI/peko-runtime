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
use crate::common::types::OutputFormat;
use crate::ipc::packet::PrincipalSendControlMode;
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

    /// Do not treat `/`-prefixed messages as slash commands; pass them to the LLM verbatim
    #[arg(long)]
    pub no_slash: bool,
}

/// Handle the send command
pub async fn handle_send(args: SendArgs, _paths: &GlobalPaths, _json: bool) -> Result<()> {
    let message = resolve_message(&args).await?;

    info!("Sending message to principal '{}'", args.principal);

    let client = DaemonClient::connect().await?;
    let output_format = if _json {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    };
    // Always use the streaming request. It emits `PrincipalSentChunk`
    // deltas as the root agent produces text, which (a) lets us print
    // incrementally and (b) keeps the per-packet idle timeout
    // (`CLI_TIMEOUT_SECS`) from firing on long responses — the one-shot
    // `PrincipalSend` path emits nothing until completion, so any answer
    // taking longer than the idle window dies with "Stream closed
    // unexpectedly". `--no-stream` still buffers before printing.
    let stream = client
        .principal_send_stream(
            &args.principal,
            message,
            _paths.user(),
            args.no_slash,
            output_format,
        )
        .await?;

    // Print the request_id to stderr before reading the stream so
    // users can run `peko interrupt <id>` from another terminal to
    // soft-interrupt the run. The daemon also accepts Ctrl-C via the
    // `tokio::select!` below, but a separate terminal is the supported
    // headless path.
    let request_id = stream.request_id();
    eprintln!("[peko] request_id={request_id} (run `peko interrupt {request_id}` to stop)");

    process_response_stream(stream, &client, &args, _json).await
}

/// Process the response stream from a `PrincipalSend` request.
///
/// In streaming mode, races the response loop against `tokio::signal::ctrl_c()`
/// so the user can interrupt a long-running stream from the same
/// terminal. On Ctrl-C, sends a `PrincipalSendControl { Interrupt }` to
/// the daemon and returns once the daemon's own `Done`/`Error`
/// closes the stream naturally (the loop will fall through with the
/// "interrupted" error message).
async fn process_response_stream(
    mut stream: crate::ipc::PacketStream,
    client: &DaemonClient,
    args: &SendArgs,
    _json: bool,
) -> Result<()> {
    // Spawn a side-channel task that watches for Ctrl-C and signals
    // the main loop. We can't await `ctrl_c()` directly inside the
    // `tokio::select!` because the future is one-shot and would be
    // consumed after the first iteration; using `Notify` lets us
    // re-arm each iteration.
    let ctrl_c_signal = std::sync::Arc::new(tokio::sync::Notify::new());
    {
        let signal = std::sync::Arc::clone(&ctrl_c_signal);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal.notify_waiters();
            }
        });
    }
    let mut interrupt_sent = false;
    let request_id = stream.request_id();
    if args.no_stream {
        let mut final_text = String::new();
        while let Some(packet) = next_or_interrupt(
            &mut stream,
            &ctrl_c_signal,
            &mut interrupt_sent,
            client,
            request_id,
        )
        .await?
        {
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
                ResponsePacket::Heartbeat { .. } => {}
                _ => {}
            }
        }
        anyhow::bail!("Stream closed unexpectedly");
    }

    let mut has_started_line = false;
    while let Some(packet) = next_or_interrupt(
        &mut stream,
        &ctrl_c_signal,
        &mut interrupt_sent,
        client,
        request_id,
    )
    .await?
    {
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
            ResponsePacket::Heartbeat { .. } => {}
            _ => {}
        }
    }
    anyhow::bail!("Stream closed unexpectedly");
}

/// Race `stream.next()` against a Ctrl-C signal. On the first Ctrl-C,
/// sends a `PrincipalSendControl { Interrupt }` to the daemon and
/// continues reading the stream (the daemon will eventually emit its
/// own `Done` with `error: Some("interrupted")` and close it). Returns
/// the next packet or `None` when the stream is fully closed.
async fn next_or_interrupt(
    stream: &mut crate::ipc::PacketStream,
    ctrl_c_signal: &std::sync::Arc<tokio::sync::Notify>,
    interrupt_sent: &mut bool,
    client: &DaemonClient,
    request_id: u64,
) -> Result<Option<ResponsePacket>> {
    // Outer loop so a Ctrl-C that races with a packet still falls back
    // to the next stream.next() call to pick up the daemon's final
    // `Done`. The `if !*interrupt_sent` guard ensures we only send the
    // interrupt once even if the user mashes Ctrl-C.
    loop {
        let notified = ctrl_c_signal.notified();
        tokio::pin!(notified);
        tokio::select! {
            biased;
            packet = stream.next() => return Ok(packet),
            () = &mut notified, if !*interrupt_sent => {
                *interrupt_sent = true;
                eprintln!("\n[peko] Ctrl-C received — sending interrupt to daemon...");
                if let Err(e) = client
                    .principal_send_control(request_id, PrincipalSendControlMode::Interrupt)
                    .await
                {
                    eprintln!("[peko] failed to send interrupt to daemon: {e}");
                }
                // Loop back and let the stream's next packet be the
                // daemon's `Done { success: false, error: "interrupted" }`.
            }
        }
    }
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
        let cli = Cli::try_parse_from(["peko", "send", "myprincipal", "hello"])
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
    fn send_parses_no_slash_flag() {
        let cli = Cli::try_parse_from(["peko", "send", "myprincipal", "--no-slash", "/help"])
            .expect("should parse send command with --no-slash");

        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.principal, "myprincipal");
                assert!(args.no_slash);
                assert_eq!(args.message, Some("/help".to_string()));
            }
            _other => panic!("expected Send command"),
        }
    }
}
