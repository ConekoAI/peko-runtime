//! CLI integration tests for basic commands: principal and config (Phase B slice 3).
//!
//! Principal create uses the ADR-054 `--detach` form (blocking create waits
//! for a real genesis turn, which an offline test cannot provide); the
//! lifecycle assertions stay filesystem-level. Config commands run offline.
//!
//! (Was `#![cfg(unix)]`; dropped with the Windows named-pipe transport
//!  landing — see ADR-038.)

mod common;
use common::{run_with_timeout, PekoCli};
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn run(cli: &PekoCli, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        args,
        Duration::from_secs(10),
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

fn assert_err(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert!(
        !status.success(),
        "expected failure but succeeded\nstdout: {stdout}\nstderr: {stderr}",
    );
}

// ---------------------------------------------------------------------------
// Principal commands (create/list/show lifecycle)
// ---------------------------------------------------------------------------
//
// The standalone `peko agent create/list/show/remove/move` CRUD surface was
// removed in the "Principal as the single actor" migration. The Principal is
// now the sole user-facing actor; these tests cover its create/list/show
// lifecycle.
//
// ADR-054 made `principal create` a BLOCKING command (provision → boot →
// wait for the one-shot genesis turn), so the bare form is neither offline
// nor fast: it spawns a daemon and waits ≥60s for a genesis turn that
// needs a reachable model. The ADR's escape hatch for scripts/CI is
// `--detach`: provisioning (workspace, agents, kb scaffold, identity,
// cron seeds) still happens synchronously, but the command returns without
// waiting for the genesis turn. These tests use `--detach` and stop the
// spawned sidecar at the end; a placeholder mock-llm URL is fine — the
// endpoint is never dialed. There is no `principal remove`/`move` command,
// so the removal/rename cases (and the legacy `--json`/`--provider` create
// flags, which Principal create does not expose) are dropped rather than
// rewritten.

#[test]
fn principal_create_list_show() {
    let cli = PekoCli::new();

    // Create requires `--model` and validates it against the catalog.
    common::agent::seed_mock_provider_in_catalog(cli.home(), "http://127.0.0.1:9/v1");

    // Create a Principal — provisions the workspace, `agents/primary.md`,
    // the kb scaffold (ADR-055), identity, and `principal.toml`; the
    // `--detach` form returns once the daemon sidecar has the principal.
    // 30s budget: with no daemon up, create spawns the sidecar first.
    let (create_res, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        &[
            "principal",
            "create",
            "test-principal",
            "--model",
            "mock-llm",
            "--detach",
        ],
        Duration::from_secs(30),
    )
    .expect("run peko command");
    let out = String::from_utf8_lossy(&create_res.stdout).into_owned();
    let err = String::from_utf8_lossy(&create_res.stderr).into_owned();
    let status = create_res.status;
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("test-principal"),
        "create should mention the created principal name: {out}"
    );

    // List Principals.
    let (out, err, status) = run(&cli, &["principal", "list"]);
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("test-principal"),
        "list should include created principal: {out}"
    );

    // Show Principal.
    let (out, err, status) = run(&cli, &["principal", "show", "test-principal"]);
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("test-principal"),
        "show should display principal info: {out}"
    );

    // Stop the sidecar the create spawned (no DaemonGuard in this test —
    // best-effort; a lingering daemon would only hold this test's
    // isolated PEKO_HOME socket).
    let _ = run(&cli, &["daemon", "stop"]);
}

#[test]
fn principal_show_nonexistent_fails() {
    let cli = PekoCli::new();
    let (out, err, status) = run(&cli, &["principal", "show", "no-such-principal"]);
    assert_err(&out, &err, &status);
}

// ---------------------------------------------------------------------------
// Config commands (offline — no daemon needed)
// ---------------------------------------------------------------------------

#[test]
fn config_path_shows_directories() {
    let cli = PekoCli::new();

    let (out, err, status) = run(&cli, &["config", "path"]);
    assert_ok(&out, &err, &status);

    let peko_dir = cli.peko_dir().to_string_lossy().into_owned();
    assert!(
        out.contains(&peko_dir) || out.contains("config"),
        "config path should mention config directories: {out}"
    );
}

#[test]
fn config_path_json_output() {
    let cli = PekoCli::new();

    let (out, err, status) = run(&cli, &["config", "path", "--json"]);
    assert_ok(&out, &err, &status);

    let json: serde_json::Value = serde_json::from_str(&out).expect("parse config path JSON");
    assert!(
        json.get("config_dir").is_some() || json.get("config_file").is_some(),
        "JSON should contain config_dir or config_file: {json}"
    );
}

#[test]
fn config_defaults_shows_default_values() {
    let cli = PekoCli::new();

    let (out, err, status) = run(&cli, &["config", "defaults"]);
    assert_ok(&out, &err, &status);
    assert!(
        !out.is_empty(),
        "config defaults should produce output: {out}"
    );
}

#[test]
fn config_set_and_get_roundtrip() {
    let cli = PekoCli::new();

    // Set a value
    let (out, err, status) = run(&cli, &["config", "set", "provider.api_key", "test-key-123"]);
    assert_ok(&out, &err, &status);

    // Get it back
    let (out, err, status) = run(&cli, &["config", "get", "provider.api_key"]);
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("test-key-123"),
        "get should return the set value: {out}"
    );
}

#[test]
fn config_validate_on_empty_config() {
    let cli = PekoCli::new();

    // Validate the default config (should be valid or report no config)
    let (out, err, status) = run(&cli, &["config", "validate"]);
    // validate may succeed or fail depending on whether a config file exists;
    // either way it should not panic.
    let combined = format!("{out}{err}");
    assert!(
        status.success()
            || combined.to_lowercase().contains("not found")
            || combined.to_lowercase().contains("no config"),
        "validate should either succeed or report missing config: {combined}"
    );
}

#[test]
fn config_init_creates_config_file() {
    let cli = PekoCli::new();

    let (out, err, status) = run(&cli, &["config", "init"]);
    assert_ok(&out, &err, &status);

    // PekoCli isolates the subprocess CWD to the temp HOME, so `config init`
    // should have written peko.toml there rather than the project root.
    let created = cli.home().join("peko.toml");
    assert!(
        created.exists(),
        "config init should create peko.toml in isolated CWD ({}): {out}",
        created.display()
    );
    assert!(
        out.to_lowercase().contains("created") || out.to_lowercase().contains("config"),
        "config init should report creating a config file: {out}"
    );
}
