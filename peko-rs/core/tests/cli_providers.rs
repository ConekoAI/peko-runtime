//! Real-LLM provider smoke tests: `peko send` → daemon → a real endpoint.
//!
//! Vendor-neutral: the endpoint comes from the environment (see
//! `common::real_llm` — `LLM_API_KEY`, `LLM_BASE_URL`, `LLM_MODEL`,
//! optional `LLM_API_FORMAT`). Each test skips when those are unset, so a
//! bare `cargo test` still passes.
//!
//! Tier: real-LLM, opt-in and local only (`make test-cli-providers` or
//! `make test-integration-llm`); CI does not run it. The mock-LLM tier
//! covers the same daemon paths deterministically.
//!
//! Each test seeds the env-described endpoint as the sole catalog entry,
//! creates a Principal pinned to it with `peko create --model`, and keeps
//! `LLM_API_KEY` in the daemon's environment via
//! [`PekoCli::allow_real_llm_keys`] (env-var key bootstrap, no keychain).

mod common;
use common::{run_with_timeout, DaemonGuard, PekoCli, RealLlm, REAL_LLM_MODEL_ID};
use std::process::Stdio;
use std::time::Duration;

/// A Principal pinned to the seeded real LLM. Call before spawning the
/// daemon: `peko create` writes files directly.
fn real_llm_principal(cli: &PekoCli, name: &str, llm: &RealLlm) {
    common::seed_real_llm_in_catalog(cli.home(), llm);
    let output = cli
        .cmd()
        .args(["create", name, "--model", REAL_LLM_MODEL_ID])
        .output()
        .expect("run `peko create`");
    assert!(
        output.status.success(),
        "`peko create {name}` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Run `peko send <principal> <prompt>`, asserting a zero exit; returns stdout.
fn send(cli: &PekoCli, principal: &str, prompt: &str, timeout: Duration) -> String {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        &["send", principal, prompt],
        timeout,
    )
    .expect("run peko send");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "peko send failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    stdout
}

/// A short prompt produces a non-empty reply.
#[tokio::test]
#[ignore = "requires LLM_API_KEY/LLM_BASE_URL/LLM_MODEL and peko daemon"]
async fn cli_providers_real_llm_smoke() {
    let Some(llm) = RealLlm::from_env() else {
        eprintln!("{}; skipping", RealLlm::missing_env());
        return;
    };
    let cli = PekoCli::new().allow_real_llm_keys();
    real_llm_principal(&cli, "providers_smoke", &llm);
    let _daemon = DaemonGuard::spawn(&cli);

    let out = send(
        &cli,
        "providers_smoke",
        "Reply with one short sentence.",
        Duration::from_secs(60),
    );
    assert!(!out.trim().is_empty(), "expected a non-empty reply");
}

/// The model emits a native tool call: it must Read a file whose contents
/// appear nowhere else, then report them.
#[tokio::test]
#[ignore = "requires LLM_API_KEY/LLM_BASE_URL/LLM_MODEL and peko daemon"]
async fn cli_providers_real_llm_native_tool_call() {
    let Some(llm) = RealLlm::from_env() else {
        eprintln!("{}; skipping", RealLlm::missing_env());
        return;
    };
    let cli = PekoCli::new().allow_real_llm_keys();
    real_llm_principal(&cli, "providers_tool_call", &llm);

    // Read resolves relative paths against the shared workspaces root.
    let workspace = cli.peko_dir().join("data").join("workspaces");
    std::fs::create_dir_all(&workspace).expect("create workspaces root");
    std::fs::write(workspace.join("tool_test.txt"), "TOOL_TEST_SECRET_123")
        .expect("write sentinel file");
    let _daemon = DaemonGuard::spawn(&cli);

    let out = send(
        &cli,
        "providers_tool_call",
        "Read the file tool_test.txt in your workspace and report its exact contents.",
        Duration::from_secs(120),
    );
    assert!(
        out.contains("TOOL_TEST_SECRET_123"),
        "expected the model to call Read and report the secret; stdout={out:?}",
    );
}
