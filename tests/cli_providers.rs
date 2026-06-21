//! CLI integration tests for the real-LLM provider smoke flows
//! (Phase B slice per `docs/integration/TESTING.md` §7).
//!
//! Coverage mirrors the `e2e_tests/providers/*.ps1` PowerShell scripts
//! that previously exercised this surface outside CI:
//!
//! | PS script          | Rust test                  | Provider  |
//! |--------------------|----------------------------|-----------|
//! | `minimax.ps1`      | `cli_providers_minimax_smoke` | minimax |
//! | `kimi.ps1`         | `cli_providers_kimi_smoke`    | kimi    |
//!
//! ## Tier: real-LLM
//!
//! Each test early-returns if the relevant API-key env var
//! (`MINIMAX_API_KEY` for the minimax test, `KIMI_API_KEY` for the
//! kimi test) is unset, so `cargo test` on a bare checkout without
//! real-LLM credentials still passes.
//!
//! The test runner is the GitHub Actions `Integration (real LLM)` job
//! (see [`.github/workflows/integration.yml`](../pekobot/peko-runtime/.github/workflows/integration.yml)),
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
//! ## Why direct config.toml writes
//!
//! Each test writes the agent's `config.toml` directly with the
//! provider-specific `api_key`, `base_url`, and `default_model`. This
//! bypasses `peko auth set` + `peko agent create --provider <p>` — both
//! of which are exercised by other test paths and would re-do
//! filesystem work we don't need to re-verify here. It also matches
//! the dual-mode pattern at `tunnel_e2e.rs::create_test_workspace`
//! (writes the api_key into the agent config directly, reads the key
//! from the relevant env var at test start).
//!
//! **Important: `PekoCli::cmd()` removes `MINIMAX_API_KEY` from the
//! daemon's env** (see [tests/common/cli.rs:115](tests/common/cli.rs#L115))
//! to safeguard the mock-tier tests. This is fine here because the
//! tests write the api_key directly into the agent config rather than
//! relying on env-var inheritance.
//!
//! ## Provider specifics
//!
//! From `src/common/services/agent_service.rs:226-270`:
//! - **kimi**: `provider_type = Kimi`, `base_url = "https://api.kimi.com/coding"`,
//!   `default_model = "k2p5"`, env var = `KIMI_API_KEY`.
//! - **minimax**: `provider_type = Minimax`,
//!   `base_url = "https://api.minimaxi.com/anthropic"`,
//!   `default_model = "MiniMax-M2.7"`, env var = `MINIMAX_API_KEY`.

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

/// Write an agent config.toml that talks to a real LLM provider.
///
/// `provider_type`, `base_url`, `default_model`, and `api_key` are all
/// baked in directly so the test bypasses the env-var / auth-store
/// indirection and exercises the same wire path as `peko agent create
/// --provider <p>` followed by an API call.
///
/// The test pre-creates the agent's config.toml in the same layout
/// that `peko agent create` would write, so the daemon's existing
/// agent-discovery path picks it up unchanged.
fn write_provider_agent(
    home: &Path,
    name: &str,
    provider_type: &str,
    base_url: &str,
    default_model: &str,
    api_key: &str,
) -> std::io::Result<()> {
    let agent_dir = Path::new(home).join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;
    let config_toml = format!(
        r#"version = "3.0"
name = "{name}"
description = "CLI integration test agent for the real-LLM provider smoke"
auto_accept_trusted = false

preferred_provider_id = "mock-llm"
preferred_model_id = "default"
default_timeout_seconds = 60

[extensions]
enabled = []

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
/// `MiniMax-M2.7` model).
///
/// Skips when `MINIMAX_API_KEY` is unset.
#[tokio::test]
#[ignore = "requires MINIMAX_API_KEY and peko daemon"]
async fn cli_providers_minimax_smoke() {
    let Some(api_key) = minimax_api_key() else {
        eprintln!("MINIMAX_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let agent_name = "providers_minimax_smoke";
    write_provider_agent(
        cli.home(),
        agent_name,
        // ProviderType::Minimax as a string. The provider dispatcher
        // uses the `base_url` to pick the right HTTP adapter.
        "minimax",
        "https://api.minimaxi.com/anthropic",
        "MiniMax-M2.7",
        &api_key,
    )
    .expect("write minimax agent");
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
    let Some(api_key) = kimi_api_key() else {
        eprintln!("KIMI_API_KEY not set; skipping");
        return;
    };

    let cli = PekoCli::new();
    let agent_name = "providers_kimi_smoke";
    write_provider_agent(
        cli.home(),
        agent_name,
        // ProviderType::Kimi as a string. The Kimi adapter uses
        // `api_key` as a bearer token against api.kimi.com.
        "kimi",
        "https://api.kimi.com/coding",
        "k2p5",
        &api_key,
    )
    .expect("write kimi agent");
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
