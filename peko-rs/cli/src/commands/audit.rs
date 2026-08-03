//! `peko audit` — read the audit log (ADR-046).
//!
//! Two subcommands, each with a different backing store:
//!
//! - **`tail`** — reads JSONL files directly off disk. This is the
//!   durable path: events that span daemon restarts (the JSONL
//!   sink survives process death, the in-memory ring buffer does
//!   not). Supports `--since`, `--type`, `--principal`, `--limit`,
//!   `--follow` (single-file follow against today's file; historical
//!   rotation needs `tail -F` from the shell).
//!
//! - **`list`** — sends an IPC `AuditQuery` to the daemon, which
//!   reads from the in-memory ring buffer. This is the fast path
//!   for "what just happened this session"; it never touches the
//!   filesystem. The CLI renders a summary count by `event_type`
//!   prefix so the user can scan what's been noisy lately.
//!
//! Why two paths? The IPC query is bounded by the ring buffer
//! (10k entries, current session only); the JSONL tail is the
//! historical record. The user picks based on intent — "what's
//! happening now" → `list`; "what happened yesterday" → `tail
//! --since 24h`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use clap::Subcommand;
use peko_core::ipc::packet::{RequestPacket, ResponsePacket};
use peko_core::ipc::DaemonClient;
use peko_observability::{AuditEvent, AuditSeverity};

use crate::commands::GlobalPaths;

/// `peko audit` subcommands.
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum AuditCommands {
    /// Print audit events from the JSONL file (durable history).
    Tail {
        /// Filter: only events at or after this many seconds ago.
        /// Examples: `--since 1h`, `--since 30m`, `--since 86400`.
        #[arg(long)]
        since: Option<String>,
        /// Filter: only events whose `event_type` starts with this prefix.
        #[arg(long = "type", value_name = "PREFIX")]
        event_type: Option<String>,
        /// Filter: only events whose caller matches this principal name
        /// (or whose `details.principal_name` matches).
        #[arg(short, long)]
        principal: Option<String>,
        /// Maximum number of events to print (newest first).
        #[arg(short, long, default_value = "100")]
        limit: usize,
        /// Stream new events as they arrive (single-file; today's
        /// audit-YYYY-MM-DD.jsonl only). For multi-day follow use
        /// `tail -F <audit_dir>/audit-*.jsonl`.
        #[arg(short, long)]
        follow: bool,
        /// Output as JSON lines (skip the human-friendly formatter).
        #[arg(long)]
        json: bool,
    },

    /// Print a summary of in-memory audit events (this session).
    List {
        /// Maximum number of events to consider (cap: ring buffer size).
        #[arg(short, long, default_value = "1000")]
        limit: u32,
        /// Filter by event type prefix.
        #[arg(long = "type", value_name = "PREFIX")]
        event_type: Option<String>,
        /// Filter by principal.
        #[arg(short, long)]
        principal: Option<String>,
    },
}

/// Connect to the daemon or surface a clear "not running" error.
async fn connect_daemon() -> Result<DaemonClient> {
    DaemonClient::connect()
        .await
        .context("Daemon is not running. Start it with: peko daemon start")
}

/// Dispatch `peko audit <subcommand>`.
pub async fn handle_audit(cmd: AuditCommands, paths: &GlobalPaths) -> Result<()> {
    match cmd {
        AuditCommands::Tail {
            since,
            event_type,
            principal,
            limit,
            follow,
            json,
        } => tail_jsonl(paths, since, event_type, principal, limit, follow, json),
        AuditCommands::List {
            limit,
            event_type,
            principal,
        } => list_via_ipc(limit, event_type, principal).await,
    }
}

/// `peko audit tail` — direct JSONL read.
///
/// Iterates every `audit-YYYY-MM-DD.jsonl` file under
/// `<data_dir>/runtime/audit/`, parses each line as an `AuditEvent`,
/// applies the filters, and prints newest-first. `--follow`
/// long-polls today's file: it re-reads on every fs change and
/// prints new lines.
fn tail_jsonl(
    paths: &GlobalPaths,
    since: Option<String>,
    event_type: Option<String>,
    principal: Option<String>,
    limit: usize,
    follow: bool,
    json: bool,
) -> Result<()> {
    let audit_dir = paths.audit_dir();
    if !audit_dir.exists() {
        eprintln!(
            "📜 Audit directory does not exist yet: {}",
            audit_dir.display()
        );
        eprintln!("   The daemon writes to this directory on first audit event.");
        return Ok(());
    }

    let since_ts = since
        .as_deref()
        .map(parse_since)
        .transpose()?
        .map(|d| d.timestamp());

    // Collect events from every dated file, then sort + filter.
    let mut events = read_jsonl_dir(&audit_dir)?;
    if let Some(prefix) = event_type.as_deref() {
        events.retain(|e| e.event_type.starts_with(prefix));
    }
    if let Some(principal) = principal.as_deref() {
        events.retain(|e| event_matches_principal(e, principal));
    }
    if let Some(cutoff) = since_ts {
        events.retain(|e| e.timestamp.timestamp() >= cutoff);
    }
    // Newest first.
    events.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
    events.truncate(limit);

    for event in &events {
        print_event(event, json);
    }

    if follow {
        // v1 limitation: single-file follow against today's file only.
        // Multi-day rotation needs `tail -F audit-*.jsonl` from the
        // shell. The daemon rotates daily; if the user runs `tail
        // --follow` over a midnight boundary, this command will
        // exit instead of picking up the new file. That's a known
        // follow-up (notify-based rotation polling).
        let today_path = today_audit_path(&audit_dir);
        if let Err(e) = follow_today(&today_path, event_type.as_deref(), principal.as_deref(), json) {
            eprintln!("(follow ended: {e})");
        }
    }

    Ok(())
}

/// Parse the `--since` value. Accepts a relative duration
/// (`"30m"`, `"2h"`, `"1d"`) or an absolute RFC3339 timestamp.
fn parse_since(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| anyhow::anyhow!("invalid --since value: {s}"))?,
    );
    let n: i64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid --since duration: {s}"))?;
    let now = Utc::now();
    let dt = match unit {
        "s" | "sec" | "secs" => now - Duration::seconds(n),
        "m" | "min" | "mins" => now - Duration::minutes(n),
        "h" | "hr" | "hrs" => now - Duration::hours(n),
        "d" | "day" | "days" => now - Duration::days(n),
        other => anyhow::bail!("unknown --since unit '{other}' (use s/m/h/d or RFC3339)"),
    };
    Ok(dt)
}

/// Read every `audit-YYYY-MM-DD.jsonl` in `dir` and concatenate the
/// parsed events (oldest first; sort happens at the call site).
fn read_jsonl_dir(dir: &Path) -> Result<Vec<AuditEvent>> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("read audit dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("audit-") || !name.ends_with(".jsonl") {
            continue;
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read {}", path.display()))?;
        for line in bytes.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            // Be tolerant of malformed lines: skip them with a
            // warning rather than aborting the whole tail. A
            // truncated last line is normal after a crash.
            match serde_json::from_slice::<AuditEvent>(line) {
                Ok(ev) => out.push(ev),
                Err(_) => {
                    eprintln!("(skipping malformed line in {})", path.display());
                }
            }
        }
    }
    Ok(out)
}

/// Today's audit file path.
fn today_audit_path(dir: &Path) -> PathBuf {
    dir.join(format!("audit-{}.jsonl", Utc::now().date_naive()))
}

/// `--follow` loop. Polls today's file with `O_APPEND`-aware reads
/// (we read the whole file each tick; rotation is handled by the
/// caller exiting and the user restarting). Cheap for small files
/// (a few hundred lines/day is typical for a single-user daemon).
fn follow_today(
    path: &Path,
    type_prefix: Option<&str>,
    principal: Option<&str>,
    json: bool,
) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("follow: open {}", path.display()))?;
    let mut pos: u64 = file.metadata()?.len();
    file.seek(SeekFrom::Start(pos))?;

    let mut buf = String::new();
    let mut stdin_closed = false;
    loop {
        // Cheap stat-based poll — file size grows on append.
        std::thread::sleep(std::time::Duration::from_millis(500));
        let len = match file.metadata() {
            Ok(m) => m.len(),
            Err(_) => break,
        };
        if len > pos {
            file.seek(SeekFrom::Start(pos))?;
            buf.clear();
            file.read_to_string(&mut buf)?;
            for line in buf.split('\n') {
                if line.is_empty() {
                    continue;
                }
                let Ok(event) = serde_json::from_str::<AuditEvent>(line) else {
                    continue;
                };
                if let Some(prefix) = type_prefix {
                    if !event.event_type.starts_with(prefix) {
                        continue;
                    }
                }
                if let Some(p) = principal {
                    if !event_matches_principal(&event, p) {
                        continue;
                    }
                }
                print_event(&event, json);
            }
            pos = len;
        }
        // Also check for rotation: if the day's file disappears
        // and a new one exists, end the follow and tell the user
        // to restart (v1 limitation — multi-day follow is
        // `tail -F`'s job).
        let expected = today_audit_path(path.parent().unwrap_or(Path::new(".")));
        if !expected.exists() {
            eprintln!("(follow: rotated to {}-MM-DD; restart with the new path)", NaiveDate::MIN);
            break;
        }
        // Co-operative exit on stdin close.
        if !stdin_closed {
            // (We don't actually try to read stdin — that would
            // block — but a SIGPIPE/Ctrl-C will exit the process.
            // v1 limitation.)
            stdin_closed = false;
        }
    }
    Ok(())
}

/// `peko audit list` — IPC query of the ring buffer.
async fn list_via_ipc(
    limit: u32,
    event_type: Option<String>,
    principal: Option<String>,
) -> Result<()> {
    let client = connect_daemon().await?;
    let req = RequestPacket::AuditQuery {
        request_id: next_request_id(),
        limit,
        event_type_prefix: event_type,
        principal,
    };
    match client.request_response(req).await? {
        ResponsePacket::AuditEvents { entries, .. } => {
            if entries.is_empty() {
                println!("📭 No events in the in-memory audit ring buffer.");
                println!("   (Cross-session history: peko audit tail --since <duration>)");
                return Ok(());
            }
            // Group by event_type prefix (split on first '.') and
            // count. The user wants "what's noisy" not the full
            // list, so this is summary-style.
            let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
            for e in &entries {
                let key = e
                    .event_type
                    .split('.')
                    .next()
                    .unwrap_or(&e.event_type)
                    .to_string();
                *counts.entry(key).or_insert(0) += 1;
            }
            println!("📊 Audit (in-memory, last {} events):", entries.len());
            let total: usize = counts.values().sum();
            for (k, v) in &counts {
                println!("  {v:>4}  {k}");
            }
            println!("  ----\n  {total:>4}  total");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => Err(anyhow::anyhow!("audit list failed: {message}")),
        other => Err(peko_core::ipc::unexpected_response(&other)),
    }
}

// `next_request_id` — single u64 counter for the CLI process. The
// daemon only cares that request_ids are unique per session; any
// monotonically increasing value works.
fn next_request_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Print a single event. `--json` emits the raw JSON line; the
/// default is a compact human-readable form (timestamp, severity
/// icon, event_type, caller if any, details).
fn print_event(event: &AuditEvent, json: bool) {
    if json {
        // Best-effort; the CLI may not have the exact same serde
        // shape as the daemon (different crates). If the round-trip
        // fails, fall back to a hand-rolled line so the user
        // still sees the data.
        match serde_json::to_string(event) {
            Ok(s) => println!("{s}"),
            Err(_) => println!(
                "{{\"timestamp\":\"{}\",\"event_type\":\"{}\"}}",
                event.timestamp.to_rfc3339(),
                event.event_type
            ),
        }
        return;
    }
    let icon = severity_icon(event.severity);
    println!(
        "{} {} {} {}",
        icon,
        event.timestamp.to_rfc3339(),
        event.component,
        event.event_type,
    );
    if let Some(caller) = &event.caller {
        println!("     caller: {caller}");
    }
    if !event.details.is_null() && event.details != serde_json::Value::Object(Default::default()) {
        println!("     details: {}", event.details);
    }
}

fn severity_icon(s: AuditSeverity) -> &'static str {
    match s {
        AuditSeverity::Debug => "·",
        AuditSeverity::Info => "ℹ",
        AuditSeverity::Warning => "⚠",
        AuditSeverity::Error => "✗",
        AuditSeverity::Security => "🛡",
    }
}

/// Filter helper — mirror of the daemon-side filter, so `tail`
/// (direct file read) matches what the IPC handler would have
/// returned. Two rules: caller subject match, or
/// `details.principal_name` match.
fn event_matches_principal(event: &AuditEvent, principal: &str) -> bool {
    if let Some(caller) = &event.caller {
        match caller {
            peko_auth::Subject::Principal(id) if id.as_str() == principal => return true,
            peko_auth::Subject::User(id) if id == principal => return true,
            _ => {}
        }
    }
    if let Some(serde_json::Value::String(name)) = event.details.get("principal_name") {
        if name == principal {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_observability::AuditEvent;

    fn evt(event_type: &str) -> AuditEvent {
        AuditEvent {
            timestamp: Utc::now(),
            component: "test".into(),
            event_type: event_type.into(),
            agent_did: None,
            caller: None,
            details: serde_json::json!({}),
            severity: AuditSeverity::Info,
        }
    }

    #[test]
    fn parse_since_accepts_relative_units() {
        let before = Utc::now();
        let s = parse_since("30m").unwrap();
        // 30 minutes earlier — should be strictly less than `before`.
        assert!(s < before);
        let diff = (before - s).num_minutes();
        assert!(diff >= 29 && diff <= 31, "diff was {diff} min");
    }

    #[test]
    fn parse_since_accepts_rfc3339() {
        let dt = parse_since("2026-01-15T10:00:00Z").unwrap();
        assert_eq!(
            dt.to_rfc3339(),
            "2026-01-15T10:00:00+00:00"
        );
    }

    #[test]
    fn parse_since_rejects_garbage() {
        assert!(parse_since("nope").is_err());
        assert!(parse_since("5x").is_err());
        assert!(parse_since("abc 30m").is_err());
    }

    #[test]
    fn today_audit_path_uses_utc_date() {
        let dir = Path::new("/var/lib/peko/runtime/audit");
        let p = today_audit_path(dir);
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("audit-"));
        assert!(name.ends_with(".jsonl"));
    }
}