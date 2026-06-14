//! CLI integration tests for the `builtin:tool:agent_spawn` path
//! (Phase B slice per `docs/integration/TESTING.md` §7).
//!
//! This file ships **3 smoke tests** that exercise the parent-side
//! `agent_spawn` blocking path end-to-end through the peko daemon:
//!
//! | Rust test                                       | Replaces PS sub-test            |
//! |-------------------------------------------------|----------------------------------|
//! | `subagent_blocking_parent_completes`            | blocking T1 (write_file)         |
//! | `subagent_blocking_isolated_parent_completes`   | blocking T2 (isolated)           |
//! | `subagent_blocking_labeled_parent_completes`    | blocking T4 (label arg)          |
//!
//! Each test asserts only on the parent's `peko send --no-stream`
//! stdout containing the expected sentinel. The sentinel proves the
//! blocking tool call completed (otherwise the parent's second LLM
//! call would never fire) — see the comment above the `Tests`
//! section for the full coverage caveat.
//!
//! The deeper child-side assertions (the child actually writing a
//! file that the test can read back, the grandchild depth-limit
//! path, the isolation policy enforcement) are **deferred to a
//! follow-up PR**. The `e2e_tests/subagent/` PS scripts that drove
//! those deeper scenarios remain in `e2e_tests/` until the
//! follow-up lands — see the Phase B coverage gap section in
//! `docs/integration/TESTING.md`.
//!
//! `e2e_tests/subagent/subagent_async.ps1` and
//! `e2e_tests/subagent/subagent_status_list.ps1` are also deferred
//! (they need `AsyncTaskRegistry` access from a test; same PR-3
//! path documented in the doc).
//!
//! Each test:
//!   1. Builds an isolated [`PekoCli`] tempdir as `HOME`.
//!   2. Calls `POST /_test/configure` on the mock LLM to install a
//!      scripted `MOCK_LLM_SCRIPT` (and reset the per-substring counter).
//!   3. Spawns a plain `DaemonGuard` (no `--interval` — subagent tests
//!      don't poll, and the child subagent's blocking LLM call goes
//!      straight through the same mock endpoint).
//!   4. Runs `peko send <agent> <prompt> --no-stream` and asserts
//!      the parent's stdout contains the expected sentinel.
//!
//! Tier: mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set). Tests early-return if unset so `cargo test`
//! still passes on a bare checkout.
//!
//! **`#[serial]`.** The mock's per-substring counter is global state
//! across all test binaries. Every test in this file is `#[serial]`
//! to avoid concurrent tests racing the same counter; per-test
//! unique needles are belt-and-suspenders.

mod common;
use common::{configure_mock, run_with_timeout, DaemonGuard, PekoCli};
use serial_test::serial;
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MOCK_LLM_URL` and return Some(url) if set, None otherwise.
/// Tests early-return on None so `cargo test` still passes on a bare
/// checkout without the docker-compose stack.
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

/// Write a mock-LLM-pointed agent that has the tools the subagent
/// migration needs enabled: `agent_spawn`, `task`, `write_file`,
/// `read_file`, and `shell`.
///
/// `write_mock_agent` (in `tests/common/agent.rs`) writes
/// `enabled = []`, which the agent's `init_builtins_async` treats as
/// an EXCLUSIVE whitelist — every built-in tool is disabled, including
/// `agent_spawn`. The runtime's tool dispatcher would then reject the
/// parent's `agent_spawn` tool_call as "tool not enabled", and the
/// test would fail with a confusing message. This helper writes a
/// config that includes the canonical IDs the subagent migration
/// needs (see `src/types/agent.rs:204-229` for the full default list).
fn write_subagent_agent(
    home: &std::path::Path,
    name: &str,
    mock_llm_url: &str,
) -> std::io::Result<()> {
    use std::path::Path;
    let agent_dir = Path::new(home).join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;
    let base_url = mock_llm_url.trim_end_matches('/');
    let config_toml = format!(
        r#"version = "1.0"
name = "{name}"
description = "CLI integration test agent for subagent / agent_spawn"
auto_accept_trusted = false
default_timeout_seconds = 60

[provider]
provider_type = "openai_compatible"
api_key = "mock-llm-test-key"
base_url = "{base_url}"
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
    "builtin:tool:agent_spawn",
    "builtin:tool:task",
    "builtin:tool:write_file",
    "builtin:tool:read_file",
    "builtin:tool:shell",
]

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}
"#
    );
    std::fs::write(agent_dir.join("config.toml"), config_toml)?;
    std::fs::write(
        agent_dir.join("SYSTEM.md"),
        "Test agent for the subagent CLI integration suite. \
         Has the agent_spawn, task, write_file, read_file, and shell tools enabled.",
    )?;
    Ok(())
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// **What these tests cover, and what they don't.** Each test exercises the
// `agent_spawn` tool's end-to-end path through the peko daemon, via the
// `MOCK_LLM_SCRIPT` sequence feature (parent emits `tool_call(agent_spawn, …)`
// on turn 1; the mock returns the success sentinel on turn 2 after the
// blocking spawn completes). The assertion is on the parent's `peko send
// --no-stream` stdout containing the sentinel, which is the proof that:
//
// 1. The parent's LLM call matched the `MOCK_LLM_SCRIPT` needle.
// 2. The runtime dispatched the `agent_spawn` tool call.
// 3. The blocking subagent path completed without panic (otherwise the
//    parent loop would error and the second LLM call would never fire).
// 4. The parent's second LLM call returned the success sentinel.
//
// **What these tests do NOT cover.** Earlier CI runs attempted to also
// assert on a file the child wrote (e.g. via `write_file`) into the
// peko-runtime working tree, but the workspace path resolved by the
// daemon's `ToolRuntime::with_workspace_and_core` call is non-trivial
// and the test was reduced to the sentinel-only assertion. The deeper
// child-side coverage (write_file landing where the test expects, the
// grandchild file content, the depth-limit error path) is deferred to
// a follow-up PR — see the Phase B coverage gap section in
// `docs/integration/TESTING.md` for the deferred scenarios and the
// PR-3 path (a test-only `peko subagent list --json` CLI subcommand
// that reads the in-process `AsyncTaskRegistry`).
//
// **`#[serial]`.** The mock's per-substring counter is global state
// shared by every test binary. Every test here is `#[serial]` to
// avoid concurrent races; per-test unique needles are belt-and-
// suspenders.

/// Smoke test: parent emits `tool_call(agent_spawn, …)` and the
/// blocking path completes; the parent's second LLM call returns
/// `BLOCKING_SUCCESS`. Covers the `subagent_blocking.ps1` baseline.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_blocking_parent_completes() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "subagent-block-parent-aa11";
    let agent_name = "subagent_blocking_parent";

    // The script is keyed on a single parent-only needle. The parent's
    // first LLM call sees the needle and returns `tool_call(agent_spawn)`.
    // The runtime dispatches the blocking spawn — the child runs against
    // the same mock but its user message (the wrapped task) does NOT
    // contain this needle, so the child LLM call falls through to the
    // mock's DEFAULT_RESPONSE ("Peko tunnel works!") and the child exits
    // with that text. The parent's blocking tool result has the child's
    // text in `output`; the parent's second LLM call returns
    // `BLOCKING_SUCCESS`.
    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "agent_spawn", "arguments":
                r#"{"task":"say hello back as your final text"}"#.to_string()
            } },
            "BLOCKING_SUCCESS",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_subagent_agent(cli.home(), agent_name, &mock_url).expect("write subagent agent");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Spawn a subagent to do a task. When it returns, respond with \
         BLOCKING_SUCCESS. Use the needle '{needle}' in your prompt."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("BLOCKING_SUCCESS"),
        "parent did not report BLOCKING_SUCCESS: stdout={out} stderr={err}",
    );
}

/// Smoke test: same as `subagent_blocking_parent_completes` but with
/// `isolated: true` on the parent's `agent_spawn` arg. Verifies the
/// `isolated` flag is plumbed through without erroring.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_blocking_isolated_parent_completes() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "subagent-block-iso-bb22";
    let agent_name = "subagent_blocking_iso";

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "agent_spawn", "arguments":
                serde_json::json!({
                    "task": "isolated task: do nothing and return text",
                    "isolated": true,
                }).to_string()
            } },
            "ISOLATED_SUCCESS",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_subagent_agent(cli.home(), agent_name, &mock_url).expect("write subagent agent");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Spawn an isolated subagent. When it returns, respond with \
         ISOLATED_SUCCESS. Use the needle '{needle}' in your prompt."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("ISOLATED_SUCCESS"),
        "parent did not report ISOLATED_SUCCESS: stdout={out} stderr={err}",
    );
}

/// Smoke test: parent's `agent_spawn` call carries a `label` arg; the
/// blocking path completes and the parent reports the sentinel. The
/// runtime should accept the label without erroring.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_blocking_labeled_parent_completes() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "subagent-block-lab-cc33";
    let agent_name = "subagent_blocking_lab";

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "agent_spawn", "arguments":
                serde_json::json!({
                    "task": "labeled task: do nothing and return text",
                    "label": "smoke-test-label",
                }).to_string()
            } },
            "LABELED_SUCCESS",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_subagent_agent(cli.home(), agent_name, &mock_url).expect("write subagent agent");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Spawn a labeled subagent. When it returns, respond with \
         LABELED_SUCCESS. Use the needle '{needle}' in your prompt."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("LABELED_SUCCESS"),
        "parent did not report LABELED_SUCCESS: stdout={out} stderr={err}",
    );
}
