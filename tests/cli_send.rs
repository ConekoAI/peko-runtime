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
use common::{write_mock_agent, DaemonGuard, PekoCli, run_with_timeout};
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

/// Run `peko session list <agent> --json` and return the parsed JSON.
fn list_sessions_json(cli: &PekoCli, agent: &str) -> serde_json::Value {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        &["session", "list", agent, "--json"],
        Duration::from_secs(10),
    )
    .expect("run peko session list");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "peko session list exited non-zero (status={:?})\nstdout: {stdout}\nstderr: {stderr}",
        out.status
    );
    serde_json::from_str(&stdout).expect("parse session list JSON")
}

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
    write_mock_agent(cli.home(), "test-agent", &mock_url).expect("write mock agent");

    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = send(&cli, &["send", "test-agent", "Hello there", "--no-stream"]);
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
    write_mock_agent(cli.home(), "echo-agent", &mock_url).expect("write mock agent");

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
    write_mock_agent(cli.home(), "file-agent", &mock_url).expect("write mock agent");

    let _daemon = DaemonGuard::spawn(&cli);

    // Write a test message file
    let test_file = cli.home().join("test_message.txt");
    std::fs::write(&test_file, "Respond with: FILE_OK").expect("write test file");

    let (stdout, stderr, status) = send(
        &cli,
        &["send", "file-agent", "--file", test_file.to_str().unwrap(), "--no-stream"],
    );
    assert_send_ok(&stdout, &stderr, &status);
    assert!(
        stdout.contains("FILE_OK"),
        "stdout did not echo the keyword from file 'FILE_OK'\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --new flag (creates new session)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn send_new_flag_creates_additional_session() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "new-agent", &mock_url).expect("write mock agent");

    let _daemon = DaemonGuard::spawn(&cli);

    // First send — creates session 1
    let (stdout1, stderr1, status1) = send(
        &cli,
        &["send", "new-agent", "First message", "--no-stream"],
    );
    assert_send_ok(&stdout1, &stderr1, &status1);

    let sessions_after_first = list_sessions_json(&cli, "new-agent");
    let count_after_first = sessions_after_first
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(count_after_first, 1, "expected 1 session after first send");

    // Second send with --new — creates session 2
    let (stdout2, stderr2, status2) = send(
        &cli,
        &["send", "new-agent", "Second message", "--new", "--no-stream"],
    );
    assert_send_ok(&stdout2, &stderr2, &status2);

    let sessions_after_second = list_sessions_json(&cli, "new-agent");
    let count_after_second = sessions_after_second
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(
        count_after_second, 2,
        "expected 2 sessions after --new send, got {count_after_second}\n\
         stdout: {stdout2}\nstderr: {stderr2}"
    );
}

// ---------------------------------------------------------------------------
// --session option (resumes specific session)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn send_session_option_resumes_existing_session() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "sess-agent", &mock_url).expect("write mock agent");

    let _daemon = DaemonGuard::spawn(&cli);

    // First send — creates a session
    let (stdout1, stderr1, status1) = send(
        &cli,
        &["send", "sess-agent", "First message", "--no-stream"],
    );
    assert_send_ok(&stdout1, &stderr1, &status1);

    let sessions = list_sessions_json(&cli, "sess-agent");
    let sess_array = sessions
        .get("sessions")
        .and_then(|s| s.as_array())
        .expect("sessions array");
    assert_eq!(sess_array.len(), 1, "expected 1 session");
    let session_id = sess_array[0]
        .get("session_id")
        .and_then(|v| v.as_str())
        .expect("session_id string")
        .to_string();

    // Second send targeting the same session — should not create a new one
    let (stdout2, stderr2, status2) = send(
        &cli,
        &[
            "send",
            "sess-agent",
            "Respond with: SESSION_RESUME_OK",
            "--session",
            &session_id,
            "--no-stream",
        ],
    );
    assert_send_ok(&stdout2, &stderr2, &status2);
    // The mock LLM may or may not echo the keyword depending on prompt matching.
    // The core behavior we verify is that the session count stays at 1 (resumed,
    // not new session created).

    let sessions_after = list_sessions_json(&cli, "sess-agent");
    let count_after = sessions_after
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(
        count_after, 1,
        "expected still 1 session after --session resume, got {count_after}"
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
    write_mock_agent(cli.home(), "other-agent", &mock_url).expect("write mock agent");

    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = send(
        &cli,
        &["send", "no-such-agent", "Hello", "--no-stream"],
    );
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
    write_mock_agent(cli.home(), "no-msg-agent", &mock_url).expect("write mock agent");

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
