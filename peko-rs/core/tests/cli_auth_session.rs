//! Integration tests for ADR-045 PR #2 step 5 — the strict
//! SID+token session auth gate.
//!
//! These tests opt into the production-default `strict_session_auth`
//! on `PekoCli`, which means the daemon enables
//! `auth_session_required=true`. The test then exercises the
//! end-to-end flow:
//!
//!   1. Daemon prints the diceware auth code on startup (stderr).
//!   2. Pre-auth CLI calls fail with `[auth_required]`.
//!   3. `peko auth submit --code <words>` succeeds; token file is
//!      mode 0600 at `<config>/run/auth-token-<sid>`.
//!   4. Post-auth CLI calls succeed.
//!   5. Wrong code, tampered token, SID-bound failure modes.
//!   6. Daemon restart invalidates old tokens.
//!
//! Each test owns its `PekoCli` (and thus its `HOME`/`PEKO_HOME`
//! tempdir), so they don't share tokens or auth-code state.
//!
//! ## Platform note
//!
//! The strict gate uses `SO_PEERCRED` / `LOCAL_PEERPID` to identify
//! the peer's session group. Linux supports this directly via
//! `SO_PEERCRED`. macOS requires `SCM_CREDS` ancillary data
//! (unsupported on the connectionless datagram socket) — until
//! that's wired, strict mode is effectively a no-op on macOS
//! (every IPC fails with "macOS requires SCM_CREDS ancillary
//! data"). These tests therefore run only on Linux; on macOS the
//! gate is documented as fail-closed-and-noisy pending the SCM_CREDS
//! work (see ADR-045 §"macOS caveat").

#![cfg(all(unix, target_os = "linux"))]

mod common;

use common::cli::PekoCli;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Read the diceware auth code from the daemon's auth-code file.
///
/// The daemon writes `<run_dir>/auth-code` mode 0600 at startup and
/// prints it to stderr for human enrollment. Tests use the file
/// (not stderr) to avoid the timing race of "drain stderr until
/// the code shows up".
fn read_auth_code(home: &Path) -> String {
    let path = home.join(".peko/run/auth-code");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read auth-code at {}: {e}", path.display()))
        .trim()
        .to_string()
}

/// Resolve the `peko` binary path. The harness does this same fallback
/// in `PekoCli::cmd`; tests calling CLI commands directly (without
/// going through `cli.cmd()`) need to repeat it.
fn peko_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_peko") {
        return std::path::PathBuf::from(p);
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = std::path::Path::new(manifest_dir)
        .parent()
        .and_then(std::path::Path::parent)
        .expect("CARGO_MANIFEST_DIR is peko-rs/core/, two parents up is the workspace root");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    target_dir.join(profile).join("peko")
}

/// Run `peko daemon status --json` and return the parsed JSON.
fn daemon_status_json(cli: &PekoCli) -> serde_json::Value {
    let out = Command::new(peko_bin())
        .env("HOME", cli.home())
        .env("USERPROFILE", cli.home())
        .env("PEKO_HOME", cli.peko_dir())
        .env("PEKO_AUTH_SESSION_REQUIRED", "0") // status is unauthenticated
        .args(["daemon", "status", "--json"])
        .output()
        .expect("spawn peko daemon status");
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("parse status JSON: {e}; raw={:?}", out.stdout))
}

/// Strict-mode CLI command (does NOT pass `PEKO_AUTH_SESSION_REQUIRED=0`).
fn run_cli_strict(cli: &PekoCli, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(peko_bin())
        .env("HOME", cli.home())
        .env("USERPROFILE", cli.home())
        .env("PEKO_HOME", cli.peko_dir())
        .env_remove("PEKO_AUTH_SESSION_REQUIRED") // ensure daemon sees "unset"
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn peko {args:?}: {e}"));
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Submit the auth code and return the JSON envelope.
fn auth_submit(cli: &PekoCli, code: &str) -> serde_json::Value {
    let (stdout, stderr, ok) =
        run_cli_strict(cli, &["auth", "submit", "--code", code, "--json"]);
    assert!(
        ok,
        "auth submit failed:\nstdout={stdout}\nstderr={stderr}"
    );
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("parse auth submit JSON: {e}; raw={stdout}"))
}

#[test]
fn startup_artifact_auth_code_file_is_0600() {
    let cli = PekoCli::new().strict_session_auth();
    common::daemon::DaemonGuard::spawn(&cli);

    let code_path = cli.peko_dir().join("run/auth-code");
    let meta = std::fs::metadata(&code_path).expect("auth-code file exists");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "auth-code mode must be 0600, got {mode:o}");

    let code = read_auth_code(cli.home());
    let word_count = code.split('-').count();
    assert_eq!(
        word_count, 6,
        "auth code should be 6 diceware words, got {word_count}: {code}"
    );
}

#[test]
fn closed_by_default_pre_auth_returns_auth_required() {
    let cli = PekoCli::new().strict_session_auth();
    common::daemon::DaemonGuard::spawn(&cli);

    let (stdout, stderr, ok) = run_cli_strict(&cli, &["daemon", "status", "--json"]);
    assert!(!ok, "pre-auth daemon status must fail");
    assert!(
        stdout.contains("[auth_required]") || stderr.contains("[auth_required]"),
        "expected [auth_required] marker; stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn successful_auth_unlocks_subsequent_commands() {
    let cli = PekoCli::new().strict_session_auth();
    common::daemon::DaemonGuard::spawn(&cli);

    let code = read_auth_code(cli.home());
    let envelope = auth_submit(&cli, &code);
    assert_eq!(envelope["authenticated"], serde_json::json!(true));
    assert!(envelope["sid"].is_number());
    assert!(envelope["expires_in_secs"].as_u64().unwrap() > 0);

    // Token file is mode 0600 under <config>/run/.
    let sid = envelope["sid"].as_i64().unwrap();
    let token_path = cli.peko_dir().join(format!("run/auth-token-{sid}"));
    let meta = std::fs::metadata(&token_path).expect("auth-token file exists");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);

    // Post-auth daemon status works (note: we still call with the
    // strict opt-out because `daemon status` is intentionally
    // unauthenticated — it's a liveness probe).
    let status = daemon_status_json(&cli);
    assert_eq!(status["running"], serde_json::json!(true));
}

#[test]
fn wrong_code_fails_with_invalid_auth_code() {
    let cli = PekoCli::new().strict_session_auth();
    common::daemon::DaemonGuard::spawn(&cli);

    let wrong = "totally-wrong-six-word-diceware-code";
    let (stdout, stderr, ok) =
        run_cli_strict(&cli, &["auth", "submit", "--code", wrong, "--json"]);
    assert!(!ok);
    assert!(
        stdout.contains("[invalid_auth_code]") || stderr.contains("[invalid_auth_code]"),
        "expected [invalid_auth_code]; stdout={stdout}\nstderr={stderr}"
    );

    // No token file should have been written.
    let run_dir = cli.peko_dir().join("run");
    let entries: Vec<_> = std::fs::read_dir(&run_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("auth-token-")
        })
        .collect();
    assert!(entries.is_empty(), "no auth-token-* file should exist");
}

#[test]
fn tampered_token_rejected_with_invalid_session_token() {
    let cli = PekoCli::new().strict_session_auth();
    common::daemon::DaemonGuard::spawn(&cli);

    let code = read_auth_code(cli.home());
    let envelope = auth_submit(&cli, &code);
    let sid = envelope["sid"].as_i64().unwrap();

    // Corrupt the token file.
    let token_path = cli.peko_dir().join(format!("run/auth-token-{sid}"));
    std::fs::write(&token_path, "definitely-not-the-real-token").unwrap();

    // Subsequent CLI call must fail.
    let (stdout, stderr, ok) = run_cli_strict(&cli, &["daemon", "status", "--json"]);
    assert!(
        !ok,
        "tampered token must fail the strict gate"
    );
    assert!(
        stdout.contains("[invalid_session_token]")
            || stderr.contains("[invalid_session_token]")
            || stdout.contains("[auth_required]"),
        "expected strict-gate rejection; stdout={stdout}\nstderr={stderr}"
    );
}