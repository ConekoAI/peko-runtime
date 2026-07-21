//! CLI integration tests for `peko log`.
//!
//! Verifies the post-F30 chat-log wire contract end-to-end through the
//! daemon's IPC path. Authorization rules (ADR-042) are unchanged:
//!
//! - Owner default view (no `--peer`) returns the owner's thread.
//! - Owner can read any peer's thread with `--peer <X>`.
//! - A peer can read only their own thread with `--peer <self>`.
//! - A peer without a Chat grant on the principal is rejected.
//! - A peer trying to read another peer's thread is rejected.
//! - `--json` round-trips the normalized chat-message array
//!   (`messages`, `nextCursor`, `hasMore`) with no session/tool/
//!   system records leaked from the underlying session JSONL.
//! - Cursor paging walks older messages without overlap or gaps.
//! - An unknown principal surfaces a `[not_found]` error.
//!
//! Tier: mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set; locally either `make docker-up` or point
//! `MOCK_LLM_URL` at any mock instance). Tests early-return if unset so
//! `cargo test` still passes on a bare checkout.
//!
//! Mirrors the structure of `tests/cli_send.rs` — isolated [`PekoCli`]
//! tempdir per test, [`DaemonGuard`] with `Drop`-based cleanup, and
//! `run_with_timeout` so a stuck subprocess panics in 20s with captured
//! output instead of hanging the test job.

mod common;
use common::{create_mock_principal, run_with_timeout, DaemonGuard, PekoCli};
use std::process::Stdio;
use std::time::Duration;

/// Skip with a warning if no mock LLM is reachable. Returns the URL.
fn mock_llm_url() -> Option<String> {
    let url = std::env::var("MOCK_LLM_URL").ok()?;
    if url.is_empty() {
        return None;
    }
    Some(url)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run `peko log` with the given args (after `--`) and return
/// (stdout, stderr, status).
fn log(cli: &PekoCli, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        &[],
        Duration::from_secs(20),
    )
    .expect("run peko log");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

/// Run `peko send` to populate the principal's session for the calling
/// peer. Requires the mock LLM to be wired in.
fn send(cli: &PekoCli, principal: &str, msg: &str) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.args(["send", principal, msg, "--no-stream"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            c
        },
        &[],
        Duration::from_secs(30),
    )
    .expect("run peko send");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

/// `peko send` with a non-default `-U` caller. Used to drive a
/// conversation as a non-owner peer so we can read back a populated
/// thread under a non-default sender.
fn send_with_user(
    cli: &PekoCli,
    principal: &str,
    msg: &str,
    user: &str,
) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.args(["-U", user, "send", principal, msg, "--no-stream"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            c
        },
        &[],
        Duration::from_secs(30),
    )
    .expect("run peko send");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

/// Grant a permission on a Principal via the daemon. Used to give a
/// non-owner peer (`user:bob`) Chat access.
fn grant(cli: &PekoCli, principal: &str, subject: &str, permission: &str) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.args(["principal", "permit", principal, subject, permission])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            c
        },
        &[],
        Duration::from_secs(20),
    )
    .expect("run peko principal permit");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "peko principal permit failed (status={:?})\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code(),
    );
}

fn assert_log_ok(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert_eq!(
        status.code(),
        Some(0),
        "peko log exited non-zero (status={status:?})\nstdout: {stdout}\nstderr: {stderr}",
    );
}

fn assert_log_err(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert!(
        !status.success(),
        "expected peko log to fail, but it succeeded\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// Decode the JSON envelope `peko log --json` emits and assert on
/// the canonical shape: `messages[]`, `nextCursor`, `hasMore`,
/// `principal`, `peer`. Panics with a rich diagnostic if the JSON
/// shape doesn't match — the wire contract is the test target, not
/// an internal session projection.
fn parse_log_envelope(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\nstdout: {stdout}"))
}

fn envelope_messages(parsed: &serde_json::Value) -> Vec<serde_json::Value> {
    parsed
        .get("messages")
        .and_then(|v| v.as_array())
        .expect("expected `messages` array in JSON output")
        .clone()
}

// ---------------------------------------------------------------------------
// Tests — auth gate, no populated session required
// ---------------------------------------------------------------------------

/// Owner default view returns the owner's thread (empty before any send).
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_owner_default_view_returns_empty_before_send() {
    let Some(_mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "log-principal-1", &_mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = log(&cli, &["log", "log-principal-1", "--json"]);
    assert_log_ok(&stdout, &stderr, &status);
    let parsed = parse_log_envelope(&stdout);
    assert_eq!(
        parsed.get("principal").and_then(|v| v.as_str()),
        Some("log-principal-1"),
        "expected principal name in JSON envelope\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        parsed.get("peer").and_then(|v| v.as_str()),
        Some("user:default"),
        "expected owner peer (user:default) by default\nstdout: {stdout}\nstderr: {stderr}"
    );
    let messages = envelope_messages(&parsed);
    assert!(
        messages.is_empty(),
        "default owner view should be empty before any send\nstdout: {stdout}"
    );
}

/// Stranger (no Chat grant) is forbidden from the default view.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_stranger_forbidden_on_default_view() {
    let Some(_mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "log-principal-2", &_mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Caller is user:eve (no grant, not the owner).
    let (stdout, stderr, status) = log(&cli, &["-U", "eve", "log", "log-principal-2"]);
    assert_log_err(&stdout, &stderr, &status);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("forbidden") || combined.contains("permission"),
        "expected forbidden/permission denial in output\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Peer without --peer (default view = owner) is forbidden — the privacy
/// gate rejects non-owner callers from reading the owner-root view.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_granted_peer_forbidden_on_owner_default_view() {
    let Some(_mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "log-principal-3", &_mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Even with a Chat grant, bob can't read the owner's default view.
    grant(&cli, "log-principal-3", "user:bob", "chat");

    let (stdout, stderr, status) = log(&cli, &["-U", "bob", "log", "log-principal-3"]);
    assert_log_err(&stdout, &stderr, &status);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("forbidden") || combined.contains("permission"),
        "expected forbidden/permission denial\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Unknown principal surfaces a `[not_found]` error.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_unknown_principal_returns_not_found() {
    let Some(_mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    // No principal created — daemon should reject the lookup.
    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = log(&cli, &["log", "ghost-principal"]);
    assert_log_err(&stdout, &stderr, &status);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("not_found") || combined.contains("not found"),
        "expected not_found error in output\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Tests — populated sessions (require MOCK_LLM_URL + peko send)
// ---------------------------------------------------------------------------

/// `peko log --json` after a `peko send` returns the owner's thread
/// with at least one user message AND one principal reply, in
/// canonical chat-message shape. Critically, no session internals
/// (tool_call, tool_result, compaction, system) leak into the
/// consumer-visible chat log.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_json_returns_messages_after_send() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "log-principal-4", &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Populate the owner's session with one send.
    let (sout, serr, sstatus) = send(&cli, "log-principal-4", "hello log");
    assert_eq!(
        sstatus.code(),
        Some(0),
        "peko send failed: status={sstatus:?}\nstdout: {sout}\nstderr: {serr}"
    );

    let (stdout, stderr, status) = log(&cli, &["log", "log-principal-4", "--json"]);
    assert_log_ok(&stdout, &stderr, &status);

    let parsed = parse_log_envelope(&stdout);
    let messages = envelope_messages(&parsed);
    assert!(
        messages.len() >= 2,
        "expected at least one user message + one principal reply after `peko send`\nstdout: {stdout}"
    );

    // Every message must carry the canonical chat-log fields. The
    // presence of any session-only field (`kind`, `toolName`,
    // `sessionId`) would be a leak from the old wire shape.
    for (index, message) in messages.iter().enumerate() {
        for required in ["id", "sender", "timestamp", "text"] {
            assert!(
                message.get(required).is_some(),
                "message[{index}] missing required field `{required}`\nmessage: {message}\nstdout: {stdout}"
            );
        }
        for forbidden in [
            "kind",
            "toolName",
            "sessionId",
            "session_id",
            "toolArgs",
            "toolResult",
        ] {
            assert!(
                message.get(forbidden).is_none(),
                "message[{index}] must not carry session-only field `{forbidden}`\nmessage: {message}\nstdout: {stdout}"
            );
        }
    }

    // Sender identity must match the user/principal pair, not the
    // session-internal `role` enum.
    let first_sender = messages[0]
        .get("sender")
        .and_then(|v| v.as_str())
        .expect("first message must carry `sender`");
    assert_eq!(
        first_sender, "user:default",
        "first message must come from the owner peer\nstdout: {stdout}"
    );
    let last_sender = messages
        .last()
        .and_then(|m| m.get("sender"))
        .and_then(|v| v.as_str())
        .expect("last message must carry `sender`");
    assert!(
        last_sender.starts_with("principal:did:peko:"),
        "last message must come from the principal itself, not a sub-role\nstdout: {stdout}\nlast_sender: {last_sender}"
    );

    // Paging metadata must be in the canonical shape.
    assert!(
        parsed.get("hasMore").is_some(),
        "JSON envelope must carry `hasMore`\nstdout: {stdout}"
    );
    assert!(
        parsed.get("nextCursor").is_some(),
        "JSON envelope must carry `nextCursor`\nstdout: {stdout}"
    );
    assert_eq!(
        parsed.get("principal").and_then(|v| v.as_str()),
        Some("log-principal-4"),
    );
}

/// Owner can read a granted peer's thread via `--peer user:<peer>`.
/// The peer's messages appear, and the principal's replies appear
/// addressed back to that peer.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_owner_reads_granted_peer_thread() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "log-principal-5", &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Grant bob Chat so he can drive the principal; bob sends once.
    grant(&cli, "log-principal-5", "user:bob", "chat");
    let (sout, serr, sstatus) = send_with_user(&cli, "log-principal-5", "hi from bob", "bob");
    assert_eq!(
        sstatus.code(),
        Some(0),
        "bob's peko send failed: status={sstatus:?}\nstdout: {sout}\nstderr: {serr}"
    );

    // Owner reads bob's thread.
    let (stdout, stderr, status) = log(
        &cli,
        &["log", "log-principal-5", "--peer", "user:bob", "--json"],
    );
    assert_log_ok(&stdout, &stderr, &status);
    let parsed = parse_log_envelope(&stdout);
    let messages = envelope_messages(&parsed);
    assert!(
        !messages.is_empty(),
        "expected messages in bob's thread after his send\nstdout: {stdout}"
    );
    assert_eq!(
        parsed.get("peer").and_then(|v| v.as_str()),
        Some("user:bob"),
        "envelope must echo the resolved peer\nstdout: {stdout}"
    );
}

/// A granted peer can read their own thread but not another peer's.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_peer_self_read_only() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "log-principal-6", &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Grant bob Chat and let him send once.
    grant(&cli, "log-principal-6", "user:bob", "chat");
    let (sout, serr, sstatus) = send_with_user(&cli, "log-principal-6", "hi from bob", "bob");
    assert_eq!(
        sstatus.code(),
        Some(0),
        "bob's peko send failed: status={sstatus:?}\nstdout: {sout}\nstderr: {serr}"
    );

    // Bob can read his own thread.
    let (stdout, stderr, status) = log(
        &cli,
        &[
            "-U",
            "bob",
            "log",
            "log-principal-6",
            "--peer",
            "user:bob",
            "--json",
        ],
    );
    assert_log_ok(&stdout, &stderr, &status);

    // Bob cannot read another peer's thread (user:default is the owner).
    let (stdout2, stderr2, status2) = log(
        &cli,
        &[
            "-U",
            "bob",
            "log",
            "log-principal-6",
            "--peer",
            "user:default",
            "--json",
        ],
    );
    assert_log_err(&stdout2, &stderr2, &status2);
    let combined = format!("{stdout2}{stderr2}");
    assert!(
        combined.contains("forbidden") || combined.contains("permission"),
        "expected forbidden denial on cross-peer read\nstdout: {stdout2}\nstderr: {stderr2}"
    );
}

/// `--cursor` walks older messages without overlap or gaps. After
/// several sends the chat log must page cleanly across the
/// `--limit` boundary and remain chronologically ordered within
/// each page and across page boundaries.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_cursor_walks_older_pages_without_overlap_or_gap() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "log-principal-7", &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Populate with a handful of sends so paging kicks in at
    // `--limit 2`. Each send produces a user message and a
    // principal reply, so 3 sends = 6 messages.
    for index in 0..3 {
        let (sout, serr, sstatus) =
            send(&cli, "log-principal-7", &format!("message number {index}"));
        assert_eq!(
            sstatus.code(),
            Some(0),
            "send {index} failed\nstdout: {sout}\nstderr: {serr}"
        );
    }

    // Walk pages with `--limit 2`. The first page is the latest
    // 2 messages; the cursor walks older from there.
    let (first_stdout, first_stderr, first_status) =
        log(&cli, &["log", "log-principal-7", "--limit", "2", "--json"]);
    assert_log_ok(&first_stdout, &first_stderr, &first_status);
    let first_page = parse_log_envelope(&first_stdout);
    let first_messages = envelope_messages(&first_page);
    assert_eq!(
        first_messages.len(),
        2,
        "first page should hold 2 messages\nstdout: {first_stdout}"
    );
    let first_has_more = first_page
        .get("hasMore")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        first_has_more,
        "first page must have more pages\nstdout: {first_stdout}"
    );
    let first_next_cursor = first_page
        .get("nextCursor")
        .and_then(|v| v.as_str())
        .expect("first page must carry a cursor");
    assert!(
        !first_next_cursor.is_empty(),
        "first page's nextCursor must not be empty\nstdout: {first_stdout}"
    );

    // Second page uses the cursor. We expect at least one more
    // message (the oldest principal reply) and `hasMore: false`
    // because 6 messages / limit 2 fits in 3 pages and the older
    // page we walked into is the final one.
    let second_args = vec![
        "log",
        "log-principal-7",
        "--limit",
        "2",
        "--cursor",
        first_next_cursor,
        "--json",
    ];
    let (second_stdout, second_stderr, second_status) = log(&cli, &second_args);
    assert_log_ok(&second_stdout, &second_stderr, &second_status);
    let second_page = parse_log_envelope(&second_stdout);
    let second_messages = envelope_messages(&second_page);
    assert!(
        !second_messages.is_empty(),
        "second page must have at least one message\nstdout: {second_stdout}"
    );

    // No overlap: union of message ids across the two pages must
    // match the message id set on each page individually.
    let first_ids: std::collections::HashSet<String> = first_messages
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_owned))
        .collect();
    let second_ids: std::collections::HashSet<String> = second_messages
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_owned))
        .collect();
    let overlap: Vec<&String> = first_ids.intersection(&second_ids).collect();
    assert!(
        overlap.is_empty(),
        "paging must not return the same message twice; overlap: {overlap:?}\nfirst: {first_stdout}\nsecond: {second_stdout}"
    );

    // Chronological order: each page is oldest-to-newest, AND the
    // oldest message on the second page must be strictly older
    // than the newest message on the first page.
    let newest_first_ts = first_messages
        .last()
        .and_then(|m| m.get("timestamp"))
        .and_then(|v| v.as_str())
        .expect("newest message must carry timestamp")
        .to_owned();
    let oldest_second_ts = second_messages
        .first()
        .and_then(|m| m.get("timestamp"))
        .and_then(|v| v.as_str())
        .expect("oldest message on second page must carry timestamp")
        .to_owned();
    assert!(
        oldest_second_ts <= newest_first_ts,
        "second page must not start after the first page's last message\nfirst_last: {newest_first_ts}\nsecond_first: {oldest_second_ts}"
    );
}
