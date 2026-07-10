//! CLI integration tests for slash commands in `peko send`.
//!
//! These tests exercise the client-side slash dispatch added for v0:
//! - Built-in `/help` lists principal metadata and enabled extensions.
//! - `/help` filters extensions by the principal's `[capabilities] grants` allowlist.
//! - Unknown slash commands fail with a clear message.
//! - `--no-slash` and the `\/...` escape hatch route literal `/`-prefixed
//!   text to the LLM instead of the slash handler.
//!
//! All daemon-gated tests are `#[ignore]` by default so `cargo test` passes
//! on a bare checkout; CI runs them with the docker-compose mock stack.

mod common;
use common::{create_mock_principal, run_with_timeout, DaemonGuard, PekoCli};
use std::path::PathBuf;
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

/// Run an arbitrary `peko …` command and return (stdout, stderr, status).
fn run(
    cli: &PekoCli,
    args: &[&str],
    timeout: Duration,
) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        args,
        timeout,
    )
    .expect("run peko command");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

/// Absolute path to a fixture directory, relative to the crate root.
fn fixture_dir(relative: &str) -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is set by cargo for integration tests");
    PathBuf::from(manifest_dir)
        .join("e2e_tests_archive")
        .join("extensions")
        .join(relative)
}

fn assert_ok(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert_eq!(
        status.code(),
        Some(0),
        "exited non-zero (status={status:?})\nstdout: {stdout}\nstderr: {stderr}",
    );
}

fn assert_err(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert!(
        !status.success(),
        "expected command to fail, but it succeeded\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// Add a skill id to the principal's `[capabilities] grants` allowlist.
fn allow_extension(cli: &PekoCli, principal_name: &str, ext_id: &str) {
    let path = cli
        .peko_dir()
        .join("principals")
        .join(principal_name)
        .join("principal.toml");
    let raw = std::fs::read_to_string(&path).expect("read principal.toml");
    let mut cfg: peko::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    cfg.capabilities.push(format!("skill:{ext_id}"));
    std::fs::write(
        &path,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires peko daemon"]
fn slash_help_lists_builtin_help() {
    let cli = PekoCli::new();
    create_mock_principal(&cli, "alice", "http://localhost:0");
    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = send(&cli, &["send", "alice", "/help"]);
    assert_ok(&stdout, &stderr, &status);
    assert!(
        stdout.contains("Built-in slash commands:"),
        "stdout should list built-in slash commands\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("/help"),
        "stdout should mention /help\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Principal: alice"),
        "stdout should show principal name\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
#[ignore = "requires peko daemon"]
fn slash_help_respects_allowlist() {
    let cli = PekoCli::new();
    create_mock_principal(&cli, "alice", "http://localhost:0");
    let _daemon = DaemonGuard::spawn(&cli);

    // Install a skill via the daemon. It is enabled by default but not in
    // the principal's allowlist, so it should not appear in /help yet.
    let install_path = fixture_dir("skill/python/calculator-skill");
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &install_path.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    let (stdout, stderr, status) = send(&cli, &["send", "alice", "/help"]);
    assert_ok(&stdout, &stderr, &status);
    assert!(
        !stdout.contains("calculator-skill"),
        "skill should be hidden when not in allowlist\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Add the skill to the allowlist and re-run /help.
    allow_extension(&cli, "alice", "calculator-skill");

    let (stdout, stderr, status) = send(&cli, &["send", "alice", "/help"]);
    assert_ok(&stdout, &stderr, &status);
    assert!(
        stdout.contains("calculator-skill"),
        "skill should appear after being added to allowlist\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
#[ignore = "requires peko daemon"]
fn slash_unknown_command_fails() {
    let cli = PekoCli::new();
    create_mock_principal(&cli, "alice", "http://localhost:0");
    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = send(&cli, &["send", "alice", "/foo"]);
    assert_err(&stdout, &stderr, &status);
    assert!(
        stderr.contains("Only /help is available in v0"),
        "stderr should explain v0 scope\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn slash_no_slash_escapes_to_llm() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "alice", &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    let (stdout, stderr, status) = send(
        &cli,
        &["send", "alice", "--no-slash", "/help", "--no-stream"],
    );
    assert_ok(&stdout, &stderr, &status);
    assert!(
        stdout.contains("SUCCESS"),
        "literal /help should reach the mock LLM\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn slash_backslash_escape_sends_literal() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "alice", &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // The `\/help` argument is stripped to `/help` by send.rs and bypasses
    // the slash handler, so the literal text reaches the LLM.
    let (stdout, stderr, status) = send(&cli, &["send", "alice", "\\/help", "--no-stream"]);
    assert_ok(&stdout, &stderr, &status);
    assert!(
        stdout.contains("SUCCESS"),
        "escaped /help should reach the mock LLM\nstdout: {stdout}\nstderr: {stderr}"
    );
}
