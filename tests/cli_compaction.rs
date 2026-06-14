//! CLI integration tests for `peko session compact` (Phase B slice
//! per `docs/integration/TESTING.md` §7).
//!
//! **Smoke test scope.** This file ships a single test that exercises
//! the `--dry-run --json` fix in
//! [`src/commands/session.rs:286-356`](../../pekobot/peko-runtime/src/commands/session.rs#L286-L356).
//! The original `e2e_tests/compaction/{cli,extension}.ps1` scripts
//! cover 6 scenarios end-to-end (dry-run, actual compact, context
//! cache, post-compact usable, custom instruction, multi-compaction,
//! plus the extension test), but they require multi-turn mock-LLM
//! setup that interacts in subtle ways with the session indexer — the
//! first user message of the conversation is what the mock keys on
//! (not the latest), and populating the agent's active session with
//! a real message stream needs more investigation than fits in this
//! PR. The PS scripts that target this surface stay in
//! `e2e_tests/compaction/` for now and are documented in TESTING.md
//! §7 as deferred to a follow-up PR.
//!
//! **What this test does prove.** The `--dry-run --json` wire shape
//! that the PS script's TEST 1 expects — specifically the
//! `dry_run: true` flag and the `DryRunReport` fields
//! (`estimated_tokens`, `context_window`, `percent`, `message_count`,
//! `messages_to_compact`) — which is the actual CLI fix this PR
//! ships. Pre-fix, `--dry-run --json` fell into the text-render path
//! and emitted no JSON at all, so any test that asserts on JSON would
//! fail at parse time.
//!
//! **Tier:** mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set). The test early-returns if unset so
//! `cargo test` still passes on a bare checkout.
//!
//! **`#[serial]`.** Currently this file has one test, but the
//! attribute is set in case future tests are added. The mock's
//! per-substring counter is global state across all test binaries.

mod common;
use common::{configure_mock, run_with_timeout, DaemonGuard, PekoCli};
use serial_test::serial;
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MOCK_LLM_URL` and return Some(url) if set, None otherwise.
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

/// Write a mock-LLM-pointed agent. Tool enablement isn't exercised by
/// the smoke test, but we keep the same shape as the other CLI test
/// files (write_file + read_file + shell + canonical IDs) so a
/// future extension of this file to add real-compaction tests doesn't
/// have to refactor the helper.
fn write_compaction_agent(
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
description = "CLI integration test agent for session compaction"
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
    "shell",
    "read_file",
    "write_file",
    "builtin:tool:shell",
    "builtin:tool:read_file",
    "builtin:tool:write_file",
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
        "Test agent for the session-compaction CLI integration suite. \
         Has write_file, read_file, and shell tools enabled.",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `compaction_cli.ps1` T1 (smoke): `peko session compact --dry-run --json`
/// emits the JSON shape the PS test expects. Asserts the presence of
/// the `dry_run: true` flag and the `DryRunReport` fields — the actual
/// CLI fix this PR ships. Pre-fix this assertion would fail at parse
/// time (no JSON was emitted). See the module doc-comment for why this
/// is a smoke test rather than a full T1-T6 + extension migration.
///
/// We do one `peko send` first to create a session (otherwise the
/// daemon refuses with "No active session for agent …"). The single
/// `peko send` is scripted to a 1-element mock-LLM sequence: a
/// tool_call(write_file) + (the mock clamps to the last element on
/// re-call, but we only call once so the second LLM call after the
/// tool dispatch returns the same tool_call shape — we don't care
/// about the post-tool LLM response because the test only needs
/// the `peko send` to complete so the session exists).
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn cli_compact_dry_run_json_reports_metadata() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();
    let agent_name = "cli_compact_dry_run";
    let needle = "cli-compact-dryjson-p4d7";

    // 1-element script: the first LLM call gets a tool_call, and any
    // subsequent LLM calls (the agent's re-prompt after the tool
    // dispatch) clamp to the last element — the same tool_call.
    // We don't care what the second LLM call returns; the parent
    // just needs to get a response so `peko send` returns and the
    // session becomes "active".
    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "write_file", "arguments":
                serde_json::json!({ "path": "warmup.txt", "content": "WARMUP" }).to_string()
            } },
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_compaction_agent(cli.home(), agent_name, &mock_url)
        .expect("write compaction agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // 1 `peko send` to create an active session.
    let warmup_prompt = format!(
        "Use the needle '{needle}' to write a file warmup.txt and respond WARMUP_DONE."
    );
    let (_out, _err, _status) = run(
        &cli,
        &["send", agent_name, &warmup_prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    // Don't assert on the WARMUP_DONE sentinel — the parent may bail
    // before reaching the post-tool LLM call. We just need the session
    // to exist; dry-run below will tell us if it does.

    // Run dry-run JSON.
    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--dry-run", "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    let parsed: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("dry-run --json parse error: {e}\nstdout: {out}"));

    // Verify the new JSON shape (added by the CLI fix).
    assert_eq!(parsed["success"], serde_json::Value::Bool(true));
    assert_eq!(
        parsed["dry_run"],
        serde_json::Value::Bool(true),
        "dry_run field should be present and true (added by --dry-run --json fix)"
    );
    for field in &[
        "estimated_tokens",
        "context_window",
        "percent",
        "message_count",
        "messages_to_compact",
    ] {
        assert!(
            parsed.get(field).is_some(),
            "{field} field should be present in dry-run --json output"
        );
    }
}
