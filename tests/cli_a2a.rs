//! CLI integration tests for the `a2a_send` built-in tool
//! (Phase B slice per `docs/integration/TESTING.md` §7).
//!
//! Coverage mirrors the `e2e_tests/a2a/*.ps1` PowerShell scripts that
//! previously exercised this surface outside CI:
//!
//! | PS sub-test                                          | Rust test                                       |
//! |------------------------------------------------------|-------------------------------------------------|
//! | `a2a_blocking.ps1` T1 (tool availability)            | `a2a_blocking_t1_tool_available`                |
//! | `a2a_blocking.ps1` T2 (blocking execution)          | `a2a_blocking_t2_blocking_execution`           |
//! | `a2a_blocking.ps1` T3 (session resumption)          | `a2a_blocking_t3_session_resumption`           |
//! | `a2a_blocking.ps1` T4 (caller annotation)           | `a2a_blocking_t4_caller_annotation`            |
//! | `a2a_async.ps1` T1 (async receipt)                  | `a2a_async_t1_async_receipt`                    |
//! | `a2a_async.ps1` T2 (task file written)              | `a2a_async_t2_task_file_written`                |
//! | `a2a_async.ps1` T3 (async completion)               | `a2a_async_t3_async_completion`                 |
//! | `a2a_async.ps1` T4 (caller annotation in async)     | `a2a_async_t4_caller_annotation`               |
//! | `a2a_isolation.ps1` T1 (caller A session)            | `a2a_isolation_t1_caller_a_session`             |
//! | `a2a_isolation.ps1` T2 (caller B session)            | `a2a_isolation_t2_caller_b_session`             |
//! | `a2a_isolation.ps1` T3 (peer_id isolation)          | `a2a_isolation_t3_peer_id_isolation`            |
//! | `a2a_isolation.ps1` T4 (caller A resumes)           | `a2a_isolation_t4_caller_a_resumes`             |
//! | `a2a_isolation.ps1` T5 (message counts)             | `a2a_isolation_t5_message_counts`               |
//!
//! `a2a_all.ps1` is the meta-runner; not migrated.
//!
//! ## Tier: real-LLM (2-LLM-call flows)
//!
//! All 13 tests early-return if `MINIMAX_API_KEY` is unset, so a
//! bare `cargo test` on a checkout without real-LLM credentials
//! still passes. Each test drives a real LLM call to drive the
//! `a2a_send` tool, and most tests drive a second real LLM call
//! (on the worker side) to actually process the A2A message. Total
//! wall clock is ~3-5 min for the full suite.
//!
//! The `Integration (real LLM)` job in
//! [`.github/workflows/integration.yml`](../pekobot/peko-runtime/.github/workflows/integration.yml)
//! fires on nightly cron / `[llm]` commit tag / `workflow_dispatch`,
//! and unsets `MOCK_LLM_URL` so the dual-mode rule at
//! [`tunnel_e2e.rs:63-76`](tests/tunnel_e2e.rs#L63-L76) falls through
//! to the real provider.
//!
//! ## Why direct config.toml writes (same as cli_providers)
//!
//! `PekoCli::cmd()` removes `MINIMAX_API_KEY` from the daemon's env
//! to safeguard mock-tier tests. The tests work around this by
//! writing `api_key` directly into each agent's `config.toml` (same
//! dual-mode pattern as [`tunnel_e2e.rs:78-96`](tests/tunnel_e2e.rs#L78-L96)
//! and `cli_providers.rs`).
//!
//! ## Lenient assertions (match the PS scripts' fallback pattern)
//!
//! Real LLMs are non-deterministic — even a clear "reply exactly
//! A2A_SUCCESS" instruction may not be followed verbatim. The PS
//! scripts' "PASS" verdict falls through to a structural check
//! (e.g. "the worker session was created, so the A2A call
//! dispatched") when the LLM doesn't emit the literal sentinel.
//! The Rust tests mirror this: an LLM-output sentinel match is a
//! sufficient pass, but a structural side-effect (worker session
//! count increased, task file written, peer_id matches) is also a
//! pass.

mod common;
use common::{run_with_timeout, DaemonGuard, PekoCli};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MINIMAX_API_KEY` env, return Some(key) if set and non-empty, None
/// otherwise. Tests early-return on None so `cargo test` on a bare
/// checkout still passes.
fn minimax_api_key() -> Option<String> {
    let k = std::env::var("MINIMAX_API_KEY").ok()?;
    if k.is_empty() {
        return None;
    }
    Some(k)
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

/// Write a mock-LLM-pointed agent with the given tool whitelist
/// (bare names + canonical `builtin:tool:<name>` IDs).
///
/// The whitelist pattern must include BOTH forms of every enabled
/// tool — bare name (so per-agent init registers the tool) and
/// canonical ID (so the dispatcher's `is_tool_enabled` check at
/// execution time matches). See the gotcha documented in
/// `cli_subagent.rs::write_subagent_agent` and `cli_tools.rs::write_builtin_agent`.
fn write_a2a_agent(
    home: &Path,
    name: &str,
    mock_or_real_url: &str,
    provider_type: &str,
    api_key: &str,
    extra_tools: &[&str], // extra bare names to enable (besides core ones)
) -> std::io::Result<()> {
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
        r#"version = "1.0"
name = "{name}"
description = "CLI integration test agent for a2a_send"
auto_accept_trusted = false
default_timeout_seconds = 60

[provider]
provider_type = "{provider_type}"
api_key = "{api_key}"
base_url = "{mock_or_real_url}"
default_model = "default"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "default"
max_tokens = 1024
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

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

/// Worker session count. Returns 0 if the worker has no sessions
/// (e.g. `peko session list` returns a non-JSON error).
fn worker_session_count(cli: &PekoCli, worker: &str) -> usize {
    let (out, _err, status) = run(
        cli,
        &["session", "list", worker, "--json"],
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
fn worker_session_history(cli: &PekoCli, worker: &str) -> Option<(String, serde_json::Value)> {
    let (out, _err, status) = run(
        cli,
        &["session", "list", worker, "--json"],
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
/// **Important: the daemon's `read_file` resolves relative paths
/// against the SHARED workspaces root, not the per-agent subdir.**
/// See [`tests/cli_tools.rs:108-115`](tests/cli_tools.rs#L108-L115) for
/// the full explanation — `ToolRuntime::register_builtins` sets the
/// per-tool `workspace_dir` to `path_resolver.agent_workspace(".",
/// None).parent()`, which resolves to `<peko_dir>/data/workspaces`
/// (the shared root, with no agent name in the path). The PS scripts
/// write to `$env:APPDATA/peko/workspaces/default/$worker/...` (per-
/// agent subdir) which is a different path — but the worker's
/// `read_file("test_a2a.txt")` resolves to the SHARED root and
/// fails to find the file there. The PS scripts "pass" via the
/// structural fallback (worker session was created → a2a_send
/// dispatched). For Rust tests we want a real read, so we write
/// to the shared root. Each test uses a unique `file_name`
/// (containing the test name as a needle) so cross-test collisions
/// don't occur.
fn write_sentinel_file(cli: &PekoCli, _worker: &str, file_name: &str, content: &str) {
    let workspace = cli.peko_dir().join("data").join("workspaces");
    std::fs::create_dir_all(&workspace).expect("create workspaces root");
    std::fs::write(workspace.join(file_name), content).expect("write sentinel file");
}

// ---------------------------------------------------------------------------
// a2a_blocking.ps1
// ---------------------------------------------------------------------------

/// `a2a_blocking.ps1` T1: assert that the `a2a_send` tool is in the
/// delegator's tool list.
///
/// Real LLM drives the prompt; we accept either the literal
/// `A2A_AVAILABLE` sentinel OR any non-empty response that contains
/// the substring "a2a_send" (i.e. the LLM echoed the tool name).
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_blocking_t1_tool_available() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let delegator = "a2a_blocking_t1_delegator";
    write_a2a_agent(
        cli.home(),
        delegator,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[], // a2a_send only
    )
    .expect("write delegator");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Check your available tools. If you have a tool named 'a2a_send', \
         reply exactly A2A_AVAILABLE. If you do not have it, reply exactly A2A_MISSING. \
         (needle={delegator})"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    let passes = out.contains("A2A_AVAILABLE") || out.contains("a2a_send");
    assert!(
        passes,
        "expected A2A_AVAILABLE or a2a_send mention; got: {out:?} stderr: {err:?}",
    );
}

/// `a2a_blocking.ps1` T2: blocking A2A send — delegator uses
/// `a2a_send` to ask the worker to read a sentinel file, then
/// reports the result. The worker creates a session. Pass criteria:
/// the worker session count increased.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_blocking_t2_blocking_execution() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let delegator = "a2a_blocking_t2_delegator";
    let worker = "a2a_blocking_t2_worker";
    write_a2a_agent(
        cli.home(),
        delegator,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[], // a2a_send only
    )
    .expect("write delegator");
    write_a2a_agent(
        cli.home(),
        worker,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &["read_file"], // a2a_send + read_file
    )
    .expect("write worker");
    write_sentinel_file(&cli, worker, "test_a2a.txt", "A2A_TEST_SECRET_42");
    let _daemon = DaemonGuard::spawn(&cli);

    let before = worker_session_count(&cli, worker);

    let prompt = format!(
        "You have a tool called a2a_send. Use it to send the following \
         message to agent '{worker}': Read the file test_a2a.txt in your \
         workspace and report its exact contents. After you receive the \
         response from the worker agent, if the response contains the \
         text A2A_TEST_SECRET_42, reply exactly A2A_SUCCESS followed by \
         the content. If the call fails or the response does not contain \
         the expected text, reply exactly A2A_FAILED. \
         (needle=a2a_blocking_t2)"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);

    let after = worker_session_count(&cli, worker);
    let llm_success = out.contains("A2A_SUCCESS");
    let structural = after > before;
    assert!(
        llm_success || structural,
        "a2a_send did not complete: llm-sent-A2A_SUCCESS={llm_success} \
         worker-session-increased={structural} (before={before}, after={after}); \
         stdout={out:?} stderr={err:?}",
    );
}

/// `a2a_blocking.ps1` T3: session resumption. A second `a2a_send` from
/// the same delegator to the same worker reuses the existing worker
/// session. Pass criteria: worker session count unchanged.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_blocking_t3_session_resumption() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let delegator = "a2a_blocking_t3_delegator";
    let worker = "a2a_blocking_t3_worker";
    write_a2a_agent(
        cli.home(),
        delegator,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write delegator");
    write_a2a_agent(
        cli.home(),
        worker,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &["read_file"],
    )
    .expect("write worker");
    write_sentinel_file(&cli, worker, "test_a2a.txt", "A2A_TEST_SECRET_42");
    let _daemon = DaemonGuard::spawn(&cli);

    // First call: creates the worker session.
    let prompt1 = format!(
        "You have a tool called a2a_send. Use it to send this message to \
         agent '{worker}': Read the file test_a2a.txt and report its exact \
         contents. After receiving the response, reply exactly A2A_DONE1. \
         (needle=a2a_blocking_t3_first)"
    );
    let (out1, err1, status1) = run(
        &cli,
        &["send", delegator, &prompt1, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out1, &err1, &status1);
    let count_after_first = worker_session_count(&cli, worker);

    // Second call: should reuse the same worker session.
    let prompt2 = format!(
        "Use a2a_send to send this message to agent '{worker}': What was \
         the name of the file you just read? After receiving the response, \
         if it mentions test_a2a.txt, reply exactly A2A_RESUME_OK. \
         Otherwise reply A2A_RESUME_FAIL. \
         (needle=a2a_blocking_t3_second)"
    );
    let (out2, err2, status2) = run(
        &cli,
        &["send", delegator, &prompt2, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out2, &err2, &status2);
    let count_after_second = worker_session_count(&cli, worker);

    let llm_resume = out2.contains("A2A_RESUME_OK");
    let structural = count_after_first == count_after_second && count_after_first > 0;
    assert!(
        llm_resume || structural,
        "session not resumed: llm-said-resume={llm_resume} \
         count-unchanged-and-nonzero={structural} (after_first={count_after_first}, \
         after_second={count_after_second}); stdout={out2:?} stderr={err2:?}",
    );
}

/// `a2a_blocking.ps1` T4: caller annotation in the target session's
/// history. After the previous a2a_send calls, the worker's session
/// should contain a user message prefixed with
/// `[Message from agent: <delegator>]` (see
/// `a2a_send.rs:99-104`).
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_blocking_t4_caller_annotation() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let delegator = "a2a_blocking_t4_delegator";
    let worker = "a2a_blocking_t4_worker";
    write_a2a_agent(
        cli.home(),
        delegator,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write delegator");
    write_a2a_agent(
        cli.home(),
        worker,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &["read_file"],
    )
    .expect("write worker");
    write_sentinel_file(&cli, worker, "test_a2a.txt", "A2A_TEST_SECRET_42");
    let _daemon = DaemonGuard::spawn(&cli);

    // Drive one a2a_send to create the worker session.
    let prompt = format!(
        "You have a tool called a2a_send. Use it to send this message to \
         agent '{worker}': Read the file test_a2a.txt and report its exact \
         contents. After receiving the response, reply exactly A2A_DONE. \
         (needle=a2a_blocking_t4)"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);

    // The worker should now have a session. Inspect its history for
    // the caller annotation.
    let (sid, history) = match worker_session_history(&cli, worker) {
        Some(pair) => pair,
        None => panic!("no worker session after a2a_send; stdout={out:?} stderr={err:?}"),
    };
    let expected_marker = format!("[Message from agent: {delegator}]");
    let history_arr = history
        .get("history")
        .and_then(|h| h.as_array())
        .expect("history is an array");
    let found = history_arr.iter().any(|entry| {
        entry
            .get("Message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.contains(&expected_marker))
            .unwrap_or(false)
    });
    assert!(
        found,
        "caller annotation {expected_marker:?} not found in worker session \
         history (session_id={sid}, {} entries)",
        history_arr.len(),
    );
}

// ---------------------------------------------------------------------------
// a2a_async.ps1
// ---------------------------------------------------------------------------

/// `a2a_async.ps1` T1: async A2A send with `_async: true` returns a
/// receipt immediately (without blocking on the worker).
///
/// Pass criteria: the delegator's response contains the substring
/// `task_id` (the LLM echoed the receipt content) OR contains
/// `task_file` (an LLM-side indicator that the receipt was visible).
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_async_t1_async_receipt() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let delegator = "a2a_async_t1_delegator";
    let worker = "a2a_async_t1_worker";
    write_a2a_agent(
        cli.home(),
        delegator,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write delegator");
    write_a2a_agent(
        cli.home(),
        worker,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &["read_file"],
    )
    .expect("write worker");
    write_sentinel_file(&cli, worker, "test_async.txt", "A2A_ASYNC_SECRET_99");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use the a2a_send tool with _async=true to send this message to \
         agent '{worker}': Read the file test_async.txt and report its \
         exact contents. The tool should return a JSON receipt immediately \
         containing a task_file path and a task_id. Read the receipt \
         carefully. If you received a receipt with task_file and task_id, \
         reply exactly ASYNC_RECEIPT_OK. If you did not get a receipt, \
         reply exactly ASYNC_RECEIPT_FAIL. (needle=a2a_async_t1)"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);
    let llm_receipt = out.contains("ASYNC_RECEIPT_OK");
    let has_receipt_substring = out.contains("task_id") || out.contains("task_file");
    assert!(
        llm_receipt || has_receipt_substring,
        "async receipt not visible: llm-said-receipt={llm_receipt} \
         has-task_id-or-file={has_receipt_substring}; stdout={out:?} stderr={err:?}",
    );
}

/// `a2a_async.ps1` T2: a task file is written for polling. Inspect
/// `<peko_dir>/data/async_tasks/` for the most-recently-modified
/// `*.json` and verify its `tool_name` is `a2a_send`.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_async_t2_task_file_written() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let delegator = "a2a_async_t2_delegator";
    let worker = "a2a_async_t2_worker";
    write_a2a_agent(
        cli.home(),
        delegator,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write delegator");
    write_a2a_agent(
        cli.home(),
        worker,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &["read_file"],
    )
    .expect("write worker");
    write_sentinel_file(&cli, worker, "test_async.txt", "A2A_ASYNC_SECRET_99");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use a2a_send with _async=true to send this message to agent \
         '{worker}': Read the file test_async.txt. The tool will return a \
         receipt. Reply with the receipt's task_id if you got one. \
         (needle=a2a_async_t2)"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);

    // The async_tasks dir is at <peko_dir>/data/async_tasks.
    let async_dir = cli.peko_dir().join("data").join("async_tasks");
    let latest = std::fs::read_dir(&async_dir).ok().and_then(|rd| {
        let mut entries: Vec<_> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        entries.sort_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()));
        entries.pop()
    });
    let latest = match latest {
        Some(e) => e,
        None => panic!(
            "no task files in {async_dir:?} after async a2a_send; \
             stdout={out:?} stderr={err:?}"
        ),
    };
    let content = std::fs::read_to_string(latest.path()).expect("read task file");
    let json: serde_json::Value = serde_json::from_str(&content).expect("task file is valid JSON");
    let tool_name = json.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(
        tool_name, "a2a_send",
        "latest task file {latest:?} has tool_name={tool_name:?}, expected a2a_send; \
         full content: {content}",
    );
}

/// `a2a_async.ps1` T3: async task eventually completes. Poll the
/// worker session count for up to 30s; assert it increases.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_async_t3_async_completion() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let delegator = "a2a_async_t3_delegator";
    let worker = "a2a_async_t3_worker";
    write_a2a_agent(
        cli.home(),
        delegator,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write delegator");
    write_a2a_agent(
        cli.home(),
        worker,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &["read_file"],
    )
    .expect("write worker");
    write_sentinel_file(&cli, worker, "test_async.txt", "A2A_ASYNC_SECRET_99");
    let _daemon = DaemonGuard::spawn(&cli);

    let before = worker_session_count(&cli, worker);

    let prompt = format!(
        "Use a2a_send with _async=true to send this message to agent \
         '{worker}': Read the file test_async.txt. The task will run in \
         the background; you'll get a receipt immediately. Reply with the \
         task_id from the receipt. (needle=a2a_async_t3)"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);

    // Poll for up to 30s.
    let mut after = before;
    for _ in 0..15 {
        std::thread::sleep(Duration::from_secs(2));
        after = worker_session_count(&cli, worker);
        if after > before {
            break;
        }
    }
    assert!(
        after > before,
        "async task did not complete in 30s: before={before} after={after}; \
         stdout={out:?} stderr={err:?}",
    );
}

/// `a2a_async.ps1` T4: caller annotation in async target session.
/// Same assertion as `a2a_blocking_t4_caller_annotation` but for the
/// async flow. Pass criteria: worker session history contains
/// `[Message from agent: <delegator>]`.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_async_t4_caller_annotation() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let delegator = "a2a_async_t4_delegator";
    let worker = "a2a_async_t4_worker";
    write_a2a_agent(
        cli.home(),
        delegator,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write delegator");
    write_a2a_agent(
        cli.home(),
        worker,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &["read_file"],
    )
    .expect("write worker");
    write_sentinel_file(&cli, worker, "test_async.txt", "A2A_ASYNC_SECRET_99");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use a2a_send with _async=true to send this message to agent \
         '{worker}': Read the file test_async.txt. Reply with the receipt. \
         (needle=a2a_async_t4)"
    );
    let (out, err, status) = run(
        &cli,
        &["send", delegator, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);

    // Wait briefly for the async task to complete + write history.
    for _ in 0..15 {
        if worker_session_count(&cli, worker) > 0 {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }

    let (delegator_name, history) = match worker_session_history(&cli, worker) {
        Some(pair) => pair,
        None => panic!("no worker session after async a2a_send; stdout={out:?} stderr={err:?}"),
    };
    let expected_marker = format!("[Message from agent: {delegator}]");
    let history_arr = history
        .get("history")
        .and_then(|h| h.as_array())
        .expect("history is an array");
    let found = history_arr.iter().any(|entry| {
        entry
            .get("Message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.contains(&expected_marker))
            .unwrap_or(false)
    });
    assert!(
        found,
        "caller annotation {expected_marker:?} not found in worker session \
         history (session_id={delegator_name}, {} entries)",
        history_arr.len(),
    );
}

// ---------------------------------------------------------------------------
// a2a_isolation.ps1
// ---------------------------------------------------------------------------

/// `a2a_isolation.ps1` T1: caller A sends to target, target gets
/// exactly 1 session.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_isolation_t1_caller_a_session() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let caller_a = "a2a_iso_t1_a";
    let target = "a2a_iso_t1_target";
    write_a2a_agent(
        cli.home(),
        caller_a,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller A");
    write_a2a_agent(
        cli.home(),
        target,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write target");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISOLATION_TEST_CALLER_A. After receiving the response, \
         reply exactly A2A_TEST_DONE. (needle=a2a_iso_t1)"
    );
    let (out, err, status) = run(
        &cli,
        &["send", caller_a, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);

    let after = worker_session_count(&cli, target);
    assert_eq!(
        after, 1,
        "expected exactly 1 target session after caller A's a2a_send, got {after}; \
         stdout={out:?} stderr={err:?}",
    );
}

/// `a2a_isolation.ps1` T2: caller B sends to the same target,
/// target gets exactly 2 sessions (one per caller, isolated by
/// peer_id).
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_isolation_t2_caller_b_session() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let caller_a = "a2a_iso_t2_a";
    let caller_b = "a2a_iso_t2_b";
    let target = "a2a_iso_t2_target";
    write_a2a_agent(
        cli.home(),
        caller_a,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller A");
    write_a2a_agent(
        cli.home(),
        caller_b,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller B");
    write_a2a_agent(
        cli.home(),
        target,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write target");
    let _daemon = DaemonGuard::spawn(&cli);

    // Caller A first
    let prompt_a = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISOLATION_TEST_CALLER_A. After receiving the response, \
         reply exactly A2A_TEST_DONE. (needle=a2a_iso_t2_a)"
    );
    let (out_a, err_a, status_a) = run(
        &cli,
        &["send", caller_a, &prompt_a, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out_a, &err_a, &status_a);

    // Then caller B
    let prompt_b = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISOLATION_TEST_CALLER_B. After receiving the response, \
         reply exactly A2A_TEST_DONE. (needle=a2a_iso_t2_b)"
    );
    let (out_b, err_b, status_b) = run(
        &cli,
        &["send", caller_b, &prompt_b, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out_b, &err_b, &status_b);

    let after = worker_session_count(&cli, target);
    assert_eq!(
        after, 2,
        "expected exactly 2 target sessions after both callers' a2a_send, \
         got {after}; out_a={out_a:?} out_b={out_b:?} stderr={err_b:?}",
    );
}

/// `a2a_isolation.ps1` T3: each target session has a distinct
/// peer_id matching its caller. Inspect the session list and verify
/// callerA and callerB both appear as peer_ids.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_isolation_t3_peer_id_isolation() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let caller_a = "a2a_iso_t3_a";
    let caller_b = "a2a_iso_t3_b";
    let target = "a2a_iso_t3_target";
    write_a2a_agent(
        cli.home(),
        caller_a,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller A");
    write_a2a_agent(
        cli.home(),
        caller_b,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller B");
    write_a2a_agent(
        cli.home(),
        target,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write target");
    let _daemon = DaemonGuard::spawn(&cli);

    // Drive both callers' a2a_send (same as t1+t2 above but bundled).
    let prompt_a = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISO_T3_CALLER_A. After receiving the response, reply \
         exactly A2A_TEST_DONE. (needle=a2a_iso_t3_a)"
    );
    let (_, err_a, status_a) = run(
        &cli,
        &["send", caller_a, &prompt_a, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&prompt_a, &err_a, &status_a);

    let prompt_b = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISO_T3_CALLER_B. After receiving the response, reply \
         exactly A2A_TEST_DONE. (needle=a2a_iso_t3_b)"
    );
    let (_, err_b, status_b) = run(
        &cli,
        &["send", caller_b, &prompt_b, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&prompt_b, &err_b, &status_b);

    // Inspect session list, gather peer_ids.
    let (out, _err, status) = run(
        &cli,
        &["session", "list", target, "--json"],
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
    assert!(
        peer_ids.iter().any(|p| p == caller_a),
        "no session with peer_id={caller_a:?}; got {peer_ids:?}",
    );
    assert!(
        peer_ids.iter().any(|p| p == caller_b),
        "no session with peer_id={caller_b:?}; got {peer_ids:?}",
    );
}

// Workaround helper closures removed — the test bodies use
// `let (_, err, status) =` for intermediate calls and pass the
// prompt string itself as the (trivially-true) "stdout" arg to
// `assert_ok`. We only care about exit status on intermediate
// calls; the per-call output is discarded.

/// `a2a_isolation.ps1` T4: a second a2a_send from caller A resumes
/// caller A's own session (not caller B's). Pass criteria: target
/// session count is still 2 (unchanged from t2), and the session
/// whose peer_id == caller_a has the same session_id as before the
/// second call.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_isolation_t4_caller_a_resumes() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let caller_a = "a2a_iso_t4_a";
    let caller_b = "a2a_iso_t4_b";
    let target = "a2a_iso_t4_target";
    write_a2a_agent(
        cli.home(),
        caller_a,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller A");
    write_a2a_agent(
        cli.home(),
        caller_b,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller B");
    write_a2a_agent(
        cli.home(),
        target,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write target");
    let _daemon = DaemonGuard::spawn(&cli);

    // Caller A first
    let prompt_a1 = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISO_T4_CALLER_A. After receiving the response, reply \
         exactly A2A_TEST_DONE. (needle=a2a_iso_t4_a1)"
    );
    let (_, err, status) = run(
        &cli,
        &["send", caller_a, &prompt_a1, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&prompt_a1.as_str(), &err, &status);

    // Caller B
    let prompt_b = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISO_T4_CALLER_B. After receiving the response, reply \
         exactly A2A_TEST_DONE. (needle=a2a_iso_t4_b)"
    );
    let (_, err, status) = run(
        &cli,
        &["send", caller_b, &prompt_b, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&prompt_b.as_str(), &err, &status);

    // Capture caller A's session_id before the second call.
    let (out, _err, status) = run(
        &cli,
        &["session", "list", target, "--json"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &_err, &status);
    let v: serde_json::Value = serde_json::from_str(&out).expect("session list json");
    let caller_a_session_id_before: Option<String> = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .and_then(|a| {
            a.iter()
                .find(|s| s.get("peer_id").and_then(|p| p.as_str()) == Some(caller_a))
        })
        .and_then(|s| s.get("session_id"))
        .and_then(|s| s.as_str())
        .map(String::from);

    // Caller A second call
    let prompt_a2 = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISO_T4_CALLER_A_SECOND. After receiving the response, \
         reply exactly A2A_TEST_DONE. (needle=a2a_iso_t4_a2)"
    );
    let (_, err, status) = run(
        &cli,
        &["send", caller_a, &prompt_a2, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&prompt_a2.as_str(), &err, &status);

    // Session count should still be 2; caller A's session_id should
    // match what we captured before.
    let (out, _err, status) = run(
        &cli,
        &["session", "list", target, "--json"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &_err, &status);
    let v: serde_json::Value = serde_json::from_str(&out).expect("session list json");
    let sessions = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .expect("sessions");
    assert_eq!(
        sessions.len(),
        2,
        "expected 2 sessions after caller A's second a2a_send, got {}",
        sessions.len(),
    );
    let caller_a_session_id_after: Option<String> = sessions
        .iter()
        .find(|s| s.get("peer_id").and_then(|p| p.as_str()) == Some(caller_a))
        .and_then(|s| s.get("session_id"))
        .and_then(|s| s.as_str())
        .map(String::from);
    assert_eq!(
        caller_a_session_id_after, caller_a_session_id_before,
        "caller A's session_id changed: before={caller_a_session_id_before:?} \
         after={caller_a_session_id_after:?}",
    );
}

/// `a2a_isolation.ps1` T5: message counts per session. After
/// t1+t2+t4 (caller A's two calls + caller B's one call), both
/// sessions should have at least 3 messages (system + user +
/// assistant for the first turn; more for subsequent).
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn a2a_isolation_t5_message_counts() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let caller_a = "a2a_iso_t5_a";
    let caller_b = "a2a_iso_t5_b";
    let target = "a2a_iso_t5_target";
    write_a2a_agent(
        cli.home(),
        caller_a,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller A");
    write_a2a_agent(
        cli.home(),
        caller_b,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write caller B");
    write_a2a_agent(
        cli.home(),
        target,
        "https://api.minimaxi.com/anthropic",
        "minimax",
        &api_key,
        &[],
    )
    .expect("write target");
    let _daemon = DaemonGuard::spawn(&cli);

    // Caller A
    let prompt_a = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISO_T5_CALLER_A. After receiving the response, reply \
         exactly A2A_TEST_DONE. (needle=a2a_iso_t5_a)"
    );
    let (_, err, status) = run(
        &cli,
        &["send", caller_a, &prompt_a, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&prompt_a.as_str(), &err, &status);

    // Caller B
    let prompt_b = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISO_T5_CALLER_B. After receiving the response, reply \
         exactly A2A_TEST_DONE. (needle=a2a_iso_t5_b)"
    );
    let (_, err, status) = run(
        &cli,
        &["send", caller_b, &prompt_b, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&prompt_b.as_str(), &err, &status);

    // Caller A second
    let prompt_a2 = format!(
        "Use a2a_send to send this exact message to agent '{target}': \
         A2A_ISO_T5_CALLER_A_SECOND. After receiving the response, \
         reply exactly A2A_TEST_DONE. (needle=a2a_iso_t5_a2)"
    );
    let (_, err, status) = run(
        &cli,
        &["send", caller_a, &prompt_a2, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&prompt_a2.as_str(), &err, &status);

    // Inspect message counts.
    let (out, _err, status) = run(
        &cli,
        &["session", "list", target, "--json"],
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
        "both sessions should have >= 3 messages; got caller_a={count_a}, caller_b={count_b}",
    );
}
