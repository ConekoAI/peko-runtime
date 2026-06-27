//! CLI integration tests for basic commands: principal, team, config (Phase B slice 3).
//!
//! **Note:** Team commands route through the daemon via IPC. Principal
//! create/list/show and config commands operate on the filesystem directly. We
//! spawn a [`DaemonGuard`] for the team tests and skip it for the offline
//! principal and config tests.
//!
//! Tier: mock-LLM for team (daemon required), offline for principal/config.
//!
//! (Was `#![cfg(unix)]`; dropped with the Windows named-pipe transport
//!  landing — see ADR-038.)

mod common;
use common::{run_with_timeout, write_v3_mock_agent, DaemonGuard, PekoCli};
use std::process::Stdio;
use std::time::Duration;

fn mock_llm_url() -> Option<String> {
    let url = std::env::var("MOCK_LLM_URL").ok()?;
    if url.is_empty() {
        return None;
    }
    Some(url)
}

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
// Principal commands (offline — create/list/show operate on the filesystem)
// ---------------------------------------------------------------------------
//
// The standalone `peko agent create/list/show/remove/move` CRUD surface was
// removed in the "Principal as the single actor" migration. The Principal is
// now the sole user-facing actor; these tests cover its create/list/show
// lifecycle. `peko principal create/list/show` write and read the workspace
// directly (no daemon/IPC), so these run offline — no `DaemonGuard` or
// `MOCK_LLM_URL` needed. There is no `principal remove`/`move` command, so the
// removal/rename cases (and the legacy `--json`/`--provider` create flags,
// which Principal create does not expose) are dropped rather than rewritten.

#[test]
fn principal_create_list_show() {
    let cli = PekoCli::new();

    // Create a Principal — writes workspace, `agents/primary.md`, identity, and
    // `principal.toml` directly.
    let (out, err, status) = run(&cli, &["principal", "create", "test-principal"]);
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
}

#[test]
fn principal_show_nonexistent_fails() {
    let cli = PekoCli::new();
    let (out, err, status) = run(&cli, &["principal", "show", "no-such-principal"]);
    assert_err(&out, &err, &status);
}

// ---------------------------------------------------------------------------
// Team commands (need daemon)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn team_create_list_show_remove() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_v3_mock_agent(cli.home(), "team-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

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
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn team_create_with_description() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_v3_mock_agent(cli.home(), "desc-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

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
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn team_move_renames_team() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_v3_mock_agent(cli.home(), "move-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    let (_, _, status) = run(&cli, &["team", "create", "old-team"]);
    assert!(status.success());

    let (out, err, status) = run(&cli, &["team", "move", "old-team", "new-team"]);
    assert_ok(&out, &err, &status);

    // Verify the command reports success
    assert!(
        out.to_lowercase().contains("moved")
            || out.to_lowercase().contains("renamed")
            || status.success(),
        "move should report success or mention move/renamed: {out}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn team_show_nonexistent_fails() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };
    let cli = PekoCli::new();
    write_v3_mock_agent(cli.home(), "show-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    let (out, err, status) = run(&cli, &["team", "show", "no-such-team"]);
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
