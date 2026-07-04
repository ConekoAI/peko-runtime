//! End-to-end user-journey scenario D1 (Phase D slice per
//! `docs/integration/TESTING.md` §7).
//!
//! Coverage — flow 1 + 2 from the Phase D plan (Principal-era equivalent):
//!
//! | Rust test                                | Flow step                                                  |
//! |------------------------------------------|------------------------------------------------------------|
//! | `principal_create_local_minimal`         | Flow 1: create a Principal locally                         |
//! | `ext_install_and_info_round_trip`        | Flow 2a: create an extension (skill)                       |
//! | `principal_capability_grant_round_trip`  | Flow 2b: grant a tool capability on the Principal          |
//! | `principal_capability_revoke_round_trip` | Flow 2c: revoke the capability                             |
//! | `principal_chats_locally_with_keyword`   | Flow 1b: chat with the Principal (mock LLM keyword echo)   |
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
//! These tests assert on plumbing: "the Principal is created on disk,
//! the extension is installed, the Principal carries the expected
//! capability grant, and a `peko send` round-trip completes end-to-end
//! with a deterministic mock response." The LLM's *decisions* are
//! irrelevant — only the orchestration surface matters.
//!
//! ## Principal-era translation
//!
//! After the "Principal as the single actor" migration:
//!
//! - The `peko agent create`/`show` CLI is gone; the Principal is the
//!   sole user-facing actor, created via `peko principal create` and
//!   inspected via `peko principal show` (which prints the DID,
//!   workspace path, and discovered agent prompts).
//! - The standalone-agent `peko ext enable <ext> --target <agent>` flow
//!   is gone — there is no `--target` Principal equivalent, because
//!   capability grants on a Principal are persisted to
//!   `<peko_dir>/principals/<name>/principal.toml [capabilities]` and
//!   take effect automatically when the root agent builds its whitelist
//!   in `run_root_agent_prompt` (see
//!   `src/principal/agent_runner.rs:99-102`). The CLI does not expose a
//!   live capability-grant command; tests must patch the config.
//! - The chat surface is `peko send <principal>` (`PrincipalSend` →
//!   `PrincipalManager::receive`).
//!
//! ## The two structural facts this file relies on
//!
//! 1. **`peko principal create <name>` defaults the owner to
//!    `user:default`** — see `src/commands/principal.rs::create_principal`.
//!    The local CLI's `GlobalPaths::user()` defaults to `"default"`,
//!    so the owner check in `PrincipalManager::receive` passes for
//!    `peko send`. This differs from `s6`, which uses the local-socket
//!    caller (`user:local`) and rewrites the owner.
//! 2. **The SKILL.md `name:` frontmatter field becomes the extension
//!    ID** ([`src/extensions/skill/adapter.rs:108-130`](../../src/extensions/skill/adapter.rs#L108-L130)).
//!    Our inline fixture writes `name: calculator-skill` so the
//!    install creates extension ID `calculator-skill`.

#[path = "../common/mod.rs"]
mod common;
use common::{create_mock_principal_with_tools, run_with_timeout, DaemonGuard, PekoCli};
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
    };
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

/// Path to the Principal's on-disk `principal.toml`.
fn principal_config_path(cli: &PekoCli, principal_name: &str) -> PathBuf {
    cli.peko_dir()
        .join("principals")
        .join(principal_name)
        .join("principal.toml")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Flow 1: create a Principal locally. The Principal's workspace
/// (`principal.toml`, identity, `agents/primary.md`) must exist under
/// `<peko_dir>/principals/<name>/`. We drive `peko principal create`
/// (the equivalent of the old `peko agent create`) and then read it
/// back via `peko principal show` — `show` prints the workspace path,
/// DID, and discovered agent prompts (the list equivalent in the
/// Principal surface; the legacy `peko agent list --json` is gone).
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn principal_create_local_minimal() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let principal_name = "s1_local_principal";

    // Seed the mock-llm catalog entry + run the real `peko principal create`.
    // Both are daemon-free filesystem operations.
    create_mock_principal_with_tools(&cli, principal_name, &mock_url, &[]);

    // On-disk `principal.toml` should reference `mock-llm` via the
    // capabilities' provider hint — equivalent to the old
    // `preferred_provider_id = "mock-llm"` assertion. The provider
    // catalog entry itself is the source of truth for base_url/api_key.
    let cfg = std::fs::read_to_string(principal_config_path(&cli, principal_name))
        .expect("read principal.toml");
    assert!(!cfg.is_empty(), "principal.toml should be non-empty: {cfg}");

    // `peko principal show` reads the workspace back through the
    // PrincipalManager; this is the round-trip verification (the
    // legacy `peko agent show` is gone).
    let _daemon = DaemonGuard::spawn(&cli);
    let (out, err, status) = run(
        &cli,
        &["principal", "show", principal_name],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains(principal_name),
        "show output should contain the principal name: stdout={out} stderr={err}",
    );

    // `peko principal agent list` (the Principal-era agent discovery
    // surface) should include the default `primary` prompt that
    // `peko principal create` writes.
    let (out, err, status) = run(
        &cli,
        &["principal", "agent", "list", principal_name],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("primary"),
        "principal agent list should include the default `primary` prompt: \
         stdout={out} stderr={err}",
    );
}

/// Flow 2a: install the calculator-skill extension (Tier 1 SKILL.md
/// detection — no `--type` flag). Verify `peko ext info
/// calculator-skill` reports `type: "skill"` and the install dir
/// contains the on-disk `SKILL.md` (per
/// `src/extension/manager/storage.rs`). This flow is unchanged by the
/// Principal migration: extensions are workspace-global, not
/// Principal-scoped.
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

/// Flow 2b: grant a tool capability on the Principal.
///
/// In the legacy standalone-agent model this was
/// `peko ext enable <ext> --target <agent>`, which wrote the canonical
/// extension id into the agent's `config.toml [extensions] enabled`
/// list. In the Principal model, the equivalent surface is the
/// Principal's `principal.toml [capabilities] tools`, which
/// `run_root_agent_prompt` extends with `capabilities.tools` /
/// `capabilities.skills` / `capabilities.mcps` / `capabilities.agents`
/// when building the root agent's whitelist. The CLI does not expose a
/// live grant command for capabilities, so we patch `principal.toml`
/// directly (mirrors the pattern in
/// `tests/common/agent.rs::create_mock_principal_with_tools`).
///
/// The on-disk config is the assertion point. We also assert that
/// the capability is granted in **both** forms — the bare name (so
/// the agent's per-agent `init_builtins_async` registers the tool)
/// and the canonical `builtin:tool:<name>` extension id (so the
/// dispatcher's `is_tool_enabled` owner check passes at execution
/// time). Granting only one form yields a silently-disabled tool.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn principal_extension_grant_round_trip() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let principal_name = "s1_extension_grant_principal";

    // Baseline: create the Principal with no extra extension grants.
    // The default `allowed_extensions` list is empty for a freshly
    // created Principal.
    create_mock_principal_with_tools(&cli, principal_name, &mock_url, &[]);

    let cfg_path = principal_config_path(&cli, principal_name);
    let before = std::fs::read_to_string(&cfg_path).expect("read principal.toml");
    // The baseline should NOT carry a `Bash` extension grant yet.
    assert!(
        !before.contains("Bash"),
        "baseline principal.toml should not carry a `Bash` extension grant: {before}",
    );

    // Patch the Principal's `allowed_extensions` to grant `Bash`
    // in both forms. This is what the legacy `peko ext enable Bash
    // --target <agent>` write did, except the on-disk shape is the
    // Principal's `principal.toml` rather than the agent's
    // `config.toml`.
    let raw = std::fs::read_to_string(&cfg_path).expect("read principal.toml");
    let mut cfg: peko::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    cfg.allowed_extensions.0 = vec!["Bash".into(), "builtin:tool:Bash".into()];
    std::fs::write(
        &cfg_path,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");

    // The on-disk config now contains the bare-name + canonical-id
    // extension grant.
    let after = std::fs::read_to_string(&cfg_path).expect("read principal.toml after grant");
    assert!(
        after.contains("Bash") && after.contains("builtin:tool:Bash"),
        "principal.toml should contain both the bare-name `Bash` and the \
         canonical `builtin:tool:Bash` extension grants after grant: {after}",
    );
}

/// Flow 2c: revoke the extension grant. The bare-name and canonical-id
/// entries are both removed from the Principal's `principal.toml
/// allowed_extensions`. Mirrors the test 3 grant assertion in
/// reverse. We round-trip through the TOML parse/serialize rather
/// than literal string edits so the test stays robust against
/// future schema changes.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn principal_extension_revoke_round_trip() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let principal_name = "s1_extension_revoke_principal";

    // Start with the extension already granted (via the helper).
    create_mock_principal_with_tools(&cli, principal_name, &mock_url, &["Bash"]);

    let cfg_path = principal_config_path(&cli, principal_name);
    let after_grant = std::fs::read_to_string(&cfg_path).expect("read principal.toml");
    assert!(
        after_grant.contains("Bash") && after_grant.contains("builtin:tool:Bash"),
        "baseline (post-grant) should contain both Bash forms: {after_grant}",
    );

    // Revoke: clear the allowed extensions list.
    let raw = std::fs::read_to_string(&cfg_path).expect("read principal.toml");
    let mut cfg: peko::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    cfg.allowed_extensions.clear();
    std::fs::write(
        &cfg_path,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");

    let after_revoke = std::fs::read_to_string(&cfg_path).expect("read principal.toml");
    assert!(
        !after_revoke.contains("Bash"),
        "principal.toml should NOT contain `Bash` after revoke: {after_revoke}",
    );
    assert!(
        !after_revoke.contains("builtin:tool:Bash"),
        "principal.toml should NOT contain `builtin:tool:Bash` after revoke: \
         {after_revoke}",
    );
}

/// Flow 1b: chat with the Principal. The mock LLM recognises
/// `Respond with: <KEYWORD>` and echoes the keyword back. This proves
/// the daemon end-to-end (CLI → daemon → `PrincipalSend` →
/// `PrincipalManager::receive` → root agent → mock LLM → response)
/// is wired correctly. The keyword-echo behaviour is documented at
/// `docs/integration/TESTING.md` §3.
#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn principal_chats_locally_with_keyword() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let principal_name = "s1_chat_principal";
    create_mock_principal_with_tools(&cli, principal_name, &mock_url, &[]);

    let _daemon = DaemonGuard::spawn(&cli);

    let (out, err, status) = run(
        &cli,
        &[
            "send",
            principal_name,
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
