//! CLI integration tests for `peko session compact` (Phase B slice
//! per `docs/integration/TESTING.md` §7).
//!
//! Migrates `e2e_tests/compaction/{cli,extension}.ps1` to Rust tests
//! gated on `MOCK_LLM_URL`. Coverage:
//!
//! | PS test                            | Rust test                                                       |
//! |------------------------------------|-----------------------------------------------------------------|
//! | `compaction_cli.ps1` T1 (dry-run)  | `cli_compact_dry_run_json_reports_metadata` (smoke)             |
//! | `compaction_cli.ps1` T1 (multi)    | `cli_compact_dry_run_json_reports_message_counts_after_multi_turn` (Issue 030 regression) |
//! | `compaction_cli.ps1` T2 (real)     | `cli_compact_actual_records_compaction_in_jsonl`                |
//! | `compaction_cli.ps1` T3 (cache)    | `cli_compact_updates_context_cache`                             |
//! | `compaction_cli.ps1` T4 (usable)   | `cli_compact_session_usable_after_compaction`                   |
//! | `compaction_cli.ps1` T5 (custom)   | `cli_compact_custom_instruction_in_summary`                     |
//! | `compaction_cli.ps1` T6 (incremental) | `cli_compact_incremental_compaction_numbers`                  |
//! | `compaction_extension.ps1` T1-T4   | `cli_compact_with_compaction_extension_installed`               |
//!
//! **Tier:** mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set). Tests early-return if unset so `cargo test`
//! still passes on a bare checkout.
//!
//! **`#[serial]`.** All tests in this file are `#[serial]` because they
//! share the mock LLM's per-substring counter (see
//! `docs/integration/TESTING.md` §3 Sequence). Per-test agent names and
//! per-test needles are belt-and-suspenders isolation.

mod common;
use common::{configure_mock, run_with_timeout, DaemonGuard, PekoCli};
use serde_json::Value;
use serial_test::serial;
use std::path::{Path, PathBuf};
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

/// Write a mock-LLM-pointed agent. `peko session compact` is
/// truncation-based (see `src/compaction/cli.rs:75`), so the compact
/// itself doesn't need any tools. We still enable `write_file` /
/// `read_file` / `shell` (and their canonical IDs) so the
/// T4 (post-compact usable) and extension tests can drive
/// `peko send <agent> <prompt>` that wants to write a file.
fn write_compaction_agent(
    home: &std::path::Path,
    name: &str,
    mock_llm_url: &str,
) -> std::io::Result<()> {
    let agent_dir = home.join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;
    let _base_url = mock_llm_url.trim_end_matches('/');
    // v3 agent config: soft hints only (catalog + keychain own the
    // provider wiring). The test harness pre-seeds a `mock-llm`
    // provider in the catalog via `seed_mock_provider_in_catalog`
    // (commit 3.5); this helper assumes that step has already run
    // for the given `home`.
    let config_toml = format!(
        r#"version = "3.0"
name = "{name}"
description = "CLI integration test agent for session compaction"
auto_accept_trusted = false
default_timeout_seconds = 60

preferred_provider_id = "mock-llm"
preferred_model_id = "default"

[extensions]
enabled = [
    "shell",
    "Read",
    "write_file",
    "builtin:tool:shell",
    "builtin:tool:Read",
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

/// Build the `MOCK_LLM_SCRIPT` for `n_turns` setup rounds.
///
/// Each turn drives the LLM twice: the first call gets a `write_file`
/// tool_call, the second call (after the tool dispatch) gets a text
/// sentinel. The flat `[tc_1, sent_1, tc_2, sent_2, …]` list is
/// consumed in order by the mock's per-substring counter, which
/// advances on every LLM call.
fn build_n_turn_script(needle: &str, n_turns: usize) -> String {
    let mut entries = Vec::with_capacity(n_turns * 2);
    for i in 0..n_turns {
        let path = format!("compaction_setup_t{i:02}.txt");
        let content = format!("COMPACTION_SETUP_T{i:02}_CONTENT");
        entries.push(serde_json::json!({
            "tool_call": {
                "name": "write_file",
                "arguments": serde_json::json!({ "path": path, "content": content }).to_string(),
            }
        }));
        entries.push(serde_json::json!({ "text": format!("SETUP_T{i:02}_DONE") }));
    }
    serde_json::json!({ needle: entries }).to_string()
}

/// Run a single `peko send` round. Returns the response stdout.
fn send_turn(cli: &PekoCli, agent_name: &str, prompt: &str, timeout: Duration) -> (String, String) {
    let (out, err, status) = run(cli, &["send", agent_name, prompt, "--no-stream"], timeout);
    assert_ok(&out, &err, &status);
    (out, err)
}

/// Build a single combined script for `n_setup_turns` setup rounds
/// plus `extra_turns` post-compact rounds, all keyed on a single
/// shared `needle`. The flat list is consumed in order by the mock's
/// per-substring counter — `(n_setup_turns + extra_turns) * 2`
/// elements total. The shared needle is critical: the mock keys
/// `MOCK_LLM_SCRIPT` on the FIRST user message in the LLM request,
/// and all the `peko send` prompts in this test embed the same
/// needle so the script entry matches regardless of which turn
/// drives the request.
fn build_full_script(
    needle: &str,
    n_setup_turns: usize,
    extra_turns: &[(String, String, String)], // (path, content, sentinel)
) -> String {
    let total = n_setup_turns + extra_turns.len();
    let mut entries = Vec::with_capacity(total * 2);
    // Setup turns
    for i in 0..n_setup_turns {
        let path = format!("compaction_setup_t{i:02}.txt");
        let content = format!("COMPACTION_SETUP_T{i:02}_CONTENT");
        entries.push(serde_json::json!({
            "tool_call": {
                "name": "write_file",
                "arguments": serde_json::json!({ "path": path, "content": content }).to_string(),
            }
        }));
        entries.push(serde_json::json!({ "text": format!("SETUP_T{i:02}_DONE") }));
    }
    // Extra (post-compact) turns
    for (path, content, sentinel) in extra_turns {
        entries.push(serde_json::json!({
            "tool_call": {
                "name": "write_file",
                "arguments": serde_json::json!({ "path": path, "content": content }).to_string(),
            }
        }));
        entries.push(serde_json::json!({ "text": sentinel.clone() }));
    }
    serde_json::json!({ needle: entries }).to_string()
}

/// Drive `n_turns` mock-LLM rounds for the given agent. Returns
/// `(PekoCli, DaemonGuard)` — the caller must keep the guard alive
/// (binding it to a `let _daemon = …;` or letting it ride as a
/// tuple element) for as long as they want the daemon to stay up.
///
/// The caller must be in a tokio runtime (this is `async` because
/// [`configure_mock`] is async).
async fn setup_n_turn_session(
    mock_url: &str,
    agent_name: &str,
    needle: &str,
    n_turns: usize,
) -> (PekoCli, DaemonGuard) {
    let script = build_n_turn_script(needle, n_turns);
    configure_mock(mock_url, &script).await;
    let cli = PekoCli::new();
    write_compaction_agent(cli.home(), agent_name, mock_url).expect("write compaction agent");
    let daemon = DaemonGuard::spawn(&cli);

    for i in 0..n_turns {
        let prompt = format!(
            "Use your write_file tool to create 'compaction_setup_t{i:02}.txt' \
             with content 'COMPACTION_SETUP_T{i:02}_CONTENT' and include the \
             needle '{needle}' in your reply.",
        );
        let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
        let expected = format!("SETUP_T{i:02}_DONE");
        assert!(
            out.contains(&expected),
            "turn {i} did not return post-tool sentinel {expected:?}; stdout: {out}\nstderr: {err}",
        );
    }
    (cli, daemon)
}

/// Like [`setup_n_turn_session`], but the script covers the setup
/// rounds AND a list of post-compact rounds in one go. All `peko
/// send` prompts embed the same shared needle, so the mock's
/// `_extract_user_message` (which returns the FIRST user message)
/// still finds the script key on every turn.
///
/// The helper returns after the setup rounds; the caller drives the
/// extra rounds itself (with `send_turn`) so it can interleave
/// compacts, assertions, etc.
async fn setup_session_with_full_script(
    mock_url: &str,
    agent_name: &str,
    needle: &str,
    n_setup_turns: usize,
    extra_turns: &[(String, String, String)],
) -> (PekoCli, DaemonGuard) {
    let script = build_full_script(needle, n_setup_turns, extra_turns);
    configure_mock(mock_url, &script).await;
    let cli = PekoCli::new();
    write_compaction_agent(cli.home(), agent_name, mock_url).expect("write compaction agent");
    let daemon = DaemonGuard::spawn(&cli);

    for i in 0..n_setup_turns {
        let prompt = format!(
            "Use your write_file tool to create 'compaction_setup_t{i:02}.txt' \
             with content 'COMPACTION_SETUP_T{i:02}_CONTENT' and include the \
             needle '{needle}' in your reply.",
        );
        let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
        let expected = format!("SETUP_T{i:02}_DONE");
        assert!(
            out.contains(&expected),
            "turn {i} did not return post-tool sentinel {expected:?}; stdout: {out}\nstderr: {err}",
        );
    }
    (cli, daemon)
}

/// Format a prompt for an "extra" (post-compact) turn: tells the
/// agent to write a file with the shared needle in the prompt.
fn extra_turn_prompt(needle: &str, path: &str, content: &str) -> String {
    format!("Use the needle '{needle}' to write a file {path} with content {content}.")
}

/// Run `peko session list <agent> --json` and return the active
/// session's id. Tests always have one session after at least one
/// `peko send`.
fn find_active_session_id(cli: &PekoCli, agent_name: &str) -> String {
    let (out, err, status) = run(
        cli,
        &["session", "list", agent_name, "--json"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    let parsed: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("session list parse error: {e}\nstdout: {out}"));
    // `active_session` is the canonical active id; if absent, fall
    // back to the first listed session.
    if let Some(active) = parsed.get("active_session").and_then(|v| v.as_str()) {
        if !active.is_empty() {
            return active.to_string();
        }
    }
    parsed
        .get("sessions")
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.get("id"))
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no session id in list output: {out}"))
}

/// The on-disk path to the session's JSONL file. `peko send` defaults
/// the team to `"default"` when no `--team` is passed
/// ([`parse_agent_identifier_with_override`](../../peko/peko-runtime/src/common/identifiers.rs)
/// in `src/common/identifiers.rs:123`), so the agent's `default/`
/// subdirectory is where the JSONL lives. (The agent is created
/// in the `default` team too — see `write_compaction_agent` and
/// `peko agent create` defaults.)
fn session_jsonl_path(cli: &PekoCli, agent_name: &str, session_id: &str) -> PathBuf {
    cli.peko_dir()
        .join("data")
        .join("sessions")
        .join(agent_name)
        .join("default")
        .join(format!("{session_id}.jsonl"))
}

/// Read the JSONL, parse each line, and return the list of compaction
/// events (in order, oldest first) with their detail map. A
/// "compaction event" is a `SessionEvent::System` with
/// `event: "compaction"`; its `detail` carries `summary`,
/// `messages_compacted`, `tokens_before`, `tokens_after`, and
/// `compaction_number`.
fn read_compaction_events(jsonl_path: &Path) -> Vec<Value> {
    let content = std::fs::read_to_string(jsonl_path)
        .unwrap_or_else(|e| panic!("read jsonl {jsonl_path:?}: {e}"));
    let mut out = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // The JSONL event shape is `{"type": "system", "event": "compaction", "detail": {...}}`.
        // We accept both `type: "system"` and the older `type: "System"` to be robust.
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let is_system = ty.eq_ignore_ascii_case("system");
        let event_name = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
        if is_system && event_name == "compaction" {
            if let Some(detail) = v.get("detail") {
                out.push(detail.clone());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// T1: dry-run (smoke)
// ---------------------------------------------------------------------------

/// `compaction_cli.ps1` T1 (smoke): `peko session compact --dry-run --json`
/// emits the JSON shape the PS test expects. Asserts the presence of
/// the `dry_run: true` flag and the `DryRunReport` fields — the actual
/// CLI fix in `be34a2e` ships. Pre-fix, this assertion would fail at
/// parse time (no JSON was emitted).
///
/// We do one `peko send` first to create a session (otherwise the
/// daemon refuses with "No active session for agent …").
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
    write_compaction_agent(cli.home(), agent_name, &mock_url).expect("write compaction agent");
    let _daemon = DaemonGuard::spawn(&cli);

    let warmup_prompt =
        format!("Use the needle '{needle}' to write a file warmup.txt and respond WARMUP_DONE.");
    let (_out, _err) = send_turn(&cli, agent_name, &warmup_prompt, Duration::from_secs(30));

    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--dry-run", "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    let parsed: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("dry-run --json parse error: {e}\nstdout: {out}"));

    assert_eq!(parsed["success"], Value::Bool(true));
    assert_eq!(
        parsed["dry_run"],
        Value::Bool(true),
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

// ---------------------------------------------------------------------------
// T1 multi: dry-run after 6 turns (Issue 030 regression)
// ---------------------------------------------------------------------------

/// Regression for issue 030: with multiple `peko send` rounds, the
/// `message_count` and `messages_to_compact` fields in
/// `peko session compact --dry-run --json` must reflect the actual
/// session contents. Pre-fix, both were hard-coded to 0 because the
/// daemon's dry-run response overloaded the real-compaction
/// `messages_compacted` field (which is meaningless for a no-op
/// preview) and the CLI re-mapped it to both output fields.
///
/// The test does 6 mock-LLM-driven `peko send` rounds. Each round
/// produces a user prompt, an assistant tool_call, a tool result,
/// and an assistant text sentinel — so the JSONL accumulates 24+
/// `message.v2` events. After 6 rounds, the dry-run must report
/// `message_count >= 6` and `messages_to_compact >= 1`.
///
/// The mock's per-substring counter advances on every LLM call, so
/// the flat `[tc_1, sent_1, tc_2, sent_2, …]` script drives all 12
/// LLM calls in order.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn cli_compact_dry_run_json_reports_message_counts_after_multi_turn() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();
    let agent_name = "cli_compact_dry_run_multi";
    let needle = "cli-compact-dryjson-multi-q3x1";
    const N_TURNS: usize = 6;

    let script = build_n_turn_script(needle, N_TURNS);
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_compaction_agent(cli.home(), agent_name, &mock_url).expect("write compaction agent");
    let _daemon = DaemonGuard::spawn(&cli);

    for i in 0..N_TURNS {
        let prompt = format!(
            "Use your write_file tool to create 'compaction_setup_t{i:02}.txt' \
             with content 'COMPACTION_SETUP_T{i:02}_CONTENT' and include the \
             needle '{needle}' in your reply.",
        );
        let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
        let expected = format!("SETUP_T{i:02}_DONE");
        assert!(
            out.contains(&expected),
            "turn {i} did not return post-tool sentinel; stdout: {out}\nstderr: {err}",
        );
    }

    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--dry-run", "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    let parsed: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("dry-run --json parse error: {e}\nstdout: {out}"));

    assert_eq!(parsed["success"], Value::Bool(true));
    assert_eq!(parsed["dry_run"], Value::Bool(true));

    let message_count = parsed["message_count"].as_u64().expect("message_count u64");
    let messages_to_compact = parsed["messages_to_compact"]
        .as_u64()
        .expect("messages_to_compact u64");
    assert!(
        message_count >= N_TURNS as u64,
        "message_count should be >= {N_TURNS} after {N_TURNS} turns, got {message_count} \
         (full output: {out})",
    );
    assert!(
        messages_to_compact >= 1,
        "messages_to_compact should be >= 1, got {messages_to_compact} (full output: {out})",
    );
}

// ---------------------------------------------------------------------------
// T2: actual compaction
// ---------------------------------------------------------------------------

/// `compaction_cli.ps1` T2: `peko session compact <agent> --json` (no
/// `--dry-run`) returns the real-compaction wire shape and writes a
/// `compaction` event into the session JSONL.
///
/// Verifies:
///   - `success: true`, `messages_compacted > 0`,
///     `tokens_before > tokens_after` (some tokens were actually saved).
///   - The JSONL contains a `compaction` event with `compaction_number: 1`
///     and a non-empty `summary`.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn cli_compact_actual_records_compaction_in_jsonl() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();
    let agent_name = "cli_compact_actual";
    let needle = "cli-compact-actual-h7n4";
    const N_TURNS: usize = 4;

    let (cli, _daemon) = setup_n_turn_session(&mock_url, agent_name, needle, N_TURNS).await;

    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    let parsed: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("compact --json parse error: {e}\nstdout: {out}"));

    assert_eq!(parsed["success"], Value::Bool(true));
    let messages_compacted = parsed["messages_compacted"]
        .as_u64()
        .expect("messages_compacted u64");
    let tokens_before = parsed["tokens_before"].as_u64().expect("tokens_before u64");
    let tokens_after = parsed["tokens_after"].as_u64().expect("tokens_after u64");
    let tokens_saved = parsed["tokens_saved"].as_u64().expect("tokens_saved u64");
    assert!(
        messages_compacted >= 1,
        "messages_compacted should be >= 1, got {messages_compacted} (full output: {out})",
    );
    assert!(
        tokens_before > tokens_after,
        "tokens_before ({tokens_before}) should exceed tokens_after ({tokens_after}) \
         after a successful compaction (full output: {out})",
    );
    assert_eq!(
        tokens_saved,
        tokens_before - tokens_after,
        "tokens_saved should equal tokens_before - tokens_after",
    );

    // Verify the JSONL has a compaction event with number 1.
    let session_id = find_active_session_id(&cli, agent_name);
    let jsonl = session_jsonl_path(&cli, agent_name, &session_id);
    let events = read_compaction_events(&jsonl);
    assert_eq!(
        events.len(),
        1,
        "expected exactly 1 compaction event after first compact, got {} (jsonl: {jsonl:?})",
        events.len(),
    );
    assert_eq!(
        events[0]["compaction_number"].as_u64(),
        Some(1),
        "first compaction should be number 1; event: {events:?}",
    );
    let summary = events[0]["summary"]
        .as_str()
        .expect("compaction summary should be a string");
    assert!(
        !summary.is_empty(),
        "compaction summary should be non-empty; event: {events:?}",
    );
}

// ---------------------------------------------------------------------------
// T3: context cache
// ---------------------------------------------------------------------------

/// `compaction_cli.ps1` T3: after a real compact, the session's
/// `<session_id>.context.cache` file is rewritten and contains a
/// system message that starts with "Conversation Summary".
///
/// The cache is the derived view the agent reads on resume; it's
/// regenerated at the end of every compact so the checksum matches
/// the JSONL. We don't pin the exact format (it can be JSONL,
/// JSON, or a comment-prefixed line format depending on the
/// implementation) — we just look for a system role whose text
/// contains the summary marker.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn cli_compact_updates_context_cache() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();
    let agent_name = "cli_compact_cache";
    let needle = "cli-compact-cache-z2r9";
    const N_TURNS: usize = 4;

    let (cli, _daemon) = setup_n_turn_session(&mock_url, agent_name, needle, N_TURNS).await;

    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    let session_id = find_active_session_id(&cli, agent_name);
    // The cache lives in the same directory as the JSONL with the
    // exact filename `<session_id>.context.cache` (NOT
    // `<session_id>.jsonl.context.cache` — see
    // `SessionStorage::context_cache_path` in
    // `src/session/jsonl.rs:528-530`).
    let jsonl = session_jsonl_path(&cli, agent_name, &session_id);
    let cache_path = jsonl.with_file_name(format!("{session_id}.context.cache"));
    let cache = std::fs::read_to_string(&cache_path)
        .unwrap_or_else(|e| panic!("read cache {cache_path:?}: {e}"));

    // The cache is the persisted LlmMessage list. Strip any
    // comment-prefixed lines (e.g. `# checksum=…`) and look for the
    // summary marker somewhere in the remaining text. Multiple
    // accepted phrasings: "Conversation Summary" (used by the
    // system) and "Compacted" (legacy alternate). Either is fine.
    let stripped: String = cache
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        stripped.contains("Conversation Summary") || stripped.contains("Compacted"),
        "context cache should contain a summary marker; got:\n{cache}",
    );
}

// ---------------------------------------------------------------------------
// T4: post-compact session usable
// ---------------------------------------------------------------------------

/// `compaction_cli.ps1` T4: after a real compact, the session is
/// still usable. We do one more `peko send` round that asks the
/// agent to write a file; the file should land in the workspace
/// and the post-tool sentinel should be in the response.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn cli_compact_session_usable_after_compaction() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();
    let agent_name = "cli_compact_usable";
    let needle = "cli-compact-usable-z3n8";
    const N_TURNS: usize = 4;

    // Single combined script for 4 setup turns + 1 post-compact
    // turn. All `peko send` prompts embed the same needle, so the
    // mock's `_extract_user_message` (which returns the FIRST
    // user message) still finds the script key on every turn.
    let extra = vec![(
        "post_compact.txt".to_string(),
        "POST_COMPACT_OK".to_string(),
        "POST_COMPACT_SUCCESS".to_string(),
    )];
    let (cli, _daemon) =
        setup_session_with_full_script(&mock_url, agent_name, needle, N_TURNS, &extra).await;

    // First compact.
    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // Post-compact send: the script has 2 more elements (tool_call
    // + sentinel) for this turn, so the response is POST_COMPACT_SUCCESS.
    let prompt = extra_turn_prompt(needle, "post_compact.txt", "POST_COMPACT_OK");
    let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
    assert!(
        out.contains("POST_COMPACT_SUCCESS"),
        "post-compact send did not return sentinel; stdout: {out}\nstderr: {err}",
    );

    let file = cli
        .peko_dir()
        .join("data")
        .join("workspaces")
        .join("post_compact.txt");
    let content = std::fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("read post-compact workspace file {file:?}: {e}"));
    assert!(
        content.contains("POST_COMPACT_OK"),
        "post-compact file content should contain POST_COMPACT_OK; got: {content}",
    );
}

// ---------------------------------------------------------------------------
// T5: custom instruction
// ---------------------------------------------------------------------------

/// `compaction_cli.ps1` T5: a compact with `--instruction` records
/// that instruction in the summary of the new compaction event.
///
/// We do 4 setup turns, compact once, do one more turn to grow the
/// session, then compact with the custom instruction. The latest
/// compaction event in the JSONL should have a `summary` field
/// containing the instruction text.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn cli_compact_custom_instruction_in_summary() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();
    let agent_name = "cli_compact_custom";
    let needle = "cli-compact-custom-w4j7";
    const N_TURNS: usize = 4;

    // 1 extra round after the first compact, to give the second
    // (custom-instruction) compact something to fold.
    let extra = vec![(
        "grow.txt".to_string(),
        "GROW".to_string(),
        "GROW_DONE".to_string(),
    )];
    let (cli, _daemon) =
        setup_session_with_full_script(&mock_url, agent_name, needle, N_TURNS, &extra).await;

    // First compact (no instruction).
    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // Grow turn.
    let prompt = extra_turn_prompt(needle, "grow.txt", "GROW");
    let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
    assert!(
        out.contains("GROW_DONE"),
        "grow turn did not return sentinel; stdout: {out}\nstderr: {err}",
    );

    // Custom-instruction compact.
    let custom_instruction = "Focus on file operations";
    let (out, err, status) = run(
        &cli,
        &[
            "session",
            "compact",
            agent_name,
            "--instruction",
            custom_instruction,
            "--json",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // The latest compaction event's summary should mention the
    // instruction.
    let session_id = find_active_session_id(&cli, agent_name);
    let jsonl = session_jsonl_path(&cli, agent_name, &session_id);
    let events = read_compaction_events(&jsonl);
    assert!(
        events.len() >= 2,
        "expected at least 2 compaction events, got {} (jsonl: {jsonl:?})",
        events.len(),
    );
    let latest_summary = events
        .last()
        .and_then(|e| e.get("summary"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    assert!(
        latest_summary.contains(custom_instruction),
        "latest compaction summary should contain the custom instruction \
         {custom_instruction:?}; got: {latest_summary:?}",
    );
}

// ---------------------------------------------------------------------------
// T6: incremental compaction numbers
// ---------------------------------------------------------------------------

/// `compaction_cli.ps1` T6: a sequence of two compactions produces
/// two compaction events in the JSONL with `compaction_number` 1
/// and 2 in that order.
///
/// Reuses the T5 setup (4 setup turns, 1 plain compact, 1 grow turn,
/// 1 custom compact). We assert:
///   - `>= 2` compaction events in the JSONL
///   - the `compaction_number` values are `[1, 2]`
///   - they are strictly increasing
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn cli_compact_incremental_compaction_numbers() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();
    let agent_name = "cli_compact_incremental";
    let needle = "cli-compact-incr-q2k5";
    const N_TURNS: usize = 4;

    let extra = vec![(
        "grow.txt".to_string(),
        "GROW".to_string(),
        "GROW_DONE".to_string(),
    )];
    let (cli, _daemon) =
        setup_session_with_full_script(&mock_url, agent_name, needle, N_TURNS, &extra).await;

    // First compact.
    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // Grow turn.
    let prompt = extra_turn_prompt(needle, "grow.txt", "GROW");
    let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
    assert!(
        out.contains("GROW_DONE"),
        "grow turn did not return sentinel; stdout: {out}\nstderr: {err}",
    );

    // Second compact (with custom instruction, just to vary the path).
    let (out, err, status) = run(
        &cli,
        &[
            "session",
            "compact",
            agent_name,
            "--instruction",
            "Focus on file operations",
            "--json",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // The JSONL should now have exactly 2 compaction events with
    // numbers 1 and 2.
    let session_id = find_active_session_id(&cli, agent_name);
    let jsonl = session_jsonl_path(&cli, agent_name, &session_id);
    let events = read_compaction_events(&jsonl);
    assert!(
        events.len() >= 2,
        "expected at least 2 compaction events, got {} (jsonl: {jsonl:?})",
        events.len(),
    );
    let numbers: Vec<u64> = events
        .iter()
        .filter_map(|e| e.get("compaction_number").and_then(|n| n.as_u64()))
        .collect();
    assert!(
        numbers.windows(2).all(|w| w[0] < w[1]),
        "compaction numbers should be strictly increasing; got: {numbers:?}",
    );
    assert_eq!(
        numbers.first().copied(),
        Some(1),
        "first compaction should be #1; got: {numbers:?}",
    );
}

// ---------------------------------------------------------------------------
// Extension test
// ---------------------------------------------------------------------------

/// `compaction_extension.ps1` T1-T4: with a custom session-compaction
/// extension installed (`e2e_tests/compaction/extensions/custom_compactor`,
/// which registers `session.compaction` and `session.compaction_post`
/// hooks), the CLI compaction flow still works end-to-end and the
/// custom instruction is preserved.
///
/// This is a smoke test of the hook-wiring path: it doesn't
/// introspect what the hook handlers do (they're no-op stubs in the
/// test extension), it just confirms that installing the extension
/// doesn't break the underlying compact and that the session
/// remains usable. The full hook semantic behavior is covered by
/// in-process unit tests for the hook dispatch.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn cli_compact_with_compaction_extension_installed() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();
    let agent_name = "cli_compact_ext";
    let needle = "cli-compact-ext-h6y9";
    const N_TURNS: usize = 3;

    // Single combined script: 3 setup rounds + 1 post-compact
    // round (after first compact) + 1 grow round (after first
    // compact, before the second). All `peko send` prompts embed
    // the same needle.
    let extra = vec![
        (
            "ext_post_compact.txt".to_string(),
            "EXT_POST_COMPACT_OK".to_string(),
            "EXT_POST_COMPACT_SUCCESS".to_string(),
        ),
        (
            "ext_grow.txt".to_string(),
            "EXT_GROW".to_string(),
            "EXT_GROW_DONE".to_string(),
        ),
    ];
    let script = build_full_script(needle, N_TURNS, &extra);
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_compaction_agent(cli.home(), agent_name, &mock_url).expect("write compaction agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Install the custom compactor extension from the on-disk
    // test fixture. The manifest lives at
    // `e2e_tests_archive/compaction/extensions/custom_compactor/manifest.yaml`
    // — `peko ext install` takes the directory path.
    // (Note: previously `e2e_tests/...`; commit 0b363ae archived the
    //  PS1 e2e_tests tree under `e2e_tests_archive/`.)
    let ext_dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/e2e_tests_archive/compaction/extensions/custom_compactor"
    );
    let (out, err, status) = run(&cli, &["ext", "install", ext_dir], Duration::from_secs(20));
    assert_ok(&out, &err, &status);
    // The install should report the extension id.
    assert!(
        out.contains("custom-compactor-test"),
        "ext install should report extension id; stdout: {out}\nstderr: {err}",
    );

    // Drive 3 setup rounds.
    for i in 0..N_TURNS {
        let prompt = format!(
            "Use your write_file tool to create 'compaction_setup_t{i:02}.txt' \
             with content 'COMPACTION_SETUP_T{i:02}_CONTENT' and include the \
             needle '{needle}' in your reply.",
        );
        let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
        let expected = format!("SETUP_T{i:02}_DONE");
        assert!(
            out.contains(&expected),
            "setup turn {i} did not return sentinel; stdout: {out}\nstderr: {err}",
        );
    }

    // First compact (no instruction) — must succeed and write
    // a compaction event.
    let (out, err, status) = run(
        &cli,
        &["session", "compact", agent_name, "--json"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    let compact1: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("compact --json parse error: {e}\nstdout: {out}"));
    assert_eq!(compact1["success"], Value::Bool(true));
    let messages_compacted = compact1["messages_compacted"]
        .as_u64()
        .expect("messages_compacted u64");
    assert!(
        messages_compacted >= 1,
        "messages_compacted should be >= 1 with extension installed; got {messages_compacted}",
    );

    let session_id = find_active_session_id(&cli, agent_name);
    let jsonl = session_jsonl_path(&cli, agent_name, &session_id);
    let events_after_first = read_compaction_events(&jsonl);
    assert_eq!(
        events_after_first.len(),
        1,
        "expected exactly 1 compaction event after first compact, got {} (jsonl: {jsonl:?})",
        events_after_first.len(),
    );

    // Post-compact send — session is still usable.
    let prompt = extra_turn_prompt(needle, "ext_post_compact.txt", "EXT_POST_COMPACT_OK");
    let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
    assert!(
        out.contains("EXT_POST_COMPACT_SUCCESS"),
        "post-compact send did not return sentinel; stdout: {out}\nstderr: {err}",
    );
    let post_file = cli
        .peko_dir()
        .join("data")
        .join("workspaces")
        .join("ext_post_compact.txt");
    let post_content = std::fs::read_to_string(&post_file)
        .unwrap_or_else(|e| panic!("read post-compact workspace file {post_file:?}: {e}"));
    assert!(
        post_content.contains("EXT_POST_COMPACT_OK"),
        "post-compact file content should contain EXT_POST_COMPACT_OK; got: {post_content}",
    );

    // Grow turn (gives the second compact something to fold).
    let prompt = extra_turn_prompt(needle, "ext_grow.txt", "EXT_GROW");
    let (out, err) = send_turn(&cli, agent_name, &prompt, Duration::from_secs(30));
    assert!(
        out.contains("EXT_GROW_DONE"),
        "grow turn did not return sentinel; stdout: {out}\nstderr: {err}",
    );

    // Custom-instruction compact.
    let custom_instruction = "Focus on file operations";
    let (out, err, status) = run(
        &cli,
        &[
            "session",
            "compact",
            agent_name,
            "--instruction",
            custom_instruction,
            "--json",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // Now we should have 2 compaction events; the latest summary
    // should include the custom instruction.
    let events_after_second = read_compaction_events(&jsonl);
    assert_eq!(
        events_after_second.len(),
        2,
        "expected exactly 2 compaction events after second compact, got {} (jsonl: {jsonl:?})",
        events_after_second.len(),
    );
    let latest_summary = events_after_second
        .last()
        .and_then(|e| e.get("summary"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    assert!(
        latest_summary.contains(custom_instruction),
        "latest compaction summary should contain the custom instruction \
         {custom_instruction:?}; got: {latest_summary:?}",
    );

    // The two compaction numbers should be 1 and 2 in order.
    let numbers: Vec<u64> = events_after_second
        .iter()
        .filter_map(|e| e.get("compaction_number").and_then(|n| n.as_u64()))
        .collect();
    assert_eq!(
        numbers,
        vec![1, 2],
        "compaction numbers should be [1, 2]; got: {numbers:?}"
    );

    // Cleanup: uninstall the extension so the next test starts
    // fresh. (The PekoCli tempdir is dropped on test exit, but the
    // `peko config` directory persists across tests within the
    // same daemon if they share one — they don't, but uninstall is
    // cheap and matches the PS test's cleanup pattern.)
    let (out, _err, status) = run(
        &cli,
        &["ext", "uninstall", "custom-compactor-test"],
        Duration::from_secs(10),
    );
    // Soft-fail the uninstall: if it didn't work (e.g. daemon
    // already dropped the registry entry), it doesn't matter for
    // this test's assertions.
    let _ = (out, status);
}
