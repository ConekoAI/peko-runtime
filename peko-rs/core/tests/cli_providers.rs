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
//! ## Principal-era target model
//!
//! After the "Principal as the single actor" migration, `peko send <name>`
//! targets a **Principal** (`PrincipalSend` → `PrincipalManager::receive`),
//! not a legacy `~/.peko/agents/<name>/` config. These tests therefore
//! create a Principal via the real `peko principal create` command, then
//! drive `peko send` against it.
//!
//! ## v3 provider catalog setup
//!
//! In the v3 provider model, the root agent only carries soft hints;
//! the actual provider metadata lives in `~/.peko/providers.toml`, and API
//! keys live in the OS keychain (or fall back to env vars under
//! `PEKO_TEST_RESOLVER_BOOTSTRAP=1` in CI).
//!
//! Each test:
//! 1. Creates a `PekoCli` with [`PekoCli::allow_real_llm_keys`] so the
//!    daemon keeps `MINIMAX_API_KEY` / `KIMI_API_KEY` and enables the
//!    env-var bootstrap.
//! 2. Seeds `providers.toml` with the minimax or kimi catalog entry as the
//!    SOLE entry, so the root agent's provider resolution falls through to
//!    it (last-resort "first enabled catalog entry" rule in `LlmResolver`).
//! 3. Creates the Principal with `peko principal create`.
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

/// Create a Principal that resolves to a real-LLM provider.
///
/// Unlike `common::agent::create_mock_principal`, this does NOT seed the
/// Create a Principal pinned to the seeded configured model. The
/// caller seeds the real endpoint (minimax/kimi) as the sole catalog
/// entry first and passes its configured model id — model-first
/// create requires `--model` and validates it against the catalog.
///
/// New Principals are created with an empty `[capabilities] grants`
/// list by default. Tests that need the root agent to call tools (e.g.
/// the native-tool-call test below) must grant them separately with
/// [`grant_tools_to_principal`].
///
/// Must be called BEFORE `DaemonGuard::spawn`: `peko principal create`
/// writes files directly and needs no daemon.
fn create_provider_principal(cli: &PekoCli, name: &str, model_id: &str) {
    let output = cli
        .cmd()
        .args(["principal", "create", name, "--model", model_id])
        .output()
        .expect("run `peko principal create`");
    assert!(
        output.status.success(),
        "`peko principal create {name} --model {model_id}` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Grant additional tools to a Principal created by
/// [`create_provider_principal`].
///
/// Tools are written into `principals/<name>/principal.toml` under
/// `[capabilities] grants` as `tool:<name>` (e.g. `tool:Read`).
fn grant_tools_to_principal(cli: &PekoCli, name: &str, tools: &[&str]) {
    let path = cli
        .peko_dir()
        .join("principals")
        .join(name)
        .join("principal.toml");
    let raw = std::fs::read_to_string(&path).expect("read principal.toml");
    let mut cfg: peko_principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");

    for tool in tools {
        cfg.capabilities.push(format!("tool:{tool}"));
    }

    std::fs::write(
        &path,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");
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
    let principal = "providers_minimax_smoke";
    common::agent::seed_minimax_provider_in_catalog(cli.home());
    create_provider_principal(&cli, principal, "minimax");
    let _daemon = DaemonGuard::spawn(&cli);

    // PS script: `peko send <principal> "Hello, can you tell me a short joke?"`
    let (out, err, status) = run(
        &cli,
        &[
            "send",
            principal,
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
/// the configured `kimi-for-coding` model).
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
    let principal = "providers_kimi_smoke";
    common::agent::seed_kimi_provider_in_catalog(cli.home());
    create_provider_principal(&cli, principal, "kimi");
    let _daemon = DaemonGuard::spawn(&cli);

    // PS script: `peko send <principal> "Hi"`
    let (out, err, status) = run(
        &cli,
        &["send", principal, "Hi", "--no-stream"],
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
/// executing `Read` and surfacing the file content in its final answer.
///
/// `Read` is granted to the Principal explicitly because newly-created
/// Principals have no tools by default.
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
    let principal = "providers_minimax_anthropic_tool_call";
    common::agent::seed_minimax_provider_in_catalog(cli.home());
    create_provider_principal(&cli, principal, "minimax");
    grant_tools_to_principal(&cli, principal, &["Read"]);

    // The daemon's `Read` resolves relative paths against the shared
    // workspaces root, so place the sentinel file there.
    let workspace = cli.peko_dir().join("data").join("workspaces");
    std::fs::create_dir_all(&workspace).expect("create workspaces root");
    std::fs::write(workspace.join("tool_test.txt"), "TOOL_TEST_SECRET_123")
        .expect("write sentinel file");

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = "Read the file tool_test.txt in your workspace and report its exact contents.";
    let (out, err, status) = run(
        &cli,
        &["send", principal, prompt, "--no-stream"],
        Duration::from_secs(120),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("TOOL_TEST_SECRET_123"),
        "expected the LLM to call Read and report the secret; \
         stdout={out:?} stderr={err:?}",
    );
}
