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
        || {
            // Each Command method returns &mut Command, so the closure
            // body must use let-bindings to materialise an owned value.
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
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
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
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
