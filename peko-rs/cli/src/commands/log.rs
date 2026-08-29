//! `peko log` — Inspect Principal chat history
//!
//! Reads a peer's conversation thread with a Principal. The default view
//! is the **owner-root view**: the conversation running on the
//! principal's owner behalf. Pass `--peer` to read a specific peer's
//! thread.
//!
//! Privacy contract (ADR-042):
//! - The owner can read any peer's thread.
//! - A non-owner peer can only read their own thread (matched by an
//!   exact `Subject`).
//! - There is no `peko session` command and there will never be one.
//!   This is the only user-facing way to inspect a principal's
//!   consumer-visible conversation without running a turn.
//!
//! Internally the read path walks the peer's **DM channel** log
//! (sprint 3 Phase 11 — `dm-<peer_child_slug>`, provisioned on first
//! contact; `principal_log` IPC → `find_peer_dm_channel` →
//! `peek_with_ids`, `Posted` events only), distinct from the
//! principal's mutable session JSONL working memory. Pre-Phase-11
//! chat-log history stays on disk but is no longer read here.
//!
//! Group channels (`peko log group:<slug>`) bypass the principal
//! privacy model: the channel log is read directly via the
//! `ChannelPeek` IPC, authors rendered verbatim. Reads are
//! membership-gated against the caller's `-U` user identity
//! (ADR-049 D6) — a non-member is refused by the daemon.

use crate::commands::{parse_recipient, GlobalPaths, Recipient};
use anyhow::{Context, Result};
use clap::Args;
use peko_core::ipc::packet::{PrincipalLogMessage, RequestPacket};
use peko_core::ipc::{DaemonClient, ResponsePacket};
use peko_protocol::channel::ChannelEvent;
use std::str::FromStr;

/// `peko log [OPTIONS] <PRINCIPAL>`
///
/// Examples:
///   peko log my-principal
///   peko log my-principal --limit 100
///   peko log my-principal --since 24h
///   peko log my-principal --watch
///   peko log my-principal --peer user:alice
///   peko log my-principal --json | jq '.[].sender'
#[derive(Args)]
#[command(disable_version_flag = true)]
pub struct LogCommand {
    /// Principal name (required)
    #[arg(value_name = "PRINCIPAL")]
    pub principal: String,

    /// Specific peer's thread (`user:<id>` or `principal:<did>`).
    /// Defaults to the principal's owner.
    #[arg(long, value_name = "SUBJECT")]
    pub peer: Option<String>,

    /// Cap on number of messages returned (default 50, max 1000).
    /// Hard cap: a single page. Use `--all` to drain older pages, or
    /// `--cursor` to page manually. Ignored with `--watch`.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Only messages newer than the duration. Accepts `<N>h`, `<N>d`,
    /// `<N>m`, `<N>s` (e.g., `24h`, `7d`, `30m`, `3600s`). Ignored
    /// with `--watch`.
    #[arg(long, value_name = "DURATION")]
    pub since: Option<String>,

    /// Opaque pagination cursor returned by a prior `peko log`
    /// call's `next_cursor` field. Pairs with `--limit` to walk
    /// older messages without overlap or gaps. With `--watch`, seeds
    /// the replay start (only rows newer than the cursor replay).
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,

    /// Drain ALL pages (bounded multi-page loop) instead of a single
    /// page. Ignored with `--watch`.
    #[arg(long)]
    pub all: bool,

    /// Block and stream new messages live (replay newer than
    /// `--cursor` first, then live rows as they're posted).
    /// Heartbeats keep a quiet thread's stream alive.
    #[arg(long)]
    pub watch: bool,

    /// Emit the raw chat-message array (with `next_cursor` /
    /// `has_more`) as JSON. With `--watch`: NDJSON — one message
    /// object per line.
    #[arg(long)]
    pub json: bool,
}

/// Handle the `peko log` command. Without flags: one page (daemon
/// default 50). `--limit N` is a hard cap on that single page; `--all`
/// opts into the multi-page drain (bounded so a runaway caller can't
/// pin the daemon forever); `--cursor` pages older messages manually.
/// `--watch` streams: replay newer than `--cursor`, then live rows.
pub async fn handle_log(cmd: LogCommand, paths: &GlobalPaths, json: bool) -> Result<()> {
    let LogCommand {
        principal,
        peer,
        limit,
        since,
        cursor,
        all,
        watch,
        json: cmd_json,
    } = cmd;

    let peer_subject = match peer.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(parse_subject(s)?),
        None => None,
    };

    let since_secs = match since.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(parse_duration_secs(s)?),
        None => None,
    };

    let use_json = cmd_json || json;

    // Group recipients (`group:<slug>`) read the channel's log
    // directly — no principal, no thread privacy check. Reads are
    // membership-gated (ADR-049 D6): the daemon refuses unless the
    // caller's `-U` user identity is a channel member.
    if let Recipient::Group(slug) = parse_recipient(&principal) {
        if peer_subject.is_some() {
            anyhow::bail!("--peer applies to principal threads, not group channels");
        }
        let client = DaemonClient::connect()
            .await
            .context("Daemon is not running. Start it with: peko daemon start")?;
        let requester = format!("user:{}", paths.user());
        return handle_group_log(&client, &slug, &requester, limit, since_secs, cursor, all, watch, use_json)
            .await;
    }

    let client = DaemonClient::connect()
        .await
        .context("Daemon is not running. Start it with: peko daemon start")?;

    if watch {
        if limit.is_some() || since_secs.is_some() || all {
            eprintln!("[peko] --watch ignores --limit/--since/--all; use --cursor to seed the replay");
        }
        return watch_principal_log(&client, &principal, peer_subject, cursor, use_json).await;
    }

    let mut cursor = cursor.filter(|c| !c.is_empty());
    let mut accumulated: Vec<PrincipalLogMessage> = Vec::new();
    let mut resolved_peer: Option<peko_auth::Subject> = None;
    // Without `--all` exactly one page is read (`--limit` is a hard
    // cap on it). With `--all`, pages drain until `has_more` is false.
    const MAX_PAGES: usize = 25;
    let max_pages = if all { MAX_PAGES } else { 1 };
    for _ in 0..max_pages {
        match client
            .principal_log(
                principal.clone(),
                peer_subject.clone(),
                limit,
                since_secs,
                cursor.clone(),
            )
            .await?
        {
            ResponsePacket::PrincipalLog {
                name: _,
                peer: page_peer,
                messages,
                next_cursor,
                has_more,
                ..
            } => {
                resolved_peer = Some(page_peer);
                accumulated.extend(messages);
                cursor = next_cursor;
                if cursor.is_none() || !has_more {
                    break;
                }
            }
            ResponsePacket::Error { message, .. } => {
                return Err(anyhow::anyhow!("peko log failed: {message}"));
            }
            other => return Err(peko_core::ipc::unexpected_response(&other)),
        }
    }

    let resolved_peer =
        resolved_peer.ok_or_else(|| anyhow::anyhow!("peko log: no pages received from daemon"))?;

    if use_json {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Out<'a> {
            principal: &'a str,
            peer: &'a str,
            next_cursor: &'a Option<String>,
            has_more: bool,
            messages: &'a [PrincipalLogMessage],
        }
        let json = serde_json::to_string_pretty(&Out {
            principal: &principal,
            peer: &resolved_peer.to_string(),
            next_cursor: &cursor,
            has_more: cursor.is_some(),
            messages: &accumulated,
        })?;
        println!("{json}");
    } else if accumulated.is_empty() {
        println!("📭 No messages for peer '{resolved_peer}' on principal '{principal}'.");
    } else {
        println!("📜 Principal '{principal}' — peer '{resolved_peer}':");
        for msg in &accumulated {
            render_chat_message(msg);
        }
        if let Some(next) = cursor.as_ref() {
            println!(
                "… (more pages available; re-run with --cursor {next:?} to read older messages)"
            );
        }
    }
    Ok(())
}

/// `peko log --watch`: replay rows newer than `--cursor`, then render
/// live rows as they're appended. Human mode reuses
/// `render_chat_message`; `--json` emits NDJSON (one message object
/// per line). Heartbeats are ignored; a lagged broadcast surfaces the
/// daemon's resync error; stream end (daemon shutdown) is a clean
/// exit. Ctrl-C kills the process via the default SIGINT behavior.
async fn watch_principal_log(
    client: &DaemonClient,
    principal: &str,
    peer: Option<peko_auth::Subject>,
    cursor: Option<String>,
    use_json: bool,
) -> Result<()> {
    let since_cursor = cursor.filter(|c| !c.is_empty());
    let mut stream = client
        .principal_log_watch(principal.to_string(), peer, since_cursor)
        .await?;
    loop {
        match stream.next().await {
            Some(ResponsePacket::PrincipalLogAppended { message, .. }) => {
                if use_json {
                    println!("{}", serde_json::to_string(&message)?);
                } else {
                    render_chat_message(&message);
                }
                use std::io::Write as _;
                std::io::stdout().flush()?;
            }
            Some(ResponsePacket::Heartbeat { .. }) => {}
            Some(ResponsePacket::Error { message, .. }) => {
                return Err(anyhow::anyhow!("peko log --watch failed: {message}"));
            }
            Some(_) => {}
            None => {
                eprintln!("[peko] log watch stream closed (daemon shutdown)");
                return Ok(());
            }
        }
    }
}

/// `peko log group:<slug>`: read the group channel's log directly via
/// the `ChannelPeek` IPC. `requester` is the caller's `-U` user
/// identity in Subject wire form — the daemon membership-gates the
/// read against it (ADR-049 D6).
///
/// Flag mapping onto what ChannelPeek actually supports:
/// - `--cursor N` → peek's `since` checkpoint (rows strictly newer
///   than line N).
/// - `--limit N` → client-side: keep the newest N rows (default 50).
/// - `--since <dur>` → client-side timestamp filter on `at`.
/// - `--all` → no-op (peek already returns the full tail).
/// - `--watch` → 2s poll loop (see `watch_group_log`).
///
/// Only `Posted` rows render, as `[<ts>] <author>: <text>` with the
/// author verbatim — group authors are raw ids (principal ids or
/// Subject wire forms), not thread peers, so no Subject mapping.
async fn handle_group_log(
    client: &DaemonClient,
    slug: &str,
    requester: &str,
    limit: Option<usize>,
    since_secs: Option<u64>,
    cursor: Option<String>,
    all: bool,
    watch: bool,
    use_json: bool,
) -> Result<()> {
    let channel = format!("group:{slug}");
    if watch {
        if limit.is_some() || since_secs.is_some() || all {
            eprintln!(
                "[peko] group --watch ignores --limit/--since/--all; use --cursor to seed the replay"
            );
        }
        return watch_group_log(client, &channel, requester, cursor, use_json).await;
    }

    let rows = peek_group_posted_rows(client, &channel, requester, cursor.filter(|c| !c.is_empty())).await?;
    let cutoff = since_secs.map(|s| chrono::Utc::now() - chrono::Duration::seconds(s as i64));
    let rows: Vec<&(String, String, String)> = rows
        .iter()
        .filter(|(at, _, _)| match cutoff {
            Some(cut) => chrono::DateTime::parse_from_rfc3339(at)
                .map(|dt| dt >= cut)
                .unwrap_or(true), // unparseable timestamps are kept
            None => true,
        })
        .collect();
    let cap = limit.unwrap_or(50).clamp(1, 1000);
    let total = rows.len();
    let rows = &rows[total.saturating_sub(cap)..];

    if use_json {
        let messages: Vec<serde_json::Value> = rows
            .iter()
            .map(|(at, author, text)| group_row_json(at, author, text))
            .collect();
        let out = serde_json::json!({ "channel": channel, "messages": messages });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if rows.is_empty() {
        println!("📭 No messages in group '{slug}'.");
    } else {
        println!("📜 Group '{slug}':");
        for (at, author, text) in rows {
            render_group_row(at, author, text);
        }
        if total > cap {
            println!("… (showing newest {cap} of {total} messages; raise --limit to see more)");
        }
    }
    Ok(())
}

/// Group `--watch`: the raw `ChannelEventsWatch` stream has no
/// heartbeats and would die at the CLI's 60s per-packet idle timeout
/// on a quiet channel, so this polls `ChannelPeek` every 2s and
/// prints rows beyond the ones already shown (the channel log is
/// append-only, so count-based diffing is exact). Ctrl-C exits via
/// default SIGINT.
async fn watch_group_log(
    client: &DaemonClient,
    channel: &str,
    requester: &str,
    cursor: Option<String>,
    use_json: bool,
) -> Result<()> {
    let mut printed = 0usize;
    let mut since = cursor.filter(|c| !c.is_empty());
    loop {
        let rows = peek_group_posted_rows(client, channel, requester, since.take()).await?;
        for (at, author, text) in rows.iter().skip(printed) {
            if use_json {
                println!("{}", group_row_json(at, author, text));
            } else {
                render_group_row(at, author, text);
            }
        }
        printed = rows.len();
        use std::io::Write as _;
        std::io::stdout().flush()?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// One `ChannelPeek` round-trip; returns the channel's `Posted` rows
/// (oldest→newest) since the cursor as `(at, author, text)` tuples.
async fn peek_group_posted_rows(
    client: &DaemonClient,
    channel: &str,
    requester: &str,
    cursor: Option<String>,
) -> Result<Vec<(String, String, String)>> {
    let packet = RequestPacket::ChannelPeek {
        request_id: 0,
        channel: channel.to_string(),
        since: cursor,
        requester: Some(requester.to_string()),
    };
    let events = match client.request_response(packet).await? {
        ResponsePacket::ChannelPeekResult { events, .. } => events,
        ResponsePacket::Error { message, .. } => {
            return Err(anyhow::anyhow!("group log failed: {message}"));
        }
        other => return Err(peko_core::ipc::unexpected_response(&other)),
    };
    Ok(events
        .into_iter()
        .filter_map(|ev| match ev {
            ChannelEvent::Posted { author, text, at, .. } => Some((at, author, text)),
            _ => None,
        })
        .collect())
}

/// Human rendering for one group row: same `[<ts>] <author>: <text>`
/// shape as principal-thread rows.
fn render_group_row(at: &str, author: &str, text: &str) {
    println!("[{}] {}: {}", timestamp_short(at), author, truncate(text, 240));
}

/// JSON rendering for one group row (batch arrays and watch NDJSON).
fn group_row_json(at: &str, author: &str, text: &str) -> serde_json::Value {
    serde_json::json!({ "at": at, "author": author, "text": text })
}

/// Parse a `--peer` value into a `Subject`. Accepts the wire format
/// `user:<id>`, `principal:<did>`, or `public`.
fn parse_subject(value: &str) -> Result<peko_auth::Subject> {
    peko_auth::Subject::from_str(value)
        .map_err(|e| anyhow::anyhow!("invalid --peer value '{value}': {e}"))
}

/// Parse a human-friendly duration string like "24h", "7d", "30m", "3600s"
/// into a number of seconds. Whitespace is ignored.
fn parse_duration_secs(input: &str) -> Result<u64> {
    let s = input.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration");
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .with_context(|| format!("invalid duration number in '{input}'"))?;
    if n == 0 {
        anyhow::bail!("duration must be > 0 (got '{input}')");
    }
    let multiplier: u64 = match unit.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86_400,
        "" => anyhow::bail!("missing unit in '{input}' (use s/m/h/d)"),
        other => anyhow::bail!("unknown duration unit '{other}' in '{input}'"),
    };
    Ok(n.saturating_mul(multiplier))
}

/// Render one log message to stdout in the default human view.
fn render_chat_message(message: &PrincipalLogMessage) {
    println!(
        "[{}] {}: {}",
        timestamp_short(&message.timestamp.to_rfc3339()),
        message.sender,
        truncate(&message.text, 240)
    );
}

/// Strip seconds / sub-seconds from an RFC3339 timestamp for display.
fn timestamp_short(ts: &str) -> String {
    if ts.find('T').is_some() {
        if let Some(plus) = ts[10..].find(['+', 'Z', '-']) {
            return format!("{} {}", &ts[..10], &ts[10..10 + plus]);
        }
        return format!("{} {}", &ts[..10], &ts[10..(ts.len().min(15))]);
    }
    ts.to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        let mut out = String::with_capacity(end + 1);
        out.push_str(&s[..end]);
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_parse_duration_secs_variants() {
        assert_eq!(parse_duration_secs("30s").unwrap(), 30);
        assert_eq!(parse_duration_secs("15m").unwrap(), 15 * 60);
        assert_eq!(parse_duration_secs("24h").unwrap(), 24 * 3600);
        assert_eq!(parse_duration_secs("7d").unwrap(), 7 * 86_400);
        assert_eq!(parse_duration_secs("3600s").unwrap(), 3600);
    }

    #[test]
    fn test_parse_duration_secs_rejects_bad_input() {
        assert!(parse_duration_secs("").is_err());
        assert!(parse_duration_secs("0h").is_err());
        assert!(parse_duration_secs("24").is_err());
        assert!(parse_duration_secs("24x").is_err());
    }

    #[test]
    fn test_truncate_respects_char_boundaries() {
        assert_eq!(truncate("hello world", 5), "hello…");
        // Non-ASCII should not split mid-codepoint.
        assert_eq!(truncate("héllo wörld", 4), "hél…");
    }

    #[test]
    fn log_parses_watch_flag() {
        let cli = crate::commands::Cli::try_parse_from(["peko", "log", "scout", "--watch"])
            .expect("should parse log command with --watch");

        match cli.command {
            crate::commands::Commands::Log(args) => {
                assert_eq!(args.principal, "scout");
                assert!(args.watch);
                assert!(!args.all);
            }
            _other => panic!("expected Log command"),
        }
    }

    #[test]
    fn log_parses_all_and_cursor_flags() {
        let cli = crate::commands::Cli::try_parse_from([
            "peko", "log", "scout", "--all", "--cursor", "41",
        ])
        .expect("should parse log command with --all/--cursor");

        match cli.command {
            crate::commands::Commands::Log(args) => {
                assert!(args.all);
                assert!(!args.watch);
                assert_eq!(args.cursor.as_deref(), Some("41"));
            }
            _other => panic!("expected Log command"),
        }
    }

    #[test]
    fn log_parses_group_recipient_with_watch() {
        let cli = crate::commands::Cli::try_parse_from([
            "peko",
            "log",
            "group:eng-standup",
            "--watch",
        ])
        .expect("should parse log command with a group recipient");

        match cli.command {
            crate::commands::Commands::Log(args) => {
                assert_eq!(args.principal, "group:eng-standup");
                assert!(args.watch);
            }
            _other => panic!("expected Log command"),
        }
    }
}
