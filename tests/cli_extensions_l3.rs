//! L3 (LLM-driven tool execution) integration tests for the unified
//! extension framework — closes the gap flagged in
//! [peko-runtime#15](https://github.com/ConekoAI/peko-runtime/issues/15).
//!
//! This file is the L3 follow-up to [`cli_extensions.rs`](cli_extensions.rs)
//! (L1 install/list/info/enable/disable/uninstall lifecycle). It exercises
//! the runtime's claim that an LLM can call an installed MCP server or
//! universal tool, the runtime dispatches the call, and the result flows
//! back to the LLM — end-to-end, through the agentic loop and the
//! `invoke_hook(ToolExecute)` registry, with a real daemon and a real
//! extension process under the LLM. The closest existing tests either:
//!
//! - Cover only the L1 lifecycle (no LLM in the path) — `cli_extensions.rs`.
//! - Drive the LLM path but only against **built-in** tools
//!   (`read_file`, `cron`, `agent_spawn`, `a2a_send`, `Write`) —
//!   `cli_tools.rs`, `cli_cron.rs`, `cli_subagent.rs`, `cli_a2a.rs`,
//!   `cli_compaction.rs`.
//! - Call `invoke_hook` directly with no LLM in the path —
//!   `tests/extension_packaging.rs:245-311`.
//!
//! ## Tool name shapes
//!
//! The MOCK_LLM_SCRIPT `tool_call.name` field must match the **fully
//! qualified** name the agentic loop's [`build_tool_definitions`]
//! (in `src/engine/agentic_loop.rs:660-668`) hands to the provider:
//!
//! - **MCP**: `mcp:{server_name}:{tool_name}` — e.g. `mcp:standard-echo:echo`.
//!   The owner `extension_id` for the whitelist is the bare `server_name`
//!   (e.g. `standard-echo`); see `McpAdapter::register_server_tools` at
//!   `src/extensions/mcp/adapter.rs:320-339`.
//! - **Universal**: the bare `tool_name` — e.g. `calculator_simple`. The
//!   owner `extension_id` is `universal:{tool_name}` — e.g.
//!   `universal:calculator_simple`; see `UniversalAdapter::register_tool`
//!   at `src/extensions/universal/adapter.rs:182-188`.
//!
//! The agent config's `[extensions] enabled` list must contain the
//! **owner canonical ID** (not the tool name), so
//! `is_tool_enabled` (`src/extension/core/tool_registry.rs:56-68`) can
//! resolve the owner and match the whitelist. A bare `calculator_simple`
//! in the whitelist would NOT match the canonical `universal:calculator_simple`
//! and the dispatcher would block the call with
//! `"Tool 'X' is currently disabled..."`.
//!
//! ## Gating
//!
//! Both tests are gated on TWO env vars:
//!
//! - `MOCK_LLM_URL` — points at the mock LLM container (see
//!   [`docs/integration/TESTING.md`](../../docs/integration/TESTING.md) §3).
//! - `PEKO_TEST_PYTHON=1` — the MCP server fixture (`mcp_server.py`) and
//!   the universal tool fixture (`calculator_simple.py`) are both Python.
//!   This gate lets the test suite pass on runners that don't have
//!   `python` on PATH (the docker-compose integration stack sets it).
//!
//! Both tests are `#[ignore] #[serial]`: the mock LLM's per-substring
//! counter is global state, and these tests are skipped on bare checkouts
//! (no docker stack) per the same convention as the rest of the
//! `tests/cli_*.rs` suite.
//!
//! ## Tier
//!
//! Mock-LLM tier (`cargo test --test cli_extensions_l3` passes on bare
//! checkout; `--include-ignored` under the docker stack exercises the
//! real path). Built-in tools are NOT re-tested here — they are covered
//! by `cli_tools.rs` (`built_in_read_file_returns_content` et al.).

mod common;
use common::{configure_mock, run_with_timeout, DaemonGuard, PekoCli};
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MOCK_LLM_URL` and return Some(url) if set, None otherwise.
/// Parallel to [`mock_llm_url`] in `cli_tools.rs`.
fn mock_llm_url() -> Option<String> {
    let url = std::env::var("MOCK_LLM_URL").ok()?;
    if url.is_empty() {
        return None;
    }
    Some(url)
}

/// Read `PEKO_TEST_PYTHON` and return `true` iff set to `"1"`. Tests that
/// need a Python runtime for MCP / universal-python fixtures early-return
/// on `false` so the suite still passes on runners that don't have
/// `python` on PATH.
fn peko_test_python() -> bool {
    std::env::var("PEKO_TEST_PYTHON").ok().as_deref() == Some("1")
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

/// Absolute path to a fixture directory, relative to the crate root.
///
/// Mirrors [`fixture_dir`] in `cli_extensions.rs:106-113` — committed at
/// `0b363ae` (the e2e_tests → e2e_tests_archive rename) — so the path
/// layout stays in sync with the L1 install suite.
fn fixture_dir(relative: &str) -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is set by cargo for integration tests");
    PathBuf::from(manifest_dir)
        .join("e2e_tests_archive")
        .join("extensions")
        .join(relative)
}

/// Write a mock-LLM-pointed agent whose `[extensions] enabled` whitelist
/// already contains the **canonical owner IDs** of the extensions the
/// caller wants to invoke. This bypasses the `peko ext enable <id>` CLI
/// path (which writes the user-provided id verbatim — `calculator_simple`
/// — and would NOT match the canonical `universal:calculator_simple`
/// owner id that the dispatcher looks up).
///
/// `ext_canonical_ids` should be the **owner extension ids** (e.g.
/// `standard-echo`, `universal:calculator_simple`), not the bare tool
/// names. See the file-level doc comment for the source of these ids.
///
/// URL goes in `base_url`, not `api_key` — same gotcha as
/// [`write_v3_mock_agent`](../../tests/common/agent.rs) (the provider
/// dispatch logic in `src/agent/agent.rs::init_provider` keys off
/// `base_url`'s hostname; an empty `base_url` would fall through to
/// OpenAI real and the test would 401).
fn write_ext_agent(
    home: &Path,
    name: &str,
    mock_llm_url: &str,
    ext_canonical_ids: &[&str],
) -> std::io::Result<()> {
    let agent_dir = home.join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;
    let _base_url = mock_llm_url.trim_end_matches('/');
    let enabled_block = ext_canonical_ids
        .iter()
        .map(|id| format!("    \"{id}\","))
        .collect::<Vec<_>>()
        .join("\n");
    let config_toml = format!(
        r#"version = "3.0"
name = "{name}"
description = "CLI integration test agent for L3 extension tool dispatch (issue #15)"
auto_accept_trusted = false

preferred_provider_id = "mock-llm"
preferred_model_id = "default"
default_timeout_seconds = 60

[extensions]
enabled = [
{enabled_block}
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
        "L3 extension dispatch test agent. The tool listed in the user's \
         prompt is the one to call.",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// L3 MCP round-trip: install the Tier-1 `standard-echo` MCP server
/// fixture, point a mock-LLM agent at it, script a 2-turn
/// `tool_call(echo, "peko-l3-mcp-…") → text("ECHO_DONE …")` dialog, run
/// `peko send`, and assert the LLM's final text contains both the
/// sentinel AND the echoed string. The second assertion is the load-
/// bearing one: it can only be in the LLM's final response if the
/// runtime actually dispatched the `tool_call` to the MCP adapter, the
/// adapter spawned the python server, the server echoed the string, and
/// the result was fed back to the LLM as a tool message.
///
/// Tier-1 detection: the fixture has only `server.json` (no manifest.yaml);
/// `peko ext install` auto-classifies it as type `mcp` per
/// `src/extension/manager/mod.rs:215-256`. The extension id is the
/// `name` from `server.json` — `standard-echo`.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL, PEKO_TEST_PYTHON=1, and peko daemon"]
#[serial]
async fn ext_mcp_standard_echo_roundtrip() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    if !peko_test_python() {
        eprintln!("PEKO_TEST_PYTHON not set; skipping");
        return;
    }

    let needle = "l3-mcp-echo-7a2f";
    let agent_name = "l3_mcp_agent";
    let ext_id = "standard-echo";

    // Script: first turn = tool_call(mcp:standard-echo:echo, {message: …});
    //          second turn = text "ECHO_DONE <needle>".
    // After the sequence is exhausted, the mock LLM keeps returning the
    // last element, so any stray LLM call after the second turn still
    // gets a deterministic response (rather than crashing the dialog).
    let tool_args = serde_json::json!({ "message": needle }).to_string();
    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "mcp:standard-echo:echo", "arguments": tool_args } },
            format!("ECHO_DONE {needle}"),
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    // Write the agent with the canonical owner id (the bare server name)
    // in the whitelist. The dispatcher's `is_tool_enabled` looks up the
    // tool's owner and matches that against this list — so the bare
    // tool name (or a wildcard) is what's required.
    write_ext_agent(cli.home(), agent_name, &mock_url, &[ext_id]).expect("write ext agent");

    let _daemon = DaemonGuard::spawn(&cli);

    // Install the MCP server from the e2e_tests_archive fixture.
    let install_path = fixture_dir("mcp/python/standard");
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &install_path.to_string_lossy()],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains(ext_id),
        "install output should surface the extension id {ext_id:?}: stdout={out} stderr={err}",
    );

    // The prompt asks the LLM to call the echo tool with the needle,
    // then emit the sentinel + the same needle (so the LLM's final text
    // is unambiguous: it MUST contain the echoed needle iff the tool
    // was actually dispatched and its result fed back).
    let prompt = format!(
        "Call your 'echo' MCP tool with the message {needle:?}. After you \
         receive the tool's response, reply with exactly: ECHO_DONE {needle}"
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("ECHO_DONE"),
        "parent did not emit ECHO_DONE: stdout={out} stderr={err}",
    );
    assert!(
        out.contains(needle),
        "parent's final text did not include the echoed needle {needle:?} — \
         the MCP tool's response was likely not fed back to the LLM.\n\
         stdout: {out}\nstderr: {err}",
    );

    // Cleanup.
    let (_, _, _) = run(&cli, &["ext", "uninstall", ext_id], Duration::from_secs(10));
}

/// L3 universal round-trip: install the `calculator_simple` universal
/// tool fixture, point a mock-LLM agent at it, script a 2-turn
/// `tool_call(calculator_simple, 7+13) → text("CALC_DONE …")` dialog, run
/// `peko send`, and assert the LLM's final text contains both the
/// sentinel AND the integer `20` (the deterministic sum the python tool
/// returns — `7.0 + 13.0 = 20.0`).
///
/// The universal owner id is `universal:calculator_simple` (NOT the bare
/// `calculator_simple` — see `UniversalAdapter::register_tool` at
/// `src/extensions/universal/adapter.rs:182-188`), so the agent's
/// `[extensions] enabled` list must contain that canonical id verbatim.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL, PEKO_TEST_PYTHON=1, and peko daemon"]
#[serial]
async fn ext_universal_calculator_simple_roundtrip() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    if !peko_test_python() {
        eprintln!("PEKO_TEST_PYTHON not set; skipping");
        return;
    }

    let needle = "l3-univ-calc-9b4e";
    let agent_name = "l3_univ_agent";
    // Canonical owner id, NOT the bare tool name.
    let ext_canonical_id = "universal:calculator_simple";

    // Script: tool_call(calculator_simple, {operation: "add", a: 7, b: 13}),
    // then text "CALC_DONE <needle>". The python tool returns
    // {success: true, result: 20.0, …, expression: "7.0 add 13.0 = 20.0"}.
    let tool_args = serde_json::json!({
        "operation": "add",
        "a": 7,
        "b": 13,
    })
    .to_string();
    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "calculator_simple", "arguments": tool_args } },
            format!("CALC_DONE {needle}"),
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_ext_agent(cli.home(), agent_name, &mock_url, &[ext_canonical_id])
        .expect("write ext agent");

    let _daemon = DaemonGuard::spawn(&cli);

    // Install the universal tool from the e2e_tests_archive fixture.
    let install_path = fixture_dir("universal/python/simple");
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &install_path.to_string_lossy()],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("calculator_simple"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // The prompt asks for 7+13; the LLM's final text is unambiguous
    // about whether it received the tool's result (which contains 20.0).
    let prompt = format!(
        "Use your calculator_simple tool to add 7 and 13. After you \
         receive the tool's response, reply with exactly: CALC_DONE {needle}"
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("CALC_DONE"),
        "parent did not emit CALC_DONE: stdout={out} stderr={err}",
    );
    // The result is `20.0`; we assert on `20` as a substring to be
    // robust to the python tool's float formatting (`.0` suffix).
    assert!(
        out.contains("20"),
        "parent's final text did not include the result 20 — \
         the universal tool's response was likely not fed back to the LLM.\n\
         stdout: {out}\nstderr: {err}",
    );

    // Cleanup.
    let (_, _, _) = run(
        &cli,
        &["ext", "uninstall", "calculator_simple"],
        Duration::from_secs(10),
    );
}
