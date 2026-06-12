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
//! The daemon's IPC server is Unix-only (`#[cfg(unix)]` in `src/ipc/server.rs`),
//! so this entire file is cfg-gated. CI Linux runs these; Windows skips.
//!
//! Requires `MOCK_LLM_URL` to be set (CI sets it via docker-compose; locally
//! either run `make docker-up` or point `MOCK_LLM_URL` at any mock instance).
//! Tests early-return if unset so `cargo test` still passes on a bare checkout.

#![cfg(unix)]

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

    let (out, _, _) = run_with_timeout(
        || cli.cmd().stdout(Stdio::piped()).stderr(Stdio::piped()),
        &["send", "test-agent", "Hello there", "--no-stream"],
        Duration::from_secs(20),
    )
    .expect("run peko send");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "peko send exited non-zero (status={:?})\nstdout: {stdout}\nstderr: {stderr}",
        out.status
    );
    // Mock's default response is "Peko tunnel works!". The CLI may add
    // formatting (e.g. the agent name as a header), so match a substring.
    assert!(
        stdout.to_lowercase().contains("peko")
            || stdout.to_lowercase().contains("tunnel")
            || stdout.to_lowercase().contains("works"),
        "stdout did not contain the mock default response\nstdout: {stdout}\nstderr: {stderr}"
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
    let (out, _, _) = run_with_timeout(
        || cli.cmd().stdout(Stdio::piped()).stderr(Stdio::piped()),
        &[
            "send",
            "echo-agent",
            "Please complete the test. Respond with: CLI_SEND_OK",
            "--no-stream",
        ],
        Duration::from_secs(20),
    )
    .expect("run peko send");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "peko send exited non-zero (status={:?})\nstdout: {stdout}\nstderr: {stderr}",
        out.status
    );
    assert!(
        stdout.contains("CLI_SEND_OK"),
        "stdout did not echo the keyword 'CLI_SEND_OK'\nstdout: {stdout}\nstderr: {stderr}"
    );
}
