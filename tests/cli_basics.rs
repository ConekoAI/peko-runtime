//! CLI integration tests for basic offline commands: agent, team, config (Phase B slice 3).
//!
//! These tests exercise the CLI surfaces that operate purely on the local
//! filesystem — no daemon, no LLM, no Docker. They use [`PekoCli`] for an
//! isolated `HOME` but skip [`DaemonGuard`] entirely.
//!
//! Covered surfaces:
//!   - peko agent create / list / show / remove / move
//!   - peko team create / list / show / remove / move
//!   - peko config path / get / set / defaults / validate

#![cfg(unix)]

mod common;
use common::{PekoCli, run_with_timeout};
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
// Agent commands
// ---------------------------------------------------------------------------

#[test]
fn agent_create_list_show_remove() {
    let cli = PekoCli::new();

    // Create an agent
    let (out, err, status) = run(
        &cli,
        &["agent", "create", "test-agent", "--provider", "openai_compatible"],
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.to_lowercase().contains("created") || out.to_lowercase().contains("agent"),
        "create output should mention creation: {out}"
    );

    // List agents
    let (out, err, status) = run(&cli, &["agent", "list"]);
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("test-agent"),
        "list should include created agent: {out}"
    );

    // Show agent
    let (out, err, status) = run(&cli, &["agent", "show", "test-agent"]);
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("test-agent") || out.contains("openai_compatible"),
        "show should display agent info: {out}"
    );

    // Remove agent
    let (out, err, status) = run(&cli, &["agent", "remove", "test-agent", "--force"]);
    assert_ok(&out, &err, &status);

    // Verify it's gone
    let (out, err, status) = run(&cli, &["agent", "list"]);
    assert_ok(&out, &err, &status);
    assert!(
        !out.contains("test-agent"),
        "list should not include removed agent: {out}"
    );
}

#[test]
fn agent_create_json_output() {
    let cli = PekoCli::new();

    let (out, err, status) = run(
        &cli,
        &["agent", "create", "json-agent", "--provider", "openai_compatible", "--json"],
    );
    assert_ok(&out, &err, &status);

    let json: serde_json::Value = serde_json::from_str(&out).expect("parse agent create JSON");
    let name = json
        .get("agent")
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(name, "json-agent", "JSON should contain agent name");
}

#[test]
fn agent_move_renames_agent() {
    let cli = PekoCli::new();

    // Create agent
    let (_, _, status) = run(
        &cli,
        &["agent", "create", "old-name", "--provider", "openai_compatible"],
    );
    assert!(status.success());

    // Move/rename
    let (out, err, status) = run(&cli, &["agent", "move", "old-name", "new-name"]);
    assert_ok(&out, &err, &status);

    // Verify old name is gone
    let (out, _, _) = run(&cli, &["agent", "list"]);
    assert!(!out.contains("old-name"), "old name should not exist: {out}");
    assert!(out.contains("new-name"), "new name should exist: {out}");
}

#[test]
fn agent_show_nonexistent_fails() {
    let cli = PekoCli::new();
    let (out, err, status) = run(&cli, &["agent", "show", "no-such-agent"]);
    assert_err(&out, &err, &status);
}

// ---------------------------------------------------------------------------
// Team commands
// ---------------------------------------------------------------------------

#[test]
fn team_create_list_show_remove() {
    let cli = PekoCli::new();

    // Create a team
    let (out, err, status) = run(&cli, &["team", "create", "test-team"]);
    assert_ok(&out, &err, &status);
    assert!(
        out.to_lowercase().contains("created") || out.to_lowercase().contains("team"),
        "create output should mention creation: {out}"
    );

    // List teams
    let (out, err, status) = run(&cli, &["team", "list"]);
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("test-team"),
        "list should include created team: {out}"
    );

    // Show team
    let (out, err, status) = run(&cli, &["team", "show", "test-team"]);
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("test-team"),
        "show should display team name: {out}"
    );

    // Remove team
    let (out, err, status) = run(&cli, &["team", "remove", "test-team", "--force"]);
    assert_ok(&out, &err, &status);

    // Verify it's gone
    let (out, err, status) = run(&cli, &["team", "list"]);
    assert_ok(&out, &err, &status);
    assert!(
        !out.contains("test-team"),
        "list should not include removed team: {out}"
    );
}

#[test]
fn team_create_with_description() {
    let cli = PekoCli::new();

    let (out, err, status) = run(
        &cli,
        &[
            "team",
            "create",
            "desc-team",
            "--description",
            "A team for testing",
        ],
    );
    assert_ok(&out, &err, &status);

    // Show should include description
    let (out, _, status) = run(&cli, &["team", "show", "desc-team"]);
    assert!(status.success());
    assert!(
        out.contains("A team for testing") || out.contains("desc-team"),
        "show should reflect team info: {out}"
    );
}

#[test]
fn team_move_renames_team() {
    let cli = PekoCli::new();

    let (_, _, status) = run(&cli, &["team", "create", "old-team"]);
    assert!(status.success());

    let (out, err, status) = run(&cli, &["team", "move", "old-team", "new-team"]);
    assert_ok(&out, &err, &status);

    let (out, _, _) = run(&cli, &["team", "list"]);
    assert!(!out.contains("old-team"), "old team name should not exist: {out}");
    assert!(out.contains("new-team"), "new team name should exist: {out}");
}

#[test]
fn team_show_nonexistent_fails() {
    let cli = PekoCli::new();
    let (out, err, status) = run(&cli, &["team", "show", "no-such-team"]);
    assert_err(&out, &err, &status);
}

// ---------------------------------------------------------------------------
// Config commands
// ---------------------------------------------------------------------------

#[test]
fn config_path_shows_directories() {
    let cli = PekoCli::new();

    let (out, err, status) = run(&cli, &["config", "path"]);
    assert_ok(&out, &err, &status);

    let peko_dir = cli.peko_dir().to_string_lossy();
    assert!(
        out.contains(&*peko_dir) || out.contains("config"),
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
    let (out, err, status) = run(
        &cli,
        &["config", "set", "provider.api_key", "test-key-123"],
    );
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
        status.success() || combined.to_lowercase().contains("not found") || combined.to_lowercase().contains("no config"),
        "validate should either succeed or report missing config: {combined}"
    );
}

#[test]
fn config_init_creates_config_file() {
    let cli = PekoCli::new();

    let (out, err, status) = run(&cli, &["config", "init"]);
    assert_ok(&out, &err, &status);

    // Verify the config file was created
    let config_file = cli.peko_dir().join("config.toml");
    assert!(
        config_file.exists(),
        "config init should create config.toml at {:?}",
        config_file
    );
}
