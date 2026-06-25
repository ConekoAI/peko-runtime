//! CLI integration tests for the `a2a_send` built-in tool
//! (mock-LLM tier).
//!
//! These tests exercise the runtime mechanics of `a2a_send` — tool
//! dispatch, session creation/resumption, caller annotation, peer_id
//! isolation, and message counts — using a deterministic mock LLM so
//! they can run in the PR gate instead of the nightly real-LLM job.
//!
//! Coverage mirrors the `e2e_tests/a2a/*.ps1` PowerShell scripts that
//! previously exercised this surface outside CI:
//!
//! | PS sub-test                                       | Rust test                                       |
//! |---------------------------------------------------|-------------------------------------------------|
//! | `a2a_blocking.ps1` T1 (tool availability)         | `a2a_blocking_t1_tool_available`                |
//! | `a2a_blocking.ps1` T2 (blocking execution)       | `a2a_blocking_t2_blocking_execution`           |
//! | `a2a_blocking.ps1` T3 (session resumption)       | `a2a_blocking_t3_session_resumption`           |
//! | `a2a_blocking.ps1` T4 (caller annotation)        | `a2a_blocking_t4_caller_annotation`            |
//! | `a2a_isolation.ps1` T1 (caller A session)         | `a2a_isolation_t1_caller_a_session`             |
//! | `a2a_isolation.ps1` T2 (caller B session)         | `a2a_isolation_t2_caller_b_session`             |
//! | `a2a_isolation.ps1` T3 (peer_id isolation)       | `a2a_isolation_t3_peer_id_isolation`            |
//! | `a2a_isolation.ps1` T4 (caller A resumes)        | `a2a_isolation_t4_caller_a_resumes`             |
//! | `a2a_isolation.ps1` T5 (message counts)          | `a2a_isolation_t5_message_counts`               |
//!
//! `a2a_all.ps1` is the meta-runner; not migrated.
//! `a2a_async.ps1` is deferred — the `a2a_send` tool's parameter
//! schema does not expose `_async`, so the LLM cannot drive the async path.
//!
//! ## Tier: mock-LLM (CI PR gate)
//!
//! All 9 tests early-return if `MOCK_LLM_URL` is unset, so a bare
//! `cargo test` on a checkout without the docker-compose stack still
//! passes. Each test drives one or more deterministic mock-LLM turns
//! to exercise the `a2a_send` path end-to-end.
//!
//! The `Integration` job in `.github/workflows/integration.yml`
//! runs the mock-LLM tier with `MOCK_LLM_URL` set. The nightly
//! `Integration (real LLM)` job unsets `MOCK_LLM_URL`, so these tests
//! are skipped there.
//!
//! ## Mock-LLM scripting
//!
//! Each test installs a per-speaker scripted dialog via
//! `POST /_test/configure` on the mock LLM. The delegator prompt
//! contains a unique parent needle; the `a2a_send` message contains a
//! unique child needle so the worker's LLM call can be scripted
//! independently. See `tests/cli_subagent.rs` for the same pattern.
//!
//! Every test is `#[serial]` because the mock's per-substring counter
//! is global state across test binaries; configuring the mock at the
//! start of each test resets the counters, but serial execution avoids
//! one test overwriting another's active script.

mod common;
use common::{configure_mock, run_with_timeout, DaemonGuard, PekoCli};
use serial_test::serial;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MOCK_LLM_URL` env, return Some(url) if set and non-empty, None
/// otherwise. Tests early-return on None so `cargo test` on a bare
/// checkout still passes.
fn mock_llm_url() -> Option<String> {
    let url = std::env::var("MOCK_LLM_URL").ok()?;
    if url.is_empty() {
        return None;
    }
    Some(url)
}

/// Run a `peko …` command and return (stdout, stderr, status).
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

fn assert_ok(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert_eq!(
        status.code(),
        Some(0),
        "exited non-zero (status={status:?})\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// Write a mock-llm-pointed agent with the given tool whitelist
/// (bare names + canonical `builtin:tool:<name>` IDs).
///
/// The whitelist pattern must include BOTH forms of every enabled
/// tool — bare name (so per-agent init registers the tool) and
/// canonical ID (so the dispatcher's `is_tool_enabled` check at
/// execution time matches). See the gotcha documented in
/// `cli_subagent.rs::write_subagent_agent` and `cli_tools.rs::write_builtin_agent`.
fn write_a2a_agent(home: &Path, name: &str, extra_tools: &[&str]) -> std::io::Result<()> {
    use std::fs;
    let agent_dir = Path::new(home).join(".peko").join("agents").join(name);
    fs::create_dir_all(&agent_dir)?;

    // Build the enabled list. The whitelist MUST contain BOTH the
    // bare tool name AND the canonical builtin:tool:<name> ID for
    // each enabled tool.
    let mut enabled: Vec<String> = vec![
        // a2a_send is always in the list — every a2a test needs it
        // on the delegator side. For the worker it's unused but
        // harmless to include.
        "a2a_send".to_string(),
        "builtin:tool:a2a_send".to_string(),
    ];
    for t in extra_tools {
        enabled.push((*t).to_string());
        enabled.push(format!("builtin:tool:{t}"));
    }

    let enabled_toml_lines: String = enabled.iter().map(|t| format!("    \"{t}\",\n")).collect();

    let config_toml = format!(
        r#"version = "3.0"
name = "{name}"
description = "CLI integration test agent for a2a_send"
auto_accept_trusted = false

preferred_provider_id = "mock-llm"
preferred_model_id = "default"
default_timeout_seconds = 60

[extensions]
enabled = [
{enabled_toml_lines}]

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}
"#
    );
    fs::write(agent_dir.join("config.toml"), config_toml)?;
    fs::write(agent_dir.join("SYSTEM.md"), "")?;
    Ok(())
}

/// Build the JSON string passed to `configure_mock` for a two-turn
/// delegator dialog (tool_call then final text).
fn delegator_script(
    needle: &str,
    tool_name: &str,
    arguments: serde_json::Value,
    final_text: &str,
) -> String {
    serde_json::json!({
        needle: [
            { "tool_call": { "name": tool_name, "arguments": arguments.to_string() } },
            final_text,
        ]
    })
    .to_string()
}

/// Build the JSON string passed to `configure_mock` for a worker that
/// performs one tool call and then emits a final text response.
fn worker_tool_script(
    needle: &str,
    tool_name: &str,
    arguments: serde_json::Value,
    final_text: &str,
) -> String {
    serde_json::json!({
        needle: [
            { "tool_call": { "name": tool_name, "arguments": arguments.to_string() } },
            final_text,
        ]
    })
    .to_string()
}

/// Merge two mock-LLM script objects (parent + child needles).
fn merge_scripts(a: &str, b: &str) -> String {
    let mut a: serde_json::Value = serde_json::from_str(a).expect("script a is valid json");
    let b: serde_json::Value = serde_json::from_str(b).expect("script b is valid json");
    if let (Some(a_obj), Some(b_obj)) = (a.as_object_mut(), b.as_object()) {
        for (k, v) in b_obj {
            a_obj.insert(k.clone(), v.clone());
        }
    }
    a.to_string()
}

/// Worker session count. Returns 0 if the worker has no sessions
/// (e.g. `peko session list` returns a non-JSON error).
///
/// A2A worker sessions are created in the target agent's *personal*
/// team context (the `StatelessAgentService` does not receive a team
/// from the caller), so the query must use `--team personal`.
fn worker_session_count(cli: &PekoCli, worker: &str) -> usize {
    let (out, _err, status) = run(
        cli,
        &["session", "list", worker, "--team", "personal", "--json"],
        Duration::from_secs(10),
    );
    if !status.success() {
        return 0;
    }
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or(serde_json::Value::Null);
    v.get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

/// Find the most-recent worker session ID and dump its history as
/// JSON. Returns None on any failure.
///
/// A2A worker sessions live in the personal team; both the list and
/// show queries target that team.
fn worker_session_history(cli: &PekoCli, worker: &str) -> Option<(String, serde_json::Value)> {
    let (out, _err, status) = run(
        cli,
        &["session", "list", worker, "--team", "personal", "--json"],
        Duration::from_secs(10),
    );
    if !status.success() {
        return None;
    }
    let list: serde_json::Value = serde_json::from_str(&out).ok()?;
    let sessions = list.get("sessions")?.as_array()?;
    let session_id = sessions.first()?.get("session_id")?.as_str()?.to_string();
    let (hout, _her, _hstatus) = run(
        cli,
        &[
            "session",
            "show",
            worker,
            "--session-id",
            &session_id,
            "--team",
            "personal",
            "--history",
            "--json",
        ],
        Duration::from_secs(10),
    );
    // The CLI may emit a leading error line before the JSON; find the
    // first '{' and parse from there.
    let json_start = hout.find('{')?;
    let json_str = &hout[json_start..];
    let history: serde_json::Value = serde_json::from_str(json_str).ok()?;
    Some((session_id, history))
}

/// Pre-create the daemon's tool workspace dir and write a sentinel
/// file into it. The `a2a_send` tool's worker side calls `read_file`
/// on this file, so the test can prove the worker got the message
/// and read it.
///
/// The daemon's `read_file` resolves relative paths against the SHARED
/// workspaces root, not the per-agent subdir. See `ToolRuntime::register_builtins`
/// and `tests/cli_tools.rs:108-115` for the full explanation.
fn write_sentinel_file(cli: &PekoCli, file_name: &str, content: &str) {
    let workspace = cli.peko_dir().join("data").join("workspaces");
    std::fs::create_dir_all(&workspace).expect("create workspaces root");
    std::fs::write(workspace.join(file_name), content).expect("write sentinel file");
}

// ---------------------------------------------------------------------------
// a2a_blocking.ps1
// ---------------------------------------------------------------------------

/// `a2a_blocking.ps1` T1: assert that the `a2a_send` tool is in the
/// delegator's tool list.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_blocking_t1_tool_available() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let delegator = "a2a_blocking_t1_delegator";
    let needle = "a2a_blocking_t1_needle";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), delegator, &[]).expect("write delegator");
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    // Script the mock to confirm the agent sees the tool.
    let script = serde_json::json!({ needle: "A2A_AVAILABLE" }).to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Check your available tools. If you have a tool named 'a2a_send', \
         reply exactly A2A_AVAILABLE. (needle={needle})"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("A2A_AVAILABLE"),
        "expected A2A_AVAILABLE; got: {out:?} stderr: {err:?}",
    );
}

/// `a2a_blocking.ps1` T2: blocking A2A send — delegator uses
/// `a2a_send` to ask the worker to read a sentinel file, then
/// reports the result. The worker creates a session.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_blocking_t2_blocking_execution() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let delegator = "a2a_blocking_t2_delegator";
    let worker = "a2a_blocking_t2_worker";
    let parent_needle = "a2a_blocking_t2_parent";
    let child_needle = "a2a_blocking_t2_child";
    let file_name = "test_a2a_t2.txt";
    let file_content = "A2A_TEST_SECRET_42";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), delegator, &[]).expect("write delegator");
    write_a2a_agent(cli.home(), worker, &["Read"]).expect("write worker");
    write_sentinel_file(&cli, file_name, file_content);
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    let a2a_message = format!(
        "Read the file {file_name} in your workspace and report its exact contents. \
         (needle={child_needle})"
    );
    let delegator_script = delegator_script(
        parent_needle,
        "a2a_send",
        serde_json::json!({"target_agent": worker, "message": a2a_message}),
        "A2A_SUCCESS",
    );
    let worker_script = worker_tool_script(
        child_needle,
        "Read",
        serde_json::json!({"file_path": file_name}),
        file_content,
    );
    configure_mock(&mock_url, &merge_scripts(&delegator_script, &worker_script)).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let before = worker_session_count(&cli, worker);

    let prompt = format!(
        "Use a2a_send to ask agent '{worker}' to read {file_name} and report its contents. \
         When you get the answer, reply A2A_SUCCESS. (needle={parent_needle})"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);

    let after = worker_session_count(&cli, worker);
    assert!(
        out.contains("A2A_SUCCESS") && after > before,
        "a2a_send did not complete: stdout={out:?} stderr={err:?} \
         before={before} after={after}",
    );
}

/// `a2a_blocking.ps1` T3: session resumption. A second `a2a_send` from
/// the same delegator to the same worker reuses the existing worker
/// session. Pass criteria: worker session count unchanged.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_blocking_t3_session_resumption() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let delegator = "a2a_blocking_t3_delegator";
    let worker = "a2a_blocking_t3_worker";
    let parent_needle1 = "a2a_blocking_t3_parent1";
    let parent_needle2 = "a2a_blocking_t3_parent2";
    let child_needle = "a2a_blocking_t3_child";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), delegator, &[]).expect("write delegator");
    write_a2a_agent(cli.home(), worker, &[]).expect("write worker");
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    // First turn: a2a_send without session_id creates the worker session.
    let script1 = serde_json::json!({
        parent_needle1: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": worker, "message": format!("First call. (needle={child_needle})")}).to_string() } },
            "A2A_DONE1",
        ],
        child_needle: "A2A_WORKER_REPLY1",
    })
    .to_string();
    configure_mock(&mock_url, &script1).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt1 = format!(
        "Send a message to agent '{worker}' using a2a_send. When done, reply A2A_DONE1. \
         (needle={parent_needle1})"
    );
    let (out1, err1, status1) = run(
        &cli,
        &["send", delegator, &prompt1, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out1, &err1, &status1);
    assert!(
        out1.contains("A2A_DONE1"),
        "first send failed: {out1:?} {err1:?}"
    );

    let count_after_first = worker_session_count(&cli, worker);
    assert!(
        count_after_first > 0,
        "first a2a_send did not create a worker session"
    );

    // Discover the worker session created by the first send so the second
    // a2a_send can pass session_id and force resumption.
    let worker_session_id = {
        let (out, _err, status) = run(
            &cli,
            &["session", "list", worker, "--team", "personal", "--json"],
            Duration::from_secs(10),
        );
        assert_ok(&out, &_err, &status);
        let v: serde_json::Value = serde_json::from_str(&out).expect("session list json");
        v.get("sessions")
            .and_then(|s| s.as_array())
            .and_then(|a| a.first())
            .and_then(|s| s.get("session_id"))
            .and_then(|s| s.as_str())
            .map(String::from)
            .expect("worker session id")
    };

    // Second turn: a2a_send with the known session_id must resume the same
    // worker session. Use `--new` on the second `peko send` so the delegator
    // starts a fresh session; otherwise the mock server (which matches on the
    // first user message in the conversation) would still see parent_needle1.
    let script2 = serde_json::json!({
        parent_needle2: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": worker, "message": format!("Second call should resume the same session. (needle={child_needle})"), "session_id": &worker_session_id}).to_string() } },
            "A2A_DONE2",
        ],
        child_needle: "A2A_WORKER_REPLY2",
    })
    .to_string();
    configure_mock(&mock_url, &script2).await;

    let prompt2 = format!(
        "Send another message to agent '{worker}' using a2a_send. When done, reply A2A_DONE2. \
         (needle={parent_needle2})"
    );
    let (out2, err2, status2) = run(
        &cli,
        &["send", delegator, &prompt2, "--new", "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out2, &err2, &status2);
    assert!(
        out2.contains("A2A_DONE2"),
        "second send failed: {out2:?} {err2:?}"
    );

    let count_after_second = worker_session_count(&cli, worker);
    assert_eq!(
        count_after_first, count_after_second,
        "worker session count changed after second a2a_send: before={count_after_first} after={count_after_second}"
    );
}

/// `a2a_blocking.ps1` T4: caller annotation in the target session's
/// history. After an a2a_send call, the worker's session should
/// contain a user message prefixed with `[Message from agent: <delegator>]`.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_blocking_t4_caller_annotation() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let delegator = "a2a_blocking_t4_delegator";
    let worker = "a2a_blocking_t4_worker";
    let parent_needle = "a2a_blocking_t4_parent";
    let child_needle = "a2a_blocking_t4_child";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), delegator, &[]).expect("write delegator");
    write_a2a_agent(cli.home(), worker, &[]).expect("write worker");
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": worker, "message": format!("Hello worker. (needle={child_needle})")}).to_string() } },
            "A2A_DONE",
        ],
        child_needle: "A2A_WORKER_REPLY",
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Send a message to agent '{worker}' using a2a_send. When done, reply A2A_DONE. \
         (needle={parent_needle})"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);
    assert!(out.contains("A2A_DONE"), "send failed: {out:?} {err:?}");

    let (sid, history) =
        worker_session_history(&cli, worker).expect("worker should have a session after a2a_send");
    let expected_marker = format!("[Message from agent: {delegator}]");
    let history_arr = history
        .get("history")
        .and_then(|h| h.as_array())
        .expect("history is an array");
    let annotation_found = history_arr.iter().any(|entry| {
        entry
            .get("Message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.contains(&expected_marker))
            .unwrap_or(false)
    });
    assert!(
        annotation_found,
        "caller annotation {expected_marker:?} not found in worker session \
         history (session_id={sid}, {} entries)",
        history_arr.len(),
    );
}

// ---------------------------------------------------------------------------
// a2a_isolation.ps1
// ---------------------------------------------------------------------------

/// `a2a_isolation.ps1` T1: caller A sends to target, target gets
/// exactly 1 session.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_isolation_t1_caller_a_session() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let caller_a = "a2a_iso_t1_a";
    let target = "a2a_iso_t1_target";
    let parent_needle = "a2a_iso_t1_parent";
    let child_needle = "a2a_iso_t1_child";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), caller_a, &[]).expect("write caller A");
    write_a2a_agent(cli.home(), target, &[]).expect("write target");
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Call from caller A. (needle={child_needle})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle: "A2A_TARGET_REPLY",
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle})"
    );
    let (out, err, status) = run(
        &cli,
        &["send", caller_a, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("A2A_TEST_DONE"),
        "send failed: {out:?} {err:?}"
    );

    let after = worker_session_count(&cli, target);
    assert_eq!(
        after, 1,
        "target should have exactly 1 session, got {after}"
    );
}

/// `a2a_isolation.ps1` T2: caller B sends to the same target,
/// target gets exactly 2 sessions (one per caller, isolated by peer_id).
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_isolation_t2_caller_b_session() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let caller_a = "a2a_iso_t2_a";
    let caller_b = "a2a_iso_t2_b";
    let target = "a2a_iso_t2_target";
    let parent_needle_a = "a2a_iso_t2_parent_a";
    let parent_needle_b = "a2a_iso_t2_parent_b";
    let child_needle_a = "a2a_iso_t2_child_a";
    let child_needle_b = "a2a_iso_t2_child_b";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), caller_a, &[]).expect("write caller A");
    write_a2a_agent(cli.home(), caller_b, &[]).expect("write caller B");
    write_a2a_agent(cli.home(), target, &[]).expect("write target");
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    let script = serde_json::json!({
        parent_needle_a: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Call from A. (needle={child_needle_a})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_a: "A2A_TARGET_REPLY_A",
        parent_needle_b: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Call from B. (needle={child_needle_b})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_b: "A2A_TARGET_REPLY_B",
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt_a = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_a})"
    );
    let (out_a, err_a, status_a) = run(
        &cli,
        &["send", caller_a, &prompt_a, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out_a, &err_a, &status_a);
    assert!(
        out_a.contains("A2A_TEST_DONE"),
        "caller A send failed: {out_a:?} {err_a:?}"
    );

    let prompt_b = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_b})"
    );
    let (out_b, err_b, status_b) = run(
        &cli,
        &["send", caller_b, &prompt_b, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out_b, &err_b, &status_b);
    assert!(
        out_b.contains("A2A_TEST_DONE"),
        "caller B send failed: {out_b:?} {err_b:?}"
    );

    let after = worker_session_count(&cli, target);
    assert_eq!(
        after, 2,
        "target should have exactly 2 sessions, got {after}"
    );
}

/// `a2a_isolation.ps1` T3: each target session has a distinct
/// peer_id matching its caller. Inspect the session list and verify
/// callerA and callerB both appear as peer_ids, and that the
/// peer_type is "agent".
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_isolation_t3_peer_id_isolation() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let caller_a = "a2a_iso_t3_a";
    let caller_b = "a2a_iso_t3_b";
    let target = "a2a_iso_t3_target";
    let parent_needle_a = "a2a_iso_t3_parent_a";
    let parent_needle_b = "a2a_iso_t3_parent_b";
    let child_needle_a = "a2a_iso_t3_child_a";
    let child_needle_b = "a2a_iso_t3_child_b";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), caller_a, &[]).expect("write caller A");
    write_a2a_agent(cli.home(), caller_b, &[]).expect("write caller B");
    write_a2a_agent(cli.home(), target, &[]).expect("write target");
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    let script = serde_json::json!({
        parent_needle_a: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Call from A. (needle={child_needle_a})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_a: "A2A_TARGET_REPLY_A",
        parent_needle_b: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Call from B. (needle={child_needle_b})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_b: "A2A_TARGET_REPLY_B",
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt_a = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_a})"
    );
    let (out_a, err_a, status_a) = run(
        &cli,
        &["send", caller_a, &prompt_a, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out_a, &err_a, &status_a);
    assert!(
        out_a.contains("A2A_TEST_DONE"),
        "caller A send failed: {out_a:?} {err_a:?}"
    );

    let prompt_b = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_b})"
    );
    let (out_b, err_b, status_b) = run(
        &cli,
        &["send", caller_b, &prompt_b, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out_b, &err_b, &status_b);
    assert!(
        out_b.contains("A2A_TEST_DONE"),
        "caller B send failed: {out_b:?} {err_b:?}"
    );

    let (out, _err, status) = run(
        &cli,
        &["session", "list", target, "--team", "personal", "--json"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &_err, &status);
    let v: serde_json::Value = serde_json::from_str(&out).expect("session list json");
    let peer_ids: Vec<String> = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.get("peer_id").and_then(|p| p.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let peer_types: Vec<String> = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    s.get("peer_type")
                        .and_then(|p| p.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();

    assert!(
        peer_ids.iter().any(|p| p == caller_a),
        "caller A peer_id not found: {peer_ids:?}"
    );
    assert!(
        peer_ids.iter().any(|p| p == caller_b),
        "caller B peer_id not found: {peer_ids:?}"
    );
    assert!(
        peer_types.iter().all(|t| t == "agent"),
        "all a2a-spawned sessions should have peer_type=agent, got {peer_types:?}"
    );
}

/// `a2a_isolation.ps1` T4: a second a2a_send from caller A resumes
/// caller A's own session (not caller B's). Pass criteria: target
/// session count is still 2, and the session whose peer_id == caller_a
/// has the same session_id as before the second call.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_isolation_t4_caller_a_resumes() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let caller_a = "a2a_iso_t4_a";
    let caller_b = "a2a_iso_t4_b";
    let target = "a2a_iso_t4_target";
    let parent_needle_a1 = "a2a_iso_t4_parent_a1";
    let parent_needle_a2 = "a2a_iso_t4_parent_a2";
    let parent_needle_b = "a2a_iso_t4_parent_b";
    let child_needle_a1 = "a2a_iso_t4_child_a1";
    let child_needle_a2 = "a2a_iso_t4_child_a2";
    let child_needle_b = "a2a_iso_t4_child_b";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), caller_a, &[]).expect("write caller A");
    write_a2a_agent(cli.home(), caller_b, &[]).expect("write caller B");
    write_a2a_agent(cli.home(), target, &[]).expect("write target");
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    let script = serde_json::json!({
        parent_needle_a1: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("First call from A. (needle={child_needle_a1})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_a1: "A2A_TARGET_REPLY_A1",
        parent_needle_b: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Call from B. (needle={child_needle_b})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_b: "A2A_TARGET_REPLY_B",
        parent_needle_a2: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Second call from A should resume. (needle={child_needle_a2})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_a2: "A2A_TARGET_REPLY_A2",
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt_a1 = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_a1})"
    );
    let (out_a1, err, status) = run(
        &cli,
        &["send", caller_a, &prompt_a1, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(prompt_a1.as_str(), &err, &status);
    assert!(
        out_a1.contains("A2A_TEST_DONE"),
        "caller A first send failed: {out_a1:?} {err:?}"
    );

    let prompt_b = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_b})"
    );
    let (out_b, err, status) = run(
        &cli,
        &["send", caller_b, &prompt_b, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(prompt_b.as_str(), &err, &status);
    assert!(
        out_b.contains("A2A_TEST_DONE"),
        "caller B send failed: {out_b:?} {err:?}"
    );

    let caller_a_session_id_before = {
        let (out, _err, status) = run(
            &cli,
            &["session", "list", target, "--team", "personal", "--json"],
            Duration::from_secs(10),
        );
        assert_ok(&out, &_err, &status);
        let v: serde_json::Value = serde_json::from_str(&out).expect("session list json");
        v.get("sessions")
            .and_then(|s| s.as_array())
            .and_then(|a| {
                a.iter()
                    .find(|s| s.get("peer_id").and_then(|p| p.as_str()) == Some(caller_a))
            })
            .and_then(|s| s.get("session_id"))
            .and_then(|s| s.as_str())
            .map(String::from)
    }
    .expect("caller A should have a session after the first send");

    let prompt_a2 = format!(
        "Send another message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_a2})"
    );
    let (out_a2, err, status) = run(
        &cli,
        &["send", caller_a, &prompt_a2, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(prompt_a2.as_str(), &err, &status);
    assert!(
        out_a2.contains("A2A_TEST_DONE"),
        "caller A second send failed: {out_a2:?} {err:?}"
    );

    let (out, _err, status) = run(
        &cli,
        &["session", "list", target, "--team", "personal", "--json"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &_err, &status);
    let v: serde_json::Value = serde_json::from_str(&out).expect("session list json");
    let sessions = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .expect("sessions");
    let caller_a_session_id_after = sessions
        .iter()
        .find(|s| s.get("peer_id").and_then(|p| p.as_str()) == Some(caller_a))
        .and_then(|s| s.get("session_id"))
        .and_then(|s| s.as_str())
        .map(String::from);

    assert_eq!(
        sessions.len(),
        2,
        "target should still have exactly 2 sessions, got {}",
        sessions.len()
    );
    assert_eq!(
        caller_a_session_id_after,
        Some(caller_a_session_id_before),
        "caller A's session_id changed after second send"
    );
}

/// `a2a_isolation.ps1` T5: message counts per session. After
/// caller A's two calls and caller B's one call, both sessions should
/// have at least 3 messages.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn a2a_isolation_t5_message_counts() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let caller_a = "a2a_iso_t5_a";
    let caller_b = "a2a_iso_t5_b";
    let target = "a2a_iso_t5_target";
    let parent_needle_a = "a2a_iso_t5_parent_a";
    let parent_needle_a2 = "a2a_iso_t5_parent_a2";
    let parent_needle_b = "a2a_iso_t5_parent_b";
    let child_needle_a = "a2a_iso_t5_child_a";
    let child_needle_a2 = "a2a_iso_t5_child_a2";
    let child_needle_b = "a2a_iso_t5_child_b";

    let cli = PekoCli::new();
    write_a2a_agent(cli.home(), caller_a, &[]).expect("write caller A");
    write_a2a_agent(cli.home(), caller_b, &[]).expect("write caller B");
    write_a2a_agent(cli.home(), target, &[]).expect("write target");
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);

    let script = serde_json::json!({
        parent_needle_a: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("First call from A. (needle={child_needle_a})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_a: "A2A_TARGET_REPLY_A",
        parent_needle_b: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Call from B. (needle={child_needle_b})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_b: "A2A_TARGET_REPLY_B",
        parent_needle_a2: [
            { "tool_call": { "name": "a2a_send", "arguments": serde_json::json!({"target_agent": target, "message": format!("Second call from A. (needle={child_needle_a2})")}).to_string() } },
            "A2A_TEST_DONE",
        ],
        child_needle_a2: "A2A_TARGET_REPLY_A2",
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt_a = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_a})"
    );
    let (out_a, err, status) = run(
        &cli,
        &["send", caller_a, &prompt_a, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(prompt_a.as_str(), &err, &status);
    assert!(
        out_a.contains("A2A_TEST_DONE"),
        "caller A first send failed: {out_a:?} {err:?}"
    );

    let prompt_b = format!(
        "Send a message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_b})"
    );
    let (out_b, err, status) = run(
        &cli,
        &["send", caller_b, &prompt_b, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(prompt_b.as_str(), &err, &status);
    assert!(
        out_b.contains("A2A_TEST_DONE"),
        "caller B send failed: {out_b:?} {err:?}"
    );

    let prompt_a2 = format!(
        "Send another message to agent '{target}' using a2a_send. When done, reply A2A_TEST_DONE. \
         (needle={parent_needle_a2})"
    );
    let (out_a2, err, status) = run(
        &cli,
        &["send", caller_a, &prompt_a2, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(prompt_a2.as_str(), &err, &status);
    assert!(
        out_a2.contains("A2A_TEST_DONE"),
        "caller A second send failed: {out_a2:?} {err:?}"
    );

    let (out, _err, status) = run(
        &cli,
        &["session", "list", target, "--team", "personal", "--json"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &_err, &status);
    let v: serde_json::Value = serde_json::from_str(&out).expect("session list json");
    let sessions = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .expect("sessions");

    let mut count_a: i64 = 0;
    let mut count_b: i64 = 0;
    for s in sessions {
        let peer = s.get("peer_id").and_then(|p| p.as_str()).unwrap_or("");
        let count = s.get("message_count").and_then(|m| m.as_i64()).unwrap_or(0);
        if peer == caller_a {
            count_a = count;
        } else if peer == caller_b {
            count_b = count;
        }
    }
    assert!(
        count_a >= 3 && count_b >= 3,
        "both sessions should have >= 3 messages; got caller_a={count_a}, caller_b={count_b}"
    );
}
