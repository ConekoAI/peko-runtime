//! CLI integration tests for the real-LLM provider smoke flows
//! (Phase B slice per `docs/integration/TESTING.md` §7).
//!
//! Coverage mirrors the `e2e_tests/providers/*.ps1` PowerShell scripts
//! that previously exercised this surface outside CI:
//!
//! | PS script          | Rust test                                       | Provider  |
//! |--------------------|-------------------------------------------------|-----------|
//! | `minimax.ps1`      | `cli_providers_minimax_smoke`                   | minimax   |
//! | `kimi.ps1`         | `cli_providers_kimi_smoke`                      | kimi      |
//! | `minimax_tools.ps1`| `cli_providers_minimax_anthropic_native_tool_call` | minimax |
//!
//! ## Tier: real-LLM
//!
//! Each test early-returns if the relevant API-key env var
//! (`MINIMAX_API_KEY` for the minimax test, `KIMI_API_KEY` for the
//! kimi test) is unset, so `cargo test` on a bare checkout without
//! real-LLM credentials still passes.
//!
//! The test runner is the GitHub Actions `Integration (real LLM)` job
//! (see [`.github/workflows/integration.yml`](../peko/peko-runtime/.github/workflows/integration.yml)),
//! which only fires on:
//! 1. Nightly cron (line 37: `cron: '0 2 * * *'`),
//! 2. Manual `workflow_dispatch`, or
//! 3. A commit message containing `[llm]`.
//!
//! The test runner unsets `MOCK_LLM_URL` (per the `test-integration-llm`
//! Makefile recipe) and passes both `MINIMAX_API_KEY` and `KIMI_API_KEY`
//! as `secrets.*` env. The `MINIMAX_API_KEY` env is what the daemon's
//! `provider.api_key_env` lookup reads; `KIMI_API_KEY` is the same
//! path for Kimi.
//!
//! ## v3 provider catalog setup
//!
//! In the v3 provider model, agents only carry soft hints
//! (`preferred_provider_id` / `preferred_model_id`). The actual provider
//! metadata lives in `~/.peko/providers.toml`, and API keys live in the
//! OS keychain (or fall back to env vars under
//! `PEKO_TEST_RESOLVER_BOOTSTRAP=1` in CI).
//!
//! Each test:
//! 1. Creates a `PekoCli` with [`PekoCli::allow_real_llm_keys`] so the
//!    daemon keeps `MINIMAX_API_KEY` / `KIMI_API_KEY` and enables the
//!    env-var bootstrap.
//! 2. Seeds `providers.toml` with the minimax or kimi catalog entry.
//! 3. Writes the agent config with `preferred_provider_id` pointing at
//!    that entry.
//!
//! This bypasses `peko auth set` + `peko provider add`, both of which
//! are exercised by other test paths.
//!
//! ## Provider specifics
//!
//! - **kimi**: catalog id `kimi`, `base_url = "https://api.kimi.com/coding"`,
//!   `default_model = "kimi-for-coding"`, env var = `KIMI_API_KEY`.
//! - **minimax**: catalog id `minimax`,
//!   `base_url = "https://api.minimaxi.com/anthropic"`,
//!   `default_model = "MiniMax-M3"`, env var = `MINIMAX_API_KEY`.

mod common;
use common::{run_with_timeout, DaemonGuard, PekoCli};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `KIMI_API_KEY` env, return Some(key) if set and non-empty, None
/// otherwise. Tests early-return on None so `cargo test` on a bare
/// checkout still passes.
fn kimi_api_key() -> Option<String> {
    let k = std::env::var("KIMI_API_KEY").ok()?;
    if k.is_empty() {
        return None;
    }
    Some(k)
}

/// Read `MINIMAX_API_KEY` env, return Some(key) if set and non-empty.
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

/// Write an agent config.toml that references a v3 provider catalog
/// entry. The actual provider metadata (base_url, default_model) and
/// API key live in `~/.peko/providers.toml` and are seeded separately
/// before the daemon starts.
fn write_provider_agent(home: &Path, name: &str, provider_id: &str) -> std::io::Result<()> {
    write_tool_agent(home, name, provider_id, &[])
}

/// Write an agent config.toml that references a v3 provider catalog
/// entry and whitelists a set of tools (bare names + canonical
/// `builtin:tool:<name>` IDs).
fn write_tool_agent(
    home: &Path,
    name: &str,
    provider_id: &str,
    extra_tools: &[&str],
) -> std::io::Result<()> {
    let agent_dir = Path::new(home).join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;

    let mut enabled = vec![
        "builtin:tool:read_file".to_string(),
        "read_file".to_string(),
    ];
    enabled.extend(extra_tools.iter().map(|s| s.to_string()));
    let enabled_toml = enabled
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");

    let config_toml = format!(
        r#"version = "3.0"
name = "{name}"
description = "CLI integration test agent for the real-LLM provider smoke"
auto_accept_trusted = false

preferred_provider_id = "{provider_id}"
preferred_model_id = "default"
default_timeout_seconds = 300

[extensions]
enabled = [{enabled_toml}]

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}
"#
    );
    std::fs::write(agent_dir.join("config.toml"), config_toml)?;
    std::fs::write(agent_dir.join("SYSTEM.md"), "")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `e2e_tests/providers/minimax.ps1` — end-to-end smoke against the
/// MiniMax (Anthropic-compatible) provider. Sends a short prompt and
/// asserts the response is non-empty (a real LLM call to
/// `https://api.minimaxi.com/anthropic` with the configured
/// `MiniMax-M3` model).
///
/// Skips when `MINIMAX_API_KEY` is unset.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn cli_providers_minimax_smoke() {
    let Some(_api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new().allow_real_llm_keys();
    let agent_name = "providers_minimax_smoke";
    common::agent::seed_minimax_provider_in_catalog(cli.home());
    write_provider_agent(cli.home(), agent_name, "minimax").expect("write minimax agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // PS script: `peko send <agent> "Hello, can you tell me a short joke?"`
    let (out, err, status) = run(
        &cli,
        &[
            "send",
            agent_name,
            "Hello, can you tell me a short joke?",
            "--no-stream",
        ],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        !out.trim().is_empty(),
        "expected non-empty response from minimax, got: stdout={out:?} stderr={err:?}",
    );
}

/// `e2e_tests/providers/kimi.ps1` — end-to-end smoke against the
/// Kimi provider. Sends a short prompt and asserts the response is
/// non-empty (a real LLM call to `https://api.kimi.com/coding` with
/// the configured `k2p5` model).
///
/// Skips when `KIMI_API_KEY` is unset.
#[tokio::test]
#[ignore = "requires KIMI_API_KEY and peko daemon"]
async fn cli_providers_kimi_smoke() {
    let Some(_api_key) = kimi_api_key() else {
        eprintln!("KIMI_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new().allow_real_llm_keys();
    let agent_name = "providers_kimi_smoke";
    common::agent::seed_kimi_provider_in_catalog(cli.home());
    write_provider_agent(cli.home(), agent_name, "kimi").expect("write kimi agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // PS script: `peko send <agent> "Hi"`
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, "Hi", "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        !out.trim().is_empty(),
        "expected non-empty response from kimi, got: stdout={out:?} stderr={err:?}",
    );
}

/// Anthropic-format native tool calling smoke test.
///
/// MiniMax exposes an Anthropic-compatible chat-completion endpoint at
/// `https://api.minimaxi.com/anthropic`. This test drives the full
/// agentic loop with a real MiniMax model and asserts that the model
/// emits a native Anthropic-format `tool_use`/`tool_result` exchange,
/// executing `read_file` and surfacing the file content in its final
/// answer.
///
/// Skips when `MINIMAX_API_KEY` is unset.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn cli_providers_minimax_anthropic_native_tool_call() {
    let Some(_api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new().allow_real_llm_keys();
    let agent_name = "providers_minimax_anthropic_tool_call";
    common::agent::seed_minimax_provider_in_catalog(cli.home());
    write_tool_agent(cli.home(), agent_name, "minimax", &[]).expect("write minimax tool agent");

    // The daemon's `read_file` resolves relative paths against the shared
    // workspaces root, so place the sentinel file there.
    let workspace = cli.peko_dir().join("data").join("workspaces");
    std::fs::create_dir_all(&workspace).expect("create workspaces root");
    std::fs::write(workspace.join("tool_test.txt"), "TOOL_TEST_SECRET_123")
        .expect("write sentinel file");

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = "Read the file tool_test.txt in your workspace and report its exact contents.";
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, prompt, "--no-stream"],
        Duration::from_secs(120),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("TOOL_TEST_SECRET_123"),
        "expected the LLM to call read_file and report the secret; \
         stdout={out:?} stderr={err:?}",
    );
}
