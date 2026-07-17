//! `peko log` — Inspect Principal activity
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
//!   This is the only user-facing way to inspect a principal's working
//!   state without running a turn.

use crate::commands::GlobalPaths;
use crate::common::services::session_service::HistoryEvent;
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
///   peko log my-principal --json | jq '.[].role'
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

    /// Cap on number of events returned (default 50, max 1000).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Only entries newer than the duration. Accepts `<N>h`, `<N>d`,
    /// `<N>m`, `<N>s` (e.g., `24h`, `7d`, `30m`, `3600s`).
    #[arg(long, value_name = "DURATION")]
    pub since: Option<String>,

    /// Emit the raw `HistoryEvent` array as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Handle the `peko log` command.
pub async fn handle_log(cmd: LogCommand, _paths: &GlobalPaths, json: bool) -> Result<()> {
    let LogCommand {
        principal,
        peer,
        limit,
        since,
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

    match client
        .principal_log(principal.clone(), peer_subject, limit, since_secs)
        .await?
    {
        ResponsePacket::PrincipalLog {
            events,
            session_id,
            peer: resolved_peer,
            truncated,
            ..
        } => {
            if use_json {
                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Out<'a> {
                    principal: &'a str,
                    peer: &'a str,
                    session_id: &'a Option<String>,
                    truncated: bool,
                    events: &'a [HistoryEvent],
                }
                let json = serde_json::to_string_pretty(&Out {
                    principal: &principal,
                    peer: &resolved_peer.to_string(),
                    session_id: &session_id,
                    truncated,
                    events: &events,
                })?;
                println!("{json}");
            } else if events.is_empty() {
                println!(
                    "📭 No events for peer '{resolved_peer}' on principal '{principal}' (session: {}).",
                    session_id.as_deref().unwrap_or("<none>")
                );
            } else {
                println!(
                    "📜 Principal '{principal}' — peer '{resolved_peer}' (session: {}):",
                    session_id.as_deref().unwrap_or("<none>")
                );
                for ev in &events {
                    render_history_event(ev);
                }
                if truncated {
                    println!(
                        "… (more events truncated by --limit; pass a larger --limit to see them)"
                    );
                }
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => Err(anyhow::anyhow!("peko log failed: {message}")),
        other => Err(crate::ipc::unexpected_response(&other)),
    }
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

/// Render one `HistoryEvent` to stdout in the default human view.
fn render_history_event(ev: &HistoryEvent) {
    match ev {
        HistoryEvent::Session {
            session_id: _,
            started_at,
        } => {
            println!("── session started ({}) ──", timestamp_short(started_at));
        }
        HistoryEvent::Message {
            role,
            content,
            timestamp,
        } => {
            println!(
                "[{}] {}: {}",
                timestamp_short(timestamp),
                capitalize(role),
                truncate(content, 240)
            );
        }
        HistoryEvent::ToolCall {
            tool_name,
            args,
            tool_call_id,
            timestamp,
        } => {
            let arg_preview = args
                .as_object()
                .map(|m| {
                    let mut parts: Vec<String> = m
                        .iter()
                        .take(2)
                        .map(|(k, v)| {
                            let v = if v.is_string() {
                                format!("\"{}\"", v.as_str().unwrap_or(""))
                            } else {
                                v.to_string()
                            };
                            format!("{k}={v}")
                        })
                        .collect();
                    if m.len() > 2 {
                        parts.push("…".into());
                    }
                    format!("{{{}}}", parts.join(", "))
                })
                .unwrap_or_else(|| truncate(args.to_string().as_str(), 80));
            println!(
                "[{}] ⤳ tool_call {tool_name} {arg_preview}  ({tool_call_id})",
                timestamp_short(timestamp)
            );
        }
        HistoryEvent::ToolResult {
            tool_call_id,
            output,
            error,
            timestamp: _,
        } => {
            let (status, body) = match (output.as_deref(), error.as_deref()) {
                (_, Some(err)) => ("error", err),
                (Some(out), _) => ("ok", out),
                (None, None) => ("empty", ""),
            };
            println!(
                "       ⤶ tool_result {status}  ({tool_call_id}) {}",
                truncate(body, 200)
            );
        }
        HistoryEvent::Thinking {
            content,
            timestamp: _,
        } => {
            println!("💭 {}", truncate(content, 200));
        }
        HistoryEvent::ModelChange {
            provider,
            model_id,
            timestamp: _,
        } => {
            println!("── model: {provider}/{model_id} ──");
        }
        HistoryEvent::Compaction {
            summary,
            timestamp: _,
        } => {
            println!("── compacted: {} ──", truncate(summary, 200));
        }
        HistoryEvent::Custom {
            custom_type,
            timestamp: _,
        } => {
            println!("• <{custom_type}>");
        }
    }
}

/// Strip seconds / sub-seconds from an RFC3339 timestamp for display.
fn timestamp_short(ts: &str) -> String {
    if let Some(_idx) = ts.find('T') {
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

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::with_capacity(s.len());
    out.extend(first.to_uppercase());
    out.extend(chars);
    out
}

/// RFC3339 timestamp for "now". Reserved for callers that need a
/// "rendered at" stamp independent of any event timestamp. Not used
/// by `render_history_event` directly — events carry their own
/// `timestamp` field (see `session_service.rs::HistoryEvent`).
#[allow(dead_code)]
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
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

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("user"), "User");
        assert_eq!(capitalize(""), "");
    }
}
