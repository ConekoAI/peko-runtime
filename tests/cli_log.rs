//! CLI integration tests for `peko log` (feat/principal-log).
//!
//! Covers the authorization contract (ADR-042) end-to-end through the
//! daemon's IPC path:
//!
//! - Owner default view (no `--peer`) returns the owner's thread.
//! - Owner can read any peer's thread with `--peer <X>`.
//! - A peer can read only their own thread with `--peer <self>`.
//! - A peer without a Chat grant on the principal is rejected.
//! - A peer trying to read another peer's thread is rejected.
//! - `--json` round-trips the raw `HistoryEvent` array.
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

    let (stdout, stderr, status) = log(&cli, &["log", "log-principal-1"]);
    assert_log_ok(&stdout, &stderr, &status);
    // Default caller is user:default = owner; no send yet → empty events,
    // but the call must succeed and carry the principal name in the header.
    assert!(
        stdout.contains("log-principal-1"),
        "expected principal name in output\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("user:default"),
        "expected owner peer (user:default) in output\nstdout: {stdout}\nstderr: {stderr}"
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

/// `peko log --json` after a `peko send` returns the owner's thread as
/// parseable JSON with at least one `Message` event in `events[]`.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn log_json_returns_events_after_send() {
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

    // Parse the top-level JSON envelope and assert on `events`.
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\nstdout: {stdout}"));
    let events = parsed
        .get("events")
        .and_then(|v| v.as_array())
        .expect("expected `events` array in JSON output");
    assert!(
        !events.is_empty(),
        "expected at least one event after `peko send`\nstdout: {stdout}"
    );
    let principal_name = parsed.get("principal").and_then(|v| v.as_str());
    assert_eq!(principal_name, Some("log-principal-4"));
}

/// Owner can read a granted peer's thread via `--peer user:<peer>`.
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
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\nstdout: {stdout}"));
    let events = parsed
        .get("events")
        .and_then(|v| v.as_array())
        .expect("expected `events` array");
    assert!(
        !events.is_empty(),
        "expected events in bob's thread after his send\nstdout: {stdout}"
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

/// `peko send` with a non-default `-U` caller. Used by the populated-
/// session tests above.
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
