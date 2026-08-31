//! Send Command - Send a message to a Principal
//!
//! The top-level `peko send` command posts a message onto the caller's
//! thread with a Principal. If no run is in flight the daemon starts
//! one and the reply streams back; if a run is already in flight the
//! message is queued onto the session inbox and folds into the running
//! turn at the next agentic step ("busy" path — the CLI prints a
//! notice and exits 0, or blocks for the reply with `--wait`).
//!
//! Examples:
//!   peko send myprincipal "What is the weather?"
//!   peko send myprincipal --file prompt.txt
//!   echo "Hello" | peko send myprincipal --stdin
//!   peko send myprincipal "also check the calendar" --wait
//!   peko send myprincipal "Hello" --model openai-gpt-4o
//!
//! Group channels (`group:<slug>` recipients) post as the caller's
//! user identity (ADR-049 Phase 2, D7): `peko send group:eng "hi"`
//! writes to the group channel's log as `user:<id>`; store-level
//! Subject membership is the write authorization. `--wait` and `--model`
//! stay refused — a group post fans out to one run per member
//! principal, so there is no single run to await or steer.

use crate::commands::{parse_recipient, GlobalPaths, Recipient};
use anyhow::{Context, Result};
use clap::Args;
use peko_channel::{ChannelCliRouter, ChannelConfig, ChannelStore};
use peko_core::ipc::packet::RequestPacket;
use peko_core::ipc::{DaemonClient, ResponsePacket};
use peko_protocol::channel::ChannelId;
use std::io::Write;
use std::str::FromStr;
use std::sync::Arc;
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

    /// When the principal is busy (message queued onto the in-flight
    /// run), block until the principal's next reply on the thread
    /// instead of exiting right after the queued notice
    #[arg(long)]
    pub wait: bool,

    /// Send as this peer instead of the global `-U/--user` identity.
    /// Accepts the wire format `user:<id>`.
    #[arg(long, value_name = "SUBJECT")]
    pub peer: Option<String>,

    /// Override the configured model for this message only
    #[arg(long, value_name = "MODEL_ID")]
    pub model: Option<String>,
}

/// Handle the send command
pub async fn handle_send(args: SendArgs, paths: &GlobalPaths, _json: bool) -> Result<()> {
    let message = resolve_message(&args).await?;

    // Refuse empty messages at the CLI layer — see Bug 4 in
    // scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md.
    // Without this guard, `peko send scout ""` still calls
    // the LLM with empty content (the 128 input tokens are the system
    // prompt) and returns a canned greeting, silently burning the
    // user's quota. A non-technical user typing this in a shell loop
    // would have no idea they were paying for empty turns.
    if message.trim().is_empty() {
        anyhow::bail!(
            "Message is empty. Provide a non-empty message as an argument, via --file, or via --stdin.\n\
             Examples:\n  \
             peko send myprincipal \"Hello\"\n  \
             peko send myprincipal --file prompt.txt\n  \
             echo \"Hello\" | peko send myprincipal --stdin"
        );
    }

    // The sender's user identity: `--peer user:<id>` overrides the
    // global `-U/--user` derivation. The daemon wraps the packet's
    // `user` string as `Subject::User(user)`, so only the user form is
    // supported here. Derived before the group branch so both the
    // principal and group paths share the same identity rule.
    let user = match args.peer.as_deref() {
        Some(raw) => match peko_auth::Subject::from_str(raw) {
            Ok(peko_auth::Subject::User(id)) => id,
            Ok(other) => {
                anyhow::bail!("--peer must be a user subject (`user:<id>`), got '{other}'")
            }
            Err(e) => anyhow::bail!("invalid --peer value '{raw}': {e}"),
        },
        None => paths.user().to_string(),
    };
    let peer = peko_auth::Subject::User(user.clone());

    // Group recipients (`group:<slug>`): post to the group channel as
    // the caller's user identity (ADR-049 Phase 2, D7). Membership is
    // the write authorization — a non-member user is refused by the
    // store with `NotMember`. `--wait` / `--model` stay refused: a
    // group post fans out to one run per member principal, so there
    // is no single run to await or steer.
    if let Recipient::Group(slug) = parse_recipient(&args.principal) {
        if args.wait {
            anyhow::bail!("groups have no bound agent run; nothing to wait on");
        }
        if args.model.is_some() {
            anyhow::bail!("--model is meaningless for group channels (no bound agent run)");
        }
        let channel = format!("group:{slug}");
        return post_to_group(paths, &channel, &message, &user).await;
    }

    info!("Sending message to principal '{}'", args.principal);

    let client = DaemonClient::connect().await?;
    // Always use the streaming request. It emits `PrincipalSentChunk`
    // deltas as the root agent produces text, which (a) lets us print
    // incrementally and (b) keeps the per-packet idle timeout
    // (`CLI_TIMEOUT_SECS`) from firing on long responses — the one-shot
    // `PrincipalSend` path emits nothing until completion, so any answer
    // taking longer than the idle window dies with "Stream closed
    // unexpectedly".
    //
    // `sent_at` is the client-side timestamp just before ingress; the
    // busy-path `--wait` loop uses it to tell the principal's reply
    // apart from older thread history.
    let sent_at = chrono::Utc::now();
    let stream = client
        .principal_send_stream(
            &args.principal,
            message,
            user,
            args.model.clone(),
        )
        .await?;

    process_response_stream(stream, &client, &args, &peer, sent_at, _json).await
}

/// `peko send group:<slug>` (ADR-049 Phase 2, D7): post `message` to
/// the group channel as `user:<user>`. Daemon-first via `ChannelPost`
/// (the daemon IPC accepts `user:<id>` senders and the store's
/// Subject membership authorizes the write); falls back to an
/// in-process `ChannelStore` when the daemon is unreachable, mirroring
/// the `peko channel` dual-path so manual smoke tests work without a
/// live daemon.
async fn post_to_group(
    paths: &GlobalPaths,
    channel: &str,
    message: &str,
    user: &str,
) -> Result<()> {
    let packet = RequestPacket::ChannelPost {
        request_id: 0,
        channel: channel.to_string(),
        sender_name: format!("user:{user}"),
        text: message.to_string(),
        parent: None,
    };
    if let Ok(client) = DaemonClient::connect().await {
        if let Ok(resp) = client.request_response(packet).await {
            match resp {
                ResponsePacket::ChannelPosted { task_id, .. } => {
                    println!("posted → {task_id}");
                    return Ok(());
                }
                // A daemon-side application error (e.g. NotMember) is
                // authoritative — the fallback reads the same store
                // and would only repeat it.
                ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("group post failed: {message}");
                }
                // Unexpected shape — fall through to the in-process path.
                _ => {}
            }
        }
    }

    let ch = ChannelId::parse(channel).with_context(|| format!("invalid ChannelId: {channel}"))?;
    let port: Arc<dyn peko_channel::ChannelPort> = Arc::new(ChannelStore::new(ChannelConfig {
        runtime_dir: paths.runtime_dir(),
        shared_dir: None,
    }));
    let router = ChannelCliRouter::new(port);
    let resp = router
        .handle_post(&ch, &peko_auth::Subject::User(user.to_string()), message, None)
        .await?;
    println!("posted → {}", resp.task_id);
    Ok(())
}

/// Process the response stream from a `PrincipalSend` request.
///
/// Races the response loop against `tokio::signal::ctrl_c()`
/// so the user can stop a long-running stream from the same
/// terminal. On Ctrl-C, sends a `PrincipalStop` to the daemon and
/// returns once the daemon's own `Done`/`Error`
/// closes the stream naturally (the loop will fall through with the
/// "stopped by user" error message).
async fn process_response_stream(
    mut stream: peko_core::ipc::PacketStream,
    client: &DaemonClient,
    args: &SendArgs,
    peer: &peko_auth::Subject,
    sent_at: chrono::DateTime<chrono::Utc>,
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
    let mut stop_sent = false;

    let mut has_started_line = false;
    // Run summary captured from the daemon's `RunSummary` packet —
    // printed as a single-line footer on stderr in `Done{success}`
    // so stdout stays pipe-safe (only the assistant text goes to
    // stdout). Field-test finding (2026-08-02): this footer was
    // previously *only* visible in `--no-stream` mode, hiding per-turn
    // token cost from the common streaming path.
    let mut summary: Option<crate::summary::RunSummaryView> = None;
    while let Some(packet) =
        next_or_stop(&mut stream, &ctrl_c_signal, &mut stop_sent, client, args, peer)
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
            // RunSummary is emitted by the daemon *before* `Done`.
            // Capture it so we can emit the per-turn footer after the
            // stream completes.
            ResponsePacket::RunSummary {
                iterations,
                usage,
                tool_errors,
                ..
            } => {
                summary = Some(crate::summary::RunSummaryView {
                    iterations,
                    usage: usage.map(|u| crate::summary::UsageView {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                        total_tokens: u.total_tokens,
                    }),
                    tool_errors: tool_errors
                        .into_iter()
                        .map(|e| crate::summary::ToolErrorView {
                            tool_name: e.tool_name,
                            error_message: e.error_message,
                        })
                        .collect(),
                });
            }
            ResponsePacket::Done { success, error, .. } => {
                // Busy path: a run is already in flight on this thread,
                // so the daemon queued the message onto the session
                // inbox instead of starting a run. Signalled by a
                // `[queued]`-prefixed `Done` error (mirrors the
                // `[not_found]` packet convention); from the user's
                // perspective this is success — nothing on stdout.
                if let Some(e) = error.as_deref() {
                    if e.starts_with("[queued]") {
                        eprintln!(
                            "[peko] {} is busy — message queued; it will be picked up at the next step (follow live: peko log {} --watch)",
                            args.principal, args.principal
                        );
                        if args.wait {
                            wait_for_queued_reply(client, args, peer, sent_at).await?;
                        }
                        return Ok(());
                    }
                }
                if has_started_line {
                    println!();
                }
                if success {
                    if let Some(ref s) = summary {
                        eprintln!("{}", s.format_footer());
                    }
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

/// Busy-path `--wait`: watch the thread until the principal posts a
/// reply newer than our queued message.
///
/// Uses the `PrincipalLogWatch` stream (replay + live, kept alive by
/// heartbeats) with a 10-minute cap. The DM channel has exactly two
/// authors (the peer and the principal), so any non-peer message
/// newer than `sent_at` is the answer. Peer-authored rows (including
/// the `⏹ stopped by user` marker) are ignored. If the watch can't be
/// established or errors mid-stream, fall back to a bounded
/// `principal_log` poll.
async fn wait_for_queued_reply(
    client: &DaemonClient,
    args: &SendArgs,
    peer: &peko_auth::Subject,
    sent_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    const WAIT_CAP: std::time::Duration = std::time::Duration::from_secs(600);

    match client
        .principal_log_watch(args.principal.clone(), Some(peer.clone()), None)
        .await
    {
        Ok(mut stream) => {
            let watch = async {
                while let Some(packet) = stream.next().await {
                    match packet {
                        ResponsePacket::PrincipalLogAppended { message, .. } => {
                            if message.timestamp > sent_at && &message.sender != peer {
                                println!("{}: {}", args.principal, message.text);
                                return true;
                            }
                        }
                        ResponsePacket::Heartbeat { .. } => {}
                        ResponsePacket::Error { message, .. } => {
                            eprintln!("[peko] --wait: log watch error ({message}); polling instead");
                            return false;
                        }
                        _ => {}
                    }
                }
                eprintln!("[peko] --wait: log watch stream closed early; polling instead");
                false
            };
            match tokio::time::timeout(WAIT_CAP, watch).await {
                Ok(true) => return Ok(()),
                // Watch failed mid-stream — fall through to the poll.
                Ok(false) => {}
                Err(_) => {
                    eprintln!(
                        "[peko] still waiting after {}s — giving up; the message stays queued (check `peko log {}`)",
                        WAIT_CAP.as_secs(),
                        args.principal
                    );
                    return Ok(());
                }
            }
        }
        Err(e) => eprintln!("[peko] --wait: log watch unavailable ({e}); polling instead"),
    }

    // Fallback: bounded `principal_log` poll every 2s (the pre-watch
    // implementation, kept for watch setup/mid-stream failures).
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    let deadline = std::time::Instant::now() + WAIT_CAP;
    loop {
        if std::time::Instant::now() >= deadline {
            eprintln!(
                "[peko] still waiting after {}s — giving up; the message stays queued (check `peko log {}`)",
                WAIT_CAP.as_secs(),
                args.principal
            );
            return Ok(());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        let resp = client
            .principal_log(args.principal.clone(), Some(peer.clone()), Some(5), None, None)
            .await;
        let messages = match resp {
            Ok(ResponsePacket::PrincipalLog { messages, .. }) => messages,
            Ok(other) => {
                eprintln!("[peko] --wait: unexpected log response {other:?}; retrying");
                continue;
            }
            Err(e) => {
                eprintln!("[peko] --wait: log poll failed ({e}); retrying");
                continue;
            }
        };
        if let Some(reply) = messages
            .iter()
            .find(|m| m.timestamp > sent_at && &m.sender != peer)
        {
            println!("{}: {}", args.principal, reply.text);
            return Ok(());
        }
    }
}

/// Race `stream.next()` against a Ctrl-C signal. On the first Ctrl-C,
/// sends a `PrincipalStop` for this send's (principal, peer) thread to
/// the daemon and continues reading the stream (the daemon will
/// eventually emit its own `Done` with `error: Some("stopped by
/// user")` and close it). Returns the next packet or `None` when the
/// stream is fully closed.
async fn next_or_stop(
    stream: &mut peko_core::ipc::PacketStream,
    ctrl_c_signal: &std::sync::Arc<tokio::sync::Notify>,
    stop_sent: &mut bool,
    client: &DaemonClient,
    args: &SendArgs,
    peer: &peko_auth::Subject,
) -> Result<Option<ResponsePacket>> {
    // Outer loop so a Ctrl-C that races with a packet still falls back
    // to the next stream.next() call to pick up the daemon's final
    // `Done`. The `if !*stop_sent` guard ensures we only send the
    // stop once even if the user mashes Ctrl-C.
    loop {
        let notified = ctrl_c_signal.notified();
        tokio::pin!(notified);
        tokio::select! {
            biased;
            packet = stream.next() => return Ok(packet),
            () = &mut notified, if !*stop_sent => {
                *stop_sent = true;
                eprintln!("\n[peko] Ctrl-C received — sending stop to daemon...");
                if let Err(e) = client
                    .principal_stop(args.principal.clone(), Some(peer.clone()))
                    .await
                {
                    eprintln!("[peko] failed to send stop to daemon: {e}");
                }
                // Loop back and let the stream's next packet be the
                // daemon's `Done { success: false, error: "stopped by user" }`.
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
            Ok(content) => Ok(content.trim().to_string()),
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
    use crate::commands::{from_cli, Cli, Commands};
    use clap::Parser;
    use peko_channel::ChannelPort as _;

    /// Bug 4 (scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md):
    /// `peko send <name> ""` previously called the LLM with empty content
    /// and silently burned a turn + tokens. The CLI now refuses empty /
    /// whitespace-only messages before any IPC.
    #[tokio::test]
    async fn handle_send_rejects_empty_message() {
        let cli =
            Cli::try_parse_from(["peko", "send", "myprincipal", ""]).expect("should parse send");
        let paths = from_cli(&cli);
        let args = match cli.command {
            Commands::Send(args) => args,
            _ => panic!("expected Send"),
        };
        let err = super::handle_send(args, &paths, false)
            .await
            .expect_err("empty message must be rejected before IPC");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Message is empty"),
            "error should explain the empty-message guard: {msg}"
        );
    }

    #[tokio::test]
    async fn handle_send_rejects_whitespace_only_message() {
        let cli = Cli::try_parse_from(["peko", "send", "myprincipal", "   \t\n  "])
            .expect("should parse send");
        let paths = from_cli(&cli);
        let args = match cli.command {
            Commands::Send(args) => args,
            _ => panic!("expected Send"),
        };
        let err = super::handle_send(args, &paths, false)
            .await
            .expect_err("whitespace-only message must be rejected before IPC");
        assert!(format!("{err:#}").contains("Message is empty"));
    }

    #[test]
    fn send_parses_principal_and_message() {
        let cli = Cli::try_parse_from(["peko", "send", "myprincipal", "hello"])
            .expect("should parse send command");

        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.principal, "myprincipal");
                assert_eq!(args.message, Some("hello".to_string()));
                assert!(!args.wait);
                assert!(args.peer.is_none());
            }
            _other => panic!("expected Send command"),
        }
    }

    #[test]
    fn send_parses_model_override_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "send",
            "myprincipal",
            "hello",
            "--model",
            "anthropic-claude-sonnet-4-5",
        ])
        .expect("should parse send command with --model");

        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.principal, "myprincipal");
                assert_eq!(args.message, Some("hello".to_string()));
                assert_eq!(args.model, Some("anthropic-claude-sonnet-4-5".to_string()));
            }
            _other => panic!("expected Send command"),
        }
    }

    #[test]
    fn send_parses_wait_flag() {
        let cli = Cli::try_parse_from(["peko", "send", "myprincipal", "hello", "--wait"])
            .expect("should parse send command with --wait");

        match cli.command {
            Commands::Send(args) => {
                assert!(args.wait);
                assert_eq!(args.principal, "myprincipal");
                assert_eq!(args.message, Some("hello".to_string()));
            }
            _other => panic!("expected Send command"),
        }
    }

    #[test]
    fn send_parses_peer_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "send",
            "myprincipal",
            "hello",
            "--peer",
            "user:alice",
        ])
        .expect("should parse send command with --peer");

        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.peer.as_deref(), Some("user:alice"));
                assert_eq!(args.principal, "myprincipal");
            }
            _other => panic!("expected Send command"),
        }
    }

    // -----------------------------------------------------------------
    // ADR-049 Phase 2 (D7): `peko send group:<slug>` posts as the
    // caller's user identity.
    // -----------------------------------------------------------------

    /// Build `SendArgs` for a group recipient without going through
    /// clap (the flag-refusal tests below still exercise the parse
    /// path end-to-end).
    fn group_send_args(principal: &str, message: &str) -> super::SendArgs {
        super::SendArgs {
            principal: principal.to_string(),
            message: Some(message.to_string()),
            file: None,
            stdin: false,
            wait: false,
            peer: None,
            model: None,
        }
    }

    /// `GlobalPaths` rooted in a tempdir, with the default `local`
    /// user identity (what `-U` defaults to).
    fn test_paths(tmp: &tempfile::TempDir) -> crate::commands::GlobalPaths {
        crate::commands::GlobalPaths::new(
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("cache"),
            "local".to_string(),
        )
    }

    /// Seed a `group:<slug>` channel (creator principal + the given
    /// user members) in the tempdir's runtime root. Returns the store
    /// so the test can assert on the resulting event log.
    async fn seed_group(
        paths: &crate::commands::GlobalPaths,
        slug: &str,
        user_members: &[&str],
    ) -> peko_channel::ChannelStore {
        use peko_channel::CreateOpts;
        use peko_subject::{PrincipalId, Subject};

        let store = peko_channel::ChannelStore::new(peko_channel::ChannelConfig {
            runtime_dir: paths.runtime_dir(),
            shared_dir: None,
        });
        let creator = PrincipalId("prin_alice".to_string());
        let channel_id =
            peko_protocol::channel::ChannelId::for_group(slug);
        let channel = store
            .create(&creator, CreateOpts::runtime(slug).with_id(channel_id))
            .await
            .expect("seed create");
        for user in user_members {
            store
                .invite(&channel, &creator, &Subject::User((*user).to_string()))
                .await
                .expect("seed invite");
        }
        store
    }

    #[tokio::test]
    async fn send_group_posts_as_caller_user_identity() {
        // No daemon in the test env, so `handle_send` exercises the
        // in-process fallback against the tempdir store.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let paths = test_paths(&tmp);
        let store = seed_group(&paths, "eng-standup", &["local"]).await;

        super::handle_send(group_send_args("group:eng-standup", "hi all"), &paths, false)
            .await
            .expect("group send must succeed for a member user");

        let events = store
            .peek(
                &peko_protocol::channel::ChannelId::for_group("eng-standup"),
                &peko_channel::Checkpoint::default(),
            )
            .await
            .expect("peek");
        let posted: Vec<_> = events
            .iter()
            .filter_map(|ev| match ev {
                peko_protocol::channel::ChannelEvent::Posted { author, text, .. } => {
                    Some((author, text))
                }
                _ => None,
            })
            .collect();
        assert_eq!(posted.len(), 1, "expected exactly one post; got {posted:?}");
        assert_eq!(posted[0].0, "user:local");
        assert_eq!(posted[0].1, "hi all");
    }

    #[tokio::test]
    async fn send_group_honors_peer_override() {
        // `--peer user:<id>` overrides the `-U` identity, mirroring
        // the principal path's derivation.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let paths = test_paths(&tmp);
        let store = seed_group(&paths, "eng", &["alice"]).await;

        let mut args = group_send_args("group:eng", "hello from alice");
        args.peer = Some("user:alice".to_string());
        super::handle_send(args, &paths, false)
            .await
            .expect("group send as --peer user must succeed");

        let events = store
            .peek(
                &peko_protocol::channel::ChannelId::for_group("eng"),
                &peko_channel::Checkpoint::default(),
            )
            .await
            .expect("peek");
        let authors: Vec<&str> = events
            .iter()
            .filter_map(|ev| match ev {
                peko_protocol::channel::ChannelEvent::Posted { author, .. } => {
                    Some(author.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(authors, vec!["user:alice"]);
    }

    #[tokio::test]
    async fn send_group_rejects_non_member_user() {
        // Membership is the write authorization: the caller's user
        // identity is not a member, so the store refuses with
        // `NotMember`.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let paths = test_paths(&tmp);
        let _store = seed_group(&paths, "eng", &[]).await;

        let err = super::handle_send(group_send_args("group:eng", "must not land"), &paths, false)
            .await
            .expect_err("non-member user must be refused");
        assert!(
            format!("{err:#}").contains("not a member"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn send_group_recipient_refuses_wait_flag() {
        let cli = Cli::try_parse_from(["peko", "send", "group:eng", "hi", "--wait"])
            .expect("should parse");
        let paths = from_cli(&cli);
        let args = match cli.command {
            Commands::Send(args) => args,
            _ => panic!("expected Send"),
        };
        let err = super::handle_send(args, &paths, false)
            .await
            .expect_err("--wait on a group must be refused");
        assert!(
            format!("{err:#}").contains("nothing to wait on"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn send_group_recipient_refuses_model_flag() {
        let cli = Cli::try_parse_from([
            "peko", "send", "group:eng", "hi", "--model", "openai-gpt-4o",
        ])
        .expect("should parse");
        let paths = from_cli(&cli);
        let args = match cli.command {
            Commands::Send(args) => args,
            _ => panic!("expected Send"),
        };
        let err = super::handle_send(args, &paths, false)
            .await
            .expect_err("--model on a group must be refused");
        assert!(
            format!("{err:#}").contains("--model is meaningless"),
            "got: {err:#}"
        );
    }
}
