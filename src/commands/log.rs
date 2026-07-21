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
//! Internally the read path is the runtime-owned
//! `ChatLogStore` (sharded by `(principal_did, peer)`), distinct
//! from the principal's mutable session JSONL working memory.

use crate::chat_log::ChatLogMessage;
use crate::commands::GlobalPaths;
use crate::ipc::{DaemonClient, ResponsePacket};
use anyhow::{Context, Result};
use clap::Args;
use std::str::FromStr;

/// `peko log [OPTIONS] <PRINCIPAL>`
///
/// Examples:
///   peko log my-principal
///   peko log my-principal --limit 100
///   peko log my-principal --since 24h
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

    /// Cap on number of messages returned in this page (default 50,
    /// max 1000). Combined with `--cursor` for paging older
    /// messages.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Only messages newer than the duration. Accepts `<N>h`, `<N>d`,
    /// `<N>m`, `<N>s` (e.g., `24h`, `7d`, `30m`, `3600s`).
    #[arg(long, value_name = "DURATION")]
    pub since: Option<String>,

    /// Opaque pagination cursor returned by a prior `peko log`
    /// call's `next_cursor` field. Pairs with `--limit` to walk
    /// older messages without overlap or gaps.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,

    /// Emit the raw chat-message array (with `next_cursor` /
    /// `has_more`) as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Handle the `peko log` command. Walks chat-log pages via
/// `--cursor` until the caller has the slice they want. Recursion is
/// bounded to a small number of pages so a runaway caller can't pin
/// the daemon forever; in practice most callers use a single
/// page with a generous `--limit`.
pub async fn handle_log(cmd: LogCommand, _paths: &GlobalPaths, json: bool) -> Result<()> {
    let LogCommand {
        principal,
        peer,
        limit,
        since,
        cursor,
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

    let client = DaemonClient::connect()
        .await
        .context("Daemon is not running. Start it with: peko daemon start")?;

    let mut cursor = cursor.filter(|c| !c.is_empty());
    let mut accumulated: Vec<ChatLogMessage> = Vec::new();
    let mut resolved_peer: Option<crate::auth::Subject> = None;
    const MAX_PAGES: usize = 25;
    for _ in 0..MAX_PAGES {
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
            other => return Err(crate::ipc::unexpected_response(&other)),
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
            messages: &'a [ChatLogMessage],
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

/// Parse a `--peer` value into a `Subject`. Accepts the wire format
/// `user:<id>`, `principal:<did>`, or `public`.
fn parse_subject(value: &str) -> Result<crate::auth::Subject> {
    crate::auth::Subject::from_str(value)
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

/// Render one chat-log message to stdout in the default human view.
fn render_chat_message(message: &ChatLogMessage) {
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
}
