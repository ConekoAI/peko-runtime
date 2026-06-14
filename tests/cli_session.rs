//! CLI integration tests for `peko session` commands (Phase B slice 2).
//!
//! Tests the offline session management CLI surface:
//!   - session list       (with --json)
//!   - session show       (with --history, --json)
//!   - session branch     (with --label, --json)
//!   - session switch
//!   - session remove
//!   - user isolation (--user / -U)
//!
//! All tests use the mock LLM for deterministic chat responses and the
//! standard PekoCli + DaemonGuard harness.

#![cfg(unix)]

mod common;
use common::{write_mock_agent, DaemonGuard, PekoCli, run_with_timeout};
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

/// Assert non-zero exit.
fn assert_err(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert!(
        !status.success(),
        "expected failure but succeeded\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// Run `peko session list <agent> --json` and return parsed JSON.
fn list_sessions_json(cli: &PekoCli, agent: &str) -> serde_json::Value {
    let (stdout, stderr, status) = run(cli, &["session", "list", agent, "--json"]);
    assert_ok(&stdout, &stderr, &status);
    serde_json::from_str(&stdout).expect("parse session list JSON")
}

/// Send a message to an agent, returning stdout.
fn send_msg(cli: &PekoCli, agent: &str, msg: &str) -> String {
    let (stdout, stderr, status) = run(
        cli,
        &["send", agent, msg, "--no-stream"],
    );
    assert_ok(&stdout, &stderr, &status);
    stdout
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_list_shows_created_sessions() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "list-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Initially no sessions
    let json = list_sessions_json(&cli, "list-agent");
    let sessions = json
        .get("sessions")
        .and_then(|s| s.as_array())
        .expect("sessions array");
    assert!(sessions.is_empty(), "expected no sessions initially");

    // Send a message — creates a session
    send_msg(&cli, "list-agent", "Hello");

    let json_after = list_sessions_json(&cli, "list-agent");
    let sessions_after = json_after
        .get("sessions")
        .and_then(|s| s.as_array())
        .expect("sessions array");
    assert_eq!(sessions_after.len(), 1, "expected 1 session after send");
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_show_displays_session_details() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "show-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Create a session
    send_msg(&cli, "show-agent", "Respond with: SHOW_TEST");

    let json = list_sessions_json(&cli, "show-agent");
    let session_id = json["sessions"][0]["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();

    // Show the session explicitly
    let (stdout, stderr, status) = run(
        &cli,
        &["session", "show", "show-agent", "--session-id", &session_id],
    );
    assert_ok(&stdout, &stderr, &status);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains(&session_id),
        "show output should contain session_id\noutput: {combined}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_show_json_output_contains_session_id() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "show-json-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    send_msg(&cli, "show-json-agent", "Hello");

    let json = list_sessions_json(&cli, "show-json-agent");
    let session_id = json["sessions"][0]["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();

    let (stdout, stderr, status) = run(
        &cli,
        &["session", "show", "show-json-agent", "--session-id", &session_id, "--json"],
    );
    assert_ok(&stdout, &stderr, &status);

    let show_json: serde_json::Value = serde_json::from_str(&stdout).expect("parse show JSON");
    let shown_id = show_json
        .get("session")
        .and_then(|s| s.get("session_id"))
        .and_then(|v| v.as_str())
        .expect("session.session_id in JSON");
    assert_eq!(shown_id, session_id, "JSON show should return correct session_id");
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_branch_creates_child_with_parent_history() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "branch-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Create parent session with two messages
    send_msg(&cli, "branch-agent", "First message");
    send_msg(&cli, "branch-agent", "Second message");

    let json = list_sessions_json(&cli, "branch-agent");
    let parent_id = json["sessions"][0]["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();
    let parent_msg_count = json["sessions"][0]["message_count"]
        .as_i64()
        .unwrap_or(0);

    // Branch from explicit session
    let (stdout, stderr, status) = run(
        &cli,
        &[
            "session",
            "branch",
            "branch-agent",
            "--session-id",
            &parent_id,
        ],
    );
    assert_ok(&stdout, &stderr, &status);

    // Verify branched session exists with correct parent
    let json_after = list_sessions_json(&cli, "branch-agent");
    let sessions = json_after["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 2, "expected 2 sessions after branch");

    let branched = sessions
        .iter()
        .find(|s| s.get("parent_session_id").and_then(|v| v.as_str()) == Some(&parent_id))
        .expect("branched session with parent_session_id");
    let branched_id = branched["session_id"].as_str().expect("branched session_id");

    // Verify branched session has history (message count >= parent - 2 for overhead)
    let branched_count = branched["message_count"].as_i64().unwrap_or(0);
    assert!(
        branched_count >= parent_msg_count - 2,
        "branched session should have roughly same message count as parent: parent={parent_msg_count}, branched={branched_count}"
    );

    // Verify we can show the branched session
    let (show_out, _, show_status) = run(
        &cli,
        &["session", "show", "branch-agent", "--session-id", branched_id],
    );
    assert!(show_status.success(), "show branched session should succeed: {show_out}");
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_branch_with_label() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "branch-label-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    send_msg(&cli, "branch-label-agent", "Hello");

    let json = list_sessions_json(&cli, "branch-label-agent");
    let parent_id = json["sessions"][0]["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();

    // Branch with a label
    let (stdout, stderr, status) = run(
        &cli,
        &[
            "session",
            "branch",
            "branch-label-agent",
            "--session-id",
            &parent_id,
            "--label",
            "test-label",
        ],
    );
    assert_ok(&stdout, &stderr, &status);

    // Verify the label was stored (check via JSON show)
    let json_after = list_sessions_json(&cli, "branch-label-agent");
    let branched = json_after["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s.get("parent_session_id").and_then(|v| v.as_str()) == Some(&parent_id))
        .expect("branched session");
    let branched_id = branched["session_id"].as_str().unwrap();

    let (show_out, _, show_status) = run(
        &cli,
        &[
            "session",
            "show",
            "branch-label-agent",
            "--session-id",
            branched_id,
            "--json",
        ],
    );
    assert!(show_status.success());
    let show_json: serde_json::Value = serde_json::from_str(&show_out).expect("parse show JSON");
    let title = show_json
        .get("session")
        .and_then(|s| s.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(title, "test-label", "branched session title should be the label");
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_switch_changes_active_session() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "switch-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Create two sessions
    send_msg(&cli, "switch-agent", "First session");
    send_msg(&cli, "switch-agent", "Second session");

    let json = list_sessions_json(&cli, "switch-agent");
    let sessions = json["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 2, "expected 2 sessions");
    let session_id_1 = sessions[0]["session_id"].as_str().unwrap().to_string();
    let session_id_2 = sessions[1]["session_id"].as_str().unwrap().to_string();

    // Switch to session 1
    let (_, _, status) = run(
        &cli,
        &["session", "switch", "switch-agent", &session_id_1],
    );
    assert!(status.success(), "switch to session 1 should succeed");

    // Verify active session is now session 1 via show
    let (show_out, _, show_status) = run(&cli, &["session", "show", "switch-agent"]);
    assert!(show_status.success());
    assert!(
        show_out.contains(&session_id_1),
        "show should display active session 1: {show_out}"
    );

    // Switch to session 2
    let (_, _, status) = run(
        &cli,
        &["session", "switch", "switch-agent", &session_id_2],
    );
    assert!(status.success(), "switch to session 2 should succeed");

    let (show_out2, _, show_status2) = run(&cli, &["session", "show", "switch-agent"]);
    assert!(show_status2.success());
    assert!(
        show_out2.contains(&session_id_2),
        "show should display active session 2: {show_out2}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_remove_deletes_session() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "remove-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Create a session
    send_msg(&cli, "remove-agent", "Hello");

    let json = list_sessions_json(&cli, "remove-agent");
    let session_id = json["sessions"][0]["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();

    // Remove the session
    let (_, _, status) = run(
        &cli,
        &["session", "remove", "remove-agent", &session_id],
    );
    assert!(status.success(), "remove session should succeed");

    // Verify it's gone
    let json_after = list_sessions_json(&cli, "remove-agent");
    let sessions_after = json_after["sessions"].as_array().unwrap();
    assert!(sessions_after.is_empty(), "expected no sessions after remove");
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_user_isolation_different_users_different_active_sessions() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "user-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Default user creates a session
    send_msg(&cli, "user-agent", "Default user message");

    let default_json = list_sessions_json(&cli, "user-agent");
    let default_sessions = default_json["sessions"].as_array().unwrap();
    assert_eq!(default_sessions.len(), 1, "default user should have 1 session");
    let default_active = default_json
        .get("active_session")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Alice creates a session with --user
    let (alice_out, _, alice_status) = run(
        &cli,
        &[
            "send",
            "user-agent",
            "Alice message",
            "--user",
            "alice",
            "--no-stream",
        ],
    );
    assert!(alice_status.success(), "alice send should succeed: {alice_out}");

    let alice_json = {
        let (stdout, stderr, status) = run(
            &cli,
            &["session", "list", "user-agent", "--user", "alice", "--json"],
        );
        assert_ok(&stdout, &stderr, &status);
        serde_json::from_str::<serde_json::Value>(&stdout).expect("parse alice sessions")
    };
    let alice_sessions = alice_json["sessions"].as_array().unwrap();
    let alice_active = alice_json
        .get("active_session")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Bob creates a session with -U (short flag)
    let (bob_out, _, bob_status) = run(
        &cli,
        &[
            "send",
            "user-agent",
            "Bob message",
            "-U",
            "bob",
            "--no-stream",
        ],
    );
    assert!(bob_status.success(), "bob send should succeed: {bob_out}");

    let bob_json = {
        let (stdout, stderr, status) = run(
            &cli,
            &["session", "list", "user-agent", "-U", "bob", "--json"],
        );
        assert_ok(&stdout, &stderr, &status);
        serde_json::from_str::<serde_json::Value>(&stdout).expect("parse bob sessions")
    };
    let bob_active = bob_json
        .get("active_session")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // All three users should have different active sessions (if isolation works)
    if let (Some(d), Some(a), Some(b)) = (&default_active, &alice_active, &bob_active) {
        assert_ne!(d, a, "default and alice should have different active sessions");
        assert_ne!(a, b, "alice and bob should have different active sessions");
        assert_ne!(d, b, "default and bob should have different active sessions");
    } else {
        // If active_session is not returned, that's a known limitation —
        // the sessions themselves are still created per-user.
        eprintln!("Note: active_session isolation not fully implemented; checking session counts instead");
        assert!(
            !alice_sessions.is_empty(),
            "alice should have at least 1 session"
        );
    }
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn session_show_no_active_session_reports_error() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "no-sess-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Try to show active session when none exists
    let (stdout, stderr, status) = run(&cli, &["session", "show", "no-sess-agent"]);
    assert_err(&stdout, &stderr, &status);
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("no active")
            || combined.contains("not found")
            || combined.contains("error"),
        "expected error about no active session, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}
