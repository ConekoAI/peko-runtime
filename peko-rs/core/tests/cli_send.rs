//! CLI integration tests for `peko send` (Phase B slice 1, per docs/integration/TESTING.md §7).
//!
//! Each test:
//!   1. Builds an isolated [`PekoCli`] tempdir as `HOME`.
//!   2. Writes a mock-LLM-pointed agent under `<HOME>/.peko/agents/<name>/`.
//!   3. Spawns a [`DaemonGuard`] (Drop kills the child).
//!   4. Runs `peko send …` via the universal [`run_with_timeout`] helper
//!      so a stuck subprocess panics in 20s with captured output instead
//!      of hanging the test job.
//!
//! The daemon's IPC server binds a Unix domain socket on Unix and a
//! Windows named pipe on Windows (ADR-038). This file used to be
//! `#![cfg(unix)]`; the gate was dropped when the Windows transport
//! landed so the same tests run on both platforms. See
//! `docs/architecture/adr/ADR-038-named-pipes-on-windows.md` for the
//! Windows side of the story.
//!
//! Requires `MOCK_LLM_URL` to be set (CI sets it via docker-compose; locally
//! either run `make docker-up` or point `MOCK_LLM_URL` at any mock instance).
//! Tests early-return if unset so `cargo test` still passes on a bare checkout.

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

/// Run `peko send` with the given args and return (stdout, stderr, status).
/// Panics on timeout with captured output.
fn send(cli: &PekoCli, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        args,
        Duration::from_secs(20),
    )
    .expect("run peko send");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

/// Assert that a `peko send` invocation exits successfully (code 0).
fn assert_send_ok(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert_eq!(
        status.code(),
        Some(0),
        "peko send exited non-zero (status={status:?})\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// Assert that a `peko send` invocation exits with a non-zero code.
fn assert_send_err(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert!(
        !status.success(),
        "expected peko send to fail, but it succeeded\nstdout: {stdout}\nstderr: {stderr}",
    );
}

// Note: session-routing tests (`--session`, `--new`, steering) were removed
// with the "Principal as the single actor" migration — `peko send` no longer
// exposes those flags (a Principal has a single per-peer session, resolved by
// the daemon).

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn send_default_response_streams_to_stdout() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "test-agent", &mock_url);

    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) =
        send(&cli, &["send", "test-agent", "Hello there", "--no-stream"]);
    assert_send_ok(&stdout, &stderr, &status);
    // The CI mock LLM is configured with `DEFAULT_RESPONSE=SUCCESS` in
    // tests/docker/docker-compose.integration.yml — every prompt that
    // doesn't match a keyword/tool-call/template returns that exact
    // string. The spec's "Peko tunnel works!" default is the upstream
    // mock fallback (used by `tunnel_e2e`), but the cli_send tests run
    // against the CI override. Assert against the configured value so
    // this test stays in sync with docker-compose.
    assert!(
        stdout.contains("SUCCESS"),
        "stdout did not contain the mock's configured default response 'SUCCESS'\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn send_keyword_echo_returns_marker() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "echo-agent", &mock_url);

    let _daemon = DaemonGuard::spawn(&cli);

    // Mock recognises `Respond with: <KEYWORD>` and echoes the keyword.
    let (stdout, stderr, status) = send(
        &cli,
        &[
            "send",
            "echo-agent",
            "Please complete the test. Respond with: CLI_SEND_OK",
            "--no-stream",
        ],
    );
    assert_send_ok(&stdout, &stderr, &status);
    assert!(
        stdout.contains("CLI_SEND_OK"),
        "stdout did not echo the keyword 'CLI_SEND_OK'\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --file option
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn send_file_option_reads_message_from_file() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "file-agent", &mock_url);

    let _daemon = DaemonGuard::spawn(&cli);

    // Write a test message file
    let test_file = cli.home().join("test_message.txt");
    std::fs::write(&test_file, "Respond with: FILE_OK").expect("write test file");

    let (stdout, stderr, status) = send(
        &cli,
        &[
            "send",
            "file-agent",
            "--file",
            test_file.to_str().unwrap(),
            "--no-stream",
        ],
    );
    assert_send_ok(&stdout, &stderr, &status);
    assert!(
        stdout.contains("FILE_OK"),
        "stdout did not echo the keyword from file 'FILE_OK'\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn send_nonexistent_agent_fails() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    // Do NOT write an agent config for "no-such-agent"
    create_mock_principal(&cli, "other-agent", &mock_url);

    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = send(&cli, &["send", "no-such-agent", "Hello", "--no-stream"]);
    assert_send_err(&stdout, &stderr, &status);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.to_lowercase().contains("not found")
            || combined.to_lowercase().contains("error")
            || combined.to_lowercase().contains("no such"),
        "expected error mentioning 'not found' or 'error' for non-existent agent, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn send_no_message_and_no_file_stdin_fails() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "no-msg-agent", &mock_url);

    let _daemon = DaemonGuard::spawn(&cli);

    // Send with no message, no --file, no --stdin
    let (stdout, stderr, status) = send(&cli, &["send", "no-msg-agent"]);
    assert_send_err(&stdout, &stderr, &status);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.to_lowercase().contains("required")
            || combined.to_lowercase().contains("message")
            || combined.to_lowercase().contains("error"),
        "expected error mentioning 'required' or 'message' for missing input, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Sprint 3 Phase 11: the peer conversation lands on the peer's DM channel
// ---------------------------------------------------------------------------

/// Recursively find every `channels/*/events.jsonl` under `root` (the
/// daemon's data dir lives under the test's isolated HOME; walking the
/// tree keeps this test agnostic of the exact platform layout).
fn channel_event_logs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "events.jsonl")
                && path
                    .parent()
                    .and_then(|p| p.parent())
                    .is_some_and(|gp| gp.file_name().is_some_and(|n| n == "channels"))
            {
                found.push(path);
            }
        }
    }
    found
}

/// Parse the `Posted` events out of a channel `events.jsonl`,
/// returning `(author, text)` rows oldest first.
fn posted_rows(path: &std::path::Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| v["kind"] == "posted")
        .map(|v| {
            (
                v["author"].as_str().unwrap_or_default().to_string(),
                v["text"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// After two successful `peko send`s, the peer's DM channel log
/// carries both directions — inbound rows attributed to the peer
/// (`user:*`, via `ChannelPort::post_attributed`), replies authored
/// by the principal (`prin_*`) — with each message present exactly
/// once (no double-posting across the two sends).
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn send_posts_both_directions_to_peer_dm_channel() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "dm-agent", &mock_url);

    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = send(
        &cli,
        &["send", "dm-agent", "FIRST_DM_MESSAGE", "--no-stream"],
    );
    assert_send_ok(&stdout, &stderr, &status);
    let (stdout, stderr, status) = send(
        &cli,
        &["send", "dm-agent", "SECOND_DM_MESSAGE", "--no-stream"],
    );
    assert_send_ok(&stdout, &stderr, &status);

    let logs = channel_event_logs(cli.home());
    assert_eq!(
        logs.len(),
        1,
        "exactly one DM channel should exist after two sends to one peer; found: {logs:?}"
    );

    let rows = posted_rows(&logs[0]);
    let inbound: Vec<&(String, String)> =
        rows.iter().filter(|(author, _)| author.starts_with("user:")).collect();
    let replies: Vec<&(String, String)> =
        rows.iter().filter(|(author, _)| author.starts_with("prin_")).collect();

    let inbound_texts: Vec<&str> = inbound.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(
        inbound_texts
            .iter()
            .filter(|t| **t == "FIRST_DM_MESSAGE")
            .count(),
        1,
        "FIRST_DM_MESSAGE must appear exactly once as a peer-authored row; rows: {rows:?}"
    );
    assert_eq!(
        inbound_texts
            .iter()
            .filter(|t| **t == "SECOND_DM_MESSAGE")
            .count(),
        1,
        "SECOND_DM_MESSAGE must appear exactly once as a peer-authored row; rows: {rows:?}"
    );
    assert!(
        replies.len() >= 2,
        "each send must post one principal-authored reply; rows: {rows:?}"
    );
}
