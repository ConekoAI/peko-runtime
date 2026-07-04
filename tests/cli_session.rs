//! CLI integration tests for Principal session listing.
//!
//! After the "Principal as the single actor" migration, `peko send <name>`
//! targets a Principal and writes its session under
//! `{data_dir}/principals/<name>/memory/sessions/`. The per-agent session
//! management surface that this file used to exercise —
//! `session show / branch / switch / remove`, the `--session-id`/`--new`
//! flags, and per-user session isolation on a shared agent — has no Principal
//! equivalent and was removed with the migration:
//!
//!   * `peko session <agent>` still reads *agent* sessions
//!     (`{data_dir}/agents/<agent>/sessions/`), which `peko send` no longer
//!     populates, so those tests could never observe a session again.
//!   * A Principal is owned by a single user (`user:default` for the CLI
//!     caller); a cross-user `peko send <princ> --user alice` is denied by the
//!     `Permission::Chat` owner check, so multi-user isolation on one
//!     Principal is not expressible.
//!
//! What remains is *listing* a Principal's sessions via
//! `peko principal memory session <name>`. That command prints one line per
//! session in the form `{session_id} [{peer}] {title}` (it ignores `--json`),
//! or `No sessions found for principal '<name>'.` when empty. These tests
//! cover that surface against the current framework.
//!
//! All tests use the mock LLM for deterministic chat responses and the
//! standard PekoCli + DaemonGuard harness.

mod common;
use common::{create_mock_principal, run_with_timeout, DaemonGuard, PekoCli};
use std::process::Stdio;
use std::time::Duration;

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

/// Run a `peko` subcommand and return (stdout, stderr, status).
fn run(cli: &PekoCli, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        args,
        Duration::from_secs(20),
    )
    .expect("run peko command");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

/// Assert exit code 0.
fn assert_ok(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert_eq!(
        status.code(),
        Some(0),
        "exited non-zero (status={status:?})\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// List a Principal's sessions via `peko principal memory session <name>`,
/// returning the parsed session lines (one per stored session).
///
/// The command prints `{session_id} [{peer}] {title}` per session, or a single
/// `No sessions found …` line when empty. We treat any line containing the
/// `[peer]` marker as a session row and ignore the empty-state message.
fn principal_sessions(cli: &PekoCli, name: &str) -> Vec<String> {
    let (stdout, stderr, status) = run(cli, &["principal", "memory", "session", name]);
    assert_ok(&stdout, &stderr, &status);
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.contains(" [") && l.contains("] "))
        .map(str::to_string)
        .collect()
}

/// Send a message to a Principal, returning stdout.
fn send_msg(cli: &PekoCli, name: &str, msg: &str) -> String {
    let (stdout, stderr, status) = run(cli, &["send", name, msg, "--no-stream"]);
    assert_ok(&stdout, &stderr, &status);
    stdout
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn principal_session_list_shows_created_session() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "list-princ", &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Initially no sessions.
    assert!(
        principal_sessions(&cli, "list-princ").is_empty(),
        "expected no sessions before any send"
    );

    // A send creates exactly one session for the calling peer.
    send_msg(&cli, "list-princ", "Hello");

    let sessions = principal_sessions(&cli, "list-princ");
    assert_eq!(
        sessions.len(),
        1,
        "expected 1 session after send, got: {sessions:?}"
    );
    // The CLI caller is `user:default`, so the session is keyed to that peer.
    assert!(
        sessions[0].contains("[user:default]"),
        "session should be keyed to the caller peer user:default: {}",
        sessions[0]
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn principal_repeated_sends_collapse_into_one_session() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "collapse-princ", &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Multiple sends from the same caller reuse the same per-peer session
    // rather than spawning new ones (there is no `--new` on `peko send`).
    send_msg(&cli, "collapse-princ", "First message");
    send_msg(&cli, "collapse-princ", "Second message");
    send_msg(&cli, "collapse-princ", "Third message");

    let sessions = principal_sessions(&cli, "collapse-princ");
    assert_eq!(
        sessions.len(),
        1,
        "repeated sends from one caller should collapse into a single session, got: {sessions:?}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn principal_session_list_empty_for_unused_principal() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    create_mock_principal(&cli, "unused-princ", &mock_url);
    // No daemon / no send: listing a freshly created Principal is an offline
    // read and must report no sessions without error.
    let (stdout, stderr, status) = run(&cli, &["principal", "memory", "session", "unused-princ"]);
    assert_ok(&stdout, &stderr, &status);
    assert!(
        stdout.to_lowercase().contains("no sessions"),
        "expected empty-state message, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}
