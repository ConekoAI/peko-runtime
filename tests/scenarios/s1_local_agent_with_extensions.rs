//! End-to-end user-journey scenario D1 (Phase D slice per
//! `docs/integration/TESTING.md` §7).
//!
//! Coverage — flow 1 + 2 from the Phase D plan:
//!
//! | Rust test                                | Flow step                                           |
//! |------------------------------------------|-----------------------------------------------------|
//! | `agent_create_local_minimal`             | Flow 1: create agent locally                        |
//! | `ext_install_and_info_round_trip`        | Flow 2a: create an extension (skill)                |
//! | `ext_enable_modifies_agent_whitelist`    | Flow 2b: enable the ext on the agent                |
//! | `ext_disable_removes_from_whitelist`     | Flow 2c: disable the ext                            |
//! | `agent_chats_locally_with_keyword`       | Flow 1b: chat with the agent (mock LLM keyword echo)|
//! | `agent_chats_with_enabled_extension`     | Flow 2d: chat with the agent after enabling ext    |
//!
//! ## Scope
//!
//! Local-only. **No PekoHub dependency.** The LLM is the CI mock
//! (which `make docker-up` brings up; `DEFAULT_RESPONSE=SUCCESS`
//! is set in the compose file). All tests early-return if
//! `MOCK_LLM_URL` is unset so a bare `cargo test` still passes.
//!
//! ## Why this is mock-LLM tier (not real-LLM)
//!
//! These tests assert on plumbing: "the agent is created on disk,
//! the extension is installed, the ext is enabled on the agent's
//! whitelist, and a `peko send` round-trip completes end-to-end
//! with a deterministic mock response." The LLM's *decisions* are
//! irrelevant — only the orchestration surface matters.
//!
//! ## The two structural facts this file relies on
//!
//! 1. **The canonical extension ID for a non-builtin extension is
//!    the extension ID itself** (no `builtin:tool:` prefix). See
//!    [`src/ipc/server.rs:1818-1903`](../../src/ipc/server.rs#L1818-L1903).
//!    So `peko ext enable calculator-skill --target <agent>`
//!    writes the literal string `calculator-skill` into the
//!    agent's `config.toml [extensions] enabled` list.
//! 2. **The SKILL.md `name:` frontmatter field becomes the
//!    extension ID** ([`src/extensions/skill/adapter.rs:108-130`](../../src/extensions/skill/adapter.rs#L108-L130)).
//!    Our inline fixture writes `name: calculator-skill` so the
//!    install creates extension ID `calculator-skill`.

#[path = "../common/mod.rs"]
mod common;
use common::{run_with_timeout, write_v3_mock_agent, DaemonGuard, PekoCli};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MOCK_LLM_URL` env, return Some(url) if set and non-empty,
/// None otherwise. Tests early-return on None so a bare `cargo test`
/// on a checkout without the docker-compose stack still passes.
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

/// Absolute path to a scratch dir under the test's isolated `HOME`,
/// where we write the on-disk extension fixture (a single `SKILL.md`).
fn scratch_dir(cli: &PekoCli) -> PathBuf {
    cli.home().join("scratch")
}

/// Write a minimal Tier 1 skill extension to a scratch dir. The
/// `name:` frontmatter becomes the extension ID, so this fixture
/// installs as extension id `calculator-skill` (Tier 1 SKILL.md
/// detection — no `--type` flag, no `manifest.yaml`).
fn write_calculator_skill(scratch: &Path) -> std::io::Result<PathBuf> {
    let skill_dir = scratch.join("calculator-skill");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: calculator-skill
description: Sum two numbers (test fixture)
---
# Sum
Compute a + b.
"#,
    )?;
    Ok(skill_dir)
}

/// Path to the agent's on-disk config.toml.
fn agent_config_path(cli: &PekoCli, agent_name: &str) -> PathBuf {
    cli.peko_dir()
        .join("agents")
        .join(agent_name)
        .join("config.toml")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Flow 1: create an agent locally. The agent's `config.toml` must
/// exist at `<peko_dir>/agents/<name>/config.toml` and must contain
/// the openai_compatible provider pointed at the mock LLM. We write
/// the agent config directly (the `write_v3_mock_agent` pattern) rather
/// than driving `peko agent create` (which would re-do the same
/// filesystem work; the create-vs-write helper is unit-tested
/// elsewhere). The point of D1 is the lifecycle, not the create CLI.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn agent_create_local_minimal() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let agent_name = "s1_local_agent";
    write_v3_mock_agent(cli.home(), agent_name, &mock_url).expect("write mock agent");

    // `peko agent show <name>` reads back the config; this is the
    // closest thing to a list/show verification without a
    // `peko agent list --json` (the list subcommand doesn't have a
    // --json flag).
    let _daemon = DaemonGuard::spawn(&cli);
    let (out, err, status) = run(
        &cli,
        &["agent", "show", agent_name],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains(agent_name),
        "show output should contain the agent name: stdout={out} stderr={err}",
    );

    // Sanity: the on-disk config carries the v3 soft hints. The
    // mock URL itself lives in the catalog (seeded by
    // `DaemonGuard::spawn` via `seed_mock_provider_in_catalog`),
    // not on the agent config — that was the v1 shape.
    let cfg =
        std::fs::read_to_string(agent_config_path(&cli, agent_name)).expect("read agent config");
    assert!(
        cfg.contains("preferred_provider_id = \"mock-llm\""),
        "agent config should reference the mock-llm catalog entry: {cfg}",
    );
    assert!(
        cfg.contains("version = \"3.0\""),
        "agent config should be v3: {cfg}",
    );
}

/// Flow 2a: install the calculator-skill extension (Tier 1 SKILL.md
/// detection — no `--type` flag). Verify `peko ext info
/// calculator-skill` reports `type: "skill"` and the install dir
/// contains the on-disk `SKILL.md` (per
/// [`src/extension/manager/storage.rs:123-172`](../../src/extension/manager/storage.rs#L123-L172)).
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn ext_install_and_info_round_trip() {
    let Some(_mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let scratch = scratch_dir(&cli);
    let skill_dir = write_calculator_skill(&scratch).expect("write skill fixture");

    let _daemon = DaemonGuard::spawn(&cli);
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &skill_dir.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("calculator-skill"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // peko ext info confirms type "skill".
    let (info_out, err, status) = run(
        &cli,
        &["ext", "info", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ok(&info_out, &err, &status);
    assert!(
        info_out.contains("\"type\": \"skill\""),
        "info should report type=skill: {info_out}",
    );

    // On-disk install dir has the SKILL.md.
    let install_dir = cli
        .peko_dir()
        .join("data")
        .join("extensions")
        .join("calculator-skill");
    assert!(
        install_dir.join("SKILL.md").exists(),
        "install dir should contain SKILL.md at {install_dir:?}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// Flow 2b: enable the calculator-skill extension on an agent.
/// `peko ext enable calculator-skill --target <agent>` writes the
/// canonical extension id (the id itself for non-builtin extensions)
/// into the agent's `config.toml [extensions] enabled` list. The
/// on-disk config is the assertion point — the CLI doesn't expose a
/// `--json` flag for the whitelist query.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn ext_enable_modifies_agent_whitelist() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let agent_name = "s1_enable_test_agent";
    write_v3_mock_agent(cli.home(), agent_name, &mock_url).expect("write mock agent");
    let scratch = scratch_dir(&cli);
    let skill_dir = write_calculator_skill(&scratch).expect("write skill fixture");

    let _daemon = DaemonGuard::spawn(&cli);

    // Install the skill.
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &skill_dir.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // The agent config has an empty `[extensions] enabled` list
    // straight from `write_v3_mock_agent`; verify that baseline.
    let cfg = agent_config_path(&cli, agent_name);
    let before = std::fs::read_to_string(&cfg).expect("read agent config");
    assert!(
        before.contains("[extensions]") && before.contains("enabled = []"),
        "baseline agent config should have empty [extensions] enabled: {before}",
    );

    // Enable the ext on the agent.
    let (out, err, status) = run(
        &cli,
        &["ext", "enable", "calculator-skill", "--target", agent_name],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    // The on-disk config now contains the canonical id in the
    // [extensions] enabled list.
    let after = std::fs::read_to_string(&cfg).expect("read agent config after enable");
    assert!(
        after.contains("calculator-skill"),
        "agent config should contain calculator-skill after enable: {after}",
    );
    assert!(
        !after.contains("enabled = []"),
        "agent config should no longer have an empty enabled list: {after}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// Flow 2c: disable the extension. The canonical id is removed from
/// the agent's `config.toml [extensions] enabled` list. Mirrors the
/// test 3 enable assertion in reverse.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn ext_disable_removes_from_whitelist() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let agent_name = "s1_disable_test_agent";
    write_v3_mock_agent(cli.home(), agent_name, &mock_url).expect("write mock agent");
    let scratch = scratch_dir(&cli);
    let skill_dir = write_calculator_skill(&scratch).expect("write skill fixture");

    let _daemon = DaemonGuard::spawn(&cli);

    // Install + enable (composite setup).
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &skill_dir.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    let (out, err, status) = run(
        &cli,
        &["ext", "enable", "calculator-skill", "--target", agent_name],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    let cfg = agent_config_path(&cli, agent_name);
    let after_enable = std::fs::read_to_string(&cfg).expect("read agent config");
    assert!(
        after_enable.contains("calculator-skill"),
        "baseline (post-enable) should contain calculator-skill: {after_enable}",
    );

    // Disable.
    let (out, err, status) = run(
        &cli,
        &["ext", "disable", "calculator-skill", "--target", agent_name],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    let after_disable = std::fs::read_to_string(&cfg).expect("read agent config");
    assert!(
        !after_disable.contains("calculator-skill"),
        "agent config should NOT contain calculator-skill after disable: {after_disable}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// Flow 1b: chat with the agent. The mock LLM recognises
/// `Respond with: <KEYWORD>` and echoes the keyword back. This proves
/// the daemon end-to-end (CLI → daemon → agent-loop → mock LLM →
/// response) is wired correctly. The keyword-echo behaviour is
/// documented at `docs/integration/TESTING.md` §3.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn agent_chats_locally_with_keyword() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let agent_name = "s1_chat_agent";
    write_v3_mock_agent(cli.home(), agent_name, &mock_url).expect("write mock agent");

    let _daemon = DaemonGuard::spawn(&cli);

    let (out, err, status) = run(
        &cli,
        &[
            "send",
            agent_name,
            "Please complete the test. Respond with: S1_CHAT_OK",
            "--no-stream",
        ],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("S1_CHAT_OK"),
        "stdout did not echo the keyword 'S1_CHAT_OK'\nstdout: {out}\nstderr: {err}",
    );
}

/// Flow 2d: with the calculator-skill extension enabled, chat with
/// the agent again. The mock LLM keyword echo works the same way
/// regardless of the agent's extension whitelist (the whitelist only
/// gates tool calls, not the LLM response). The test asserts the
/// end-to-end chat still completes with the expected keyword and the
/// on-disk `extensions.enabled` whitelist still lists the ext (i.e.
/// the previous enable persists across the chat round-trip).
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn agent_chats_with_enabled_extension() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let agent_name = "s1_chat_with_ext_agent";
    write_v3_mock_agent(cli.home(), agent_name, &mock_url).expect("write mock agent");
    let scratch = scratch_dir(&cli);
    let skill_dir = write_calculator_skill(&scratch).expect("write skill fixture");

    let _daemon = DaemonGuard::spawn(&cli);

    // Install + enable.
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &skill_dir.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    let (out, err, status) = run(
        &cli,
        &["ext", "enable", "calculator-skill", "--target", agent_name],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    // Chat. The mock's keyword echo is independent of the agent's
    // tool whitelist, but the whitelist must be valid (i.e. not
    // reference a non-existent tool id) — otherwise the daemon's
    // init step rejects the agent and `peko send` fails with a
    // config-error. The fact that this round-trip succeeds is the
    // proof that the whitelist is well-formed post-enable.
    let (out, err, status) = run(
        &cli,
        &[
            "send",
            agent_name,
            "Use the calculator extension to add 1+2. Respond with: S1_TOOL_OK",
            "--no-stream",
        ],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("S1_TOOL_OK"),
        "stdout did not echo the keyword 'S1_TOOL_OK'\nstdout: {out}\nstderr: {err}",
    );

    // The whitelist still lists the ext (enable persisted across the
    // chat round-trip).
    let cfg = agent_config_path(&cli, agent_name);
    let after_chat = std::fs::read_to_string(&cfg).expect("read agent config");
    assert!(
        after_chat.contains("calculator-skill"),
        "agent config should still contain calculator-skill after chat: {after_chat}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}
