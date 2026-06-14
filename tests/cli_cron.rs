//! CLI integration tests for `peko cron` (Phase B slice 4, per docs/integration/TESTING.md §7).
//!
//! Coverage mirrors the four `e2e_tests/cron/*.ps1` scripts that previously
//! exercised this surface outside CI:
//!
//! | PS script             | Rust tests                                                                 |
//! |-----------------------|----------------------------------------------------------------------------|
//! | `cron_basics.ps1`     | `cron_*_persists`, `cron_list_*`, `cron_remove_*`, `cron_history_*`         |
//! | `cron_execution.ps1`  | `cron_run_triggers_due_job`, `cron_announce_writes_file_on_run`            |
//! | `cron_agent_tool.ps1` | (deferred — requires agent + tool-call driving, see [tests/cli_a2a.rs]…)    |
//! | `cron_idle_event.ps1` | `cron_add_idle_does_not_panic`, `cron_add_event_does_not_panic`            |
//!
//! Each test:
//!   1. Builds an isolated [`PekoCli`] tempdir as `HOME`.
//!   2. Spawns a [`CronDaemonGuard`] — same lifecycle as `DaemonGuard` but
//!      starts the daemon with `--interval 1` so the cron poll cycle is
//!      fast enough to keep this under 30s/test.
//!   3. Runs `peko cron …` and asserts the IPC + on-disk result.
//!
//! Tier: mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set; locally either `make docker-up` or point
//! `MOCK_LLM_URL` at any mock instance). Tests early-return if unset
//! so `cargo test` still passes on a bare checkout.
//!
//! The daemon's IPC server binds a Unix domain socket on Unix and a
//! Windows named pipe on Windows (ADR-038). This file used to be
//! `#![cfg(unix)]`; the gate was dropped when the Windows transport
//! landed so the same tests run on both platforms. See
//! `docs/architecture/adr/ADR-038-named-pipes-on-windows.md` for the
//! Windows side of the story.

mod common;
use common::{PekoCli, run_with_timeout};
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Daemon with a 1s cron poll interval
// ---------------------------------------------------------------------------

/// Like [`common::DaemonGuard`] but starts the daemon with `--interval 1` so
/// the cron poll cycle is fast enough for tests that wait for jobs to fire.
struct CronDaemonGuard {
    child: std::process::Child,
}

impl CronDaemonGuard {
    fn spawn(cli: &PekoCli) -> Self {
        let child = cli
            .cmd()
            .args(["daemon", "start", "--foreground", "--interval", "1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn peko daemon start --foreground --interval 1");

        let mut guard = Self { child };
        guard.wait_ready(cli, Duration::from_secs(30));
        guard
    }

    fn wait_ready(&mut self, cli: &PekoCli, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        let mut last_status_json = String::new();
        loop {
            let output = common::try_run_with_timeout(
                || {
                    let mut c = cli.cmd();
                    c.args(["daemon", "status", "--json"])
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null());
                    c
                },
                &[],
                Duration::from_secs(6),
            );
            last_status_json = match &output {
                Ok((o, _, _)) if o.status.success() => {
                    String::from_utf8_lossy(&o.stdout).into_owned()
                }
                _ => last_status_json,
            };
            let running = match &output {
                Ok((o, _, _)) if o.status.success() => {
                    serde_json::from_slice::<serde_json::Value>(&o.stdout)
                        .ok()
                        .and_then(|v| v.get("running").and_then(|r| r.as_bool()))
                        .unwrap_or(false)
                }
                _ => false,
            };
            if running {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "peko daemon did not become ready in {timeout:?} (sock: {})\n\
                     --- last status JSON ---\n{last_status_json}\n--- end ---",
                    cli.daemon_endpoint(),
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for CronDaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn skip_if_no_mock() -> Option<()> {
    let url = std::env::var("MOCK_LLM_URL").ok()?;
    if url.is_empty() {
        return None;
    }
    Some(())
}

fn run(cli: &PekoCli, args: &[&str], timeout: Duration) -> (String, String, std::process::ExitStatus) {
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

fn assert_err(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert!(
        !status.success(),
        "expected failure but succeeded\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// Run `peko cron list --json` and return the parsed JSON array of jobs.
fn list_jobs_json(cli: &PekoCli) -> Vec<serde_json::Value> {
    let (out, err, status) = run(cli, &["cron", "list", "--json"], Duration::from_secs(10));
    assert_ok(&out, &err, &status);
    serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("cron list --json did not parse: {e}\nstdout: {out}"))
}

/// Remove every job whose name matches the prefix. Used for cleanup.
fn remove_jobs_with_prefix(cli: &PekoCli, prefix: &str) {
    let jobs = list_jobs_json(cli);
    for job in jobs {
        let Some(name) = job.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if name.starts_with(prefix) {
            let Some(id) = job.get("id").and_then(|i| i.as_str()) else {
                continue;
            };
            let _ = run(
                cli,
                &["cron", "remove", id, "--force"],
                Duration::from_secs(5),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CLI CRUD (mirrors cron_basics.ps1)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_list_empty_db() {
    if skip_if_no_mock().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);

    let (out, err, status) = run(&cli, &["cron", "list"], Duration::from_secs(10));
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("No cron jobs found") || out.to_lowercase().contains("no cron jobs"),
        "empty list should report no jobs, got: {out}"
    );

    let (json_out, err, status) = run(
        &cli,
        &["cron", "list", "--json"],
        Duration::from_secs(10),
    );
    assert_ok(&json_out, &err, &status);
    let arr: serde_json::Value = serde_json::from_str(&json_out)
        .unwrap_or_else(|e| panic!("cron list --json did not parse: {e}\nstdout: {json_out}"));
    let len = arr.as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(len, 0, "empty DB --json should be []: {json_out}");
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_cron_expression_persists() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-cron-");

    let name = "e2e-cron-cron-job";
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "add", "--name", name, "--schedule", "0 0 * * * *", "--message", "ping",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("Added") && out.contains("cron_"),
        "add output should say Added and surface the cron_ id, got: {out}"
    );

    let jobs = list_jobs_json(&cli);
    let added = jobs.iter().find(|j| j.get("name").and_then(|n| n.as_str()) == Some(name));
    assert!(added.is_some(), "added cron-expression job not in list: {jobs:?}");

    let job = added.unwrap();
    assert!(
        job.get("id").and_then(|i| i.as_str()).unwrap_or("").starts_with("cron_"),
        "job id should start with cron_: {job:?}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_at_persists_with_delete_after_run() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-at-");

    // Far-future timestamp so it does NOT fire during this test.
    let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let name = "e2e-cron-at-job";
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "at", "--name", name, "--at", &future, "--message", "ping",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("Added"),
        "at output should say Added, got: {out}"
    );

    let jobs = list_jobs_json(&cli);
    let added = jobs.iter().find(|j| j.get("name").and_then(|n| n.as_str()) == Some(name));
    assert!(added.is_some(), "added at-job not in list: {jobs:?}");

    // The CLI hard-codes `delete_after_run: true` for at-jobs (commands/cron.rs:329).
    let delete_after = added
        .unwrap()
        .get("delete_after_run")
        .and_then(|v| v.as_bool());
    assert_eq!(
        delete_after,
        Some(true),
        "at-jobs must be one-shot (delete_after_run=true): {added:?}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_every_persists() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-every-");

    let name = "e2e-cron-every-job";
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "every", "--name", name, "--interval-ms", "60000", "--message", "tick",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("Added"),
        "every output should say Added, got: {out}"
    );

    let jobs = list_jobs_json(&cli);
    assert!(
        jobs.iter().any(|j| j.get("name").and_then(|n| n.as_str()) == Some(name)),
        "added every-job not in list: {jobs:?}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_idle_persists() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-idle-");

    let name = "e2e-cron-idle-job";
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "add-idle", "--name", name, "--minutes", "1", "--message", "wakeup",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("Added"),
        "add-idle output should say Added, got: {out}"
    );

    let jobs = list_jobs_json(&cli);
    assert!(
        jobs.iter().any(|j| j.get("name").and_then(|n| n.as_str()) == Some(name)),
        "added idle-job not in list: {jobs:?}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_event_persists() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-event-");

    let name = "e2e-cron-event-job";
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "add-event", "--name", name, "--event-type", "internal", "--once",
            "--message", "react",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("Added"),
        "add-event output should say Added, got: {out}"
    );

    let jobs = list_jobs_json(&cli);
    assert!(
        jobs.iter().any(|j| j.get("name").and_then(|n| n.as_str()) == Some(name)),
        "added event-job not in list: {jobs:?}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_list_json_returns_added_count() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-count-");

    // Add three jobs with distinct schedule kinds.
    let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    for kind_args in [
        vec!["add", "--name", "e2e-cron-count-a", "--schedule", "0 0 * * * *", "--message", "a"],
        vec!["at", "--name", "e2e-cron-count-b", "--at", &future, "--message", "b"],
        vec!["every", "--name", "e2e-cron-count-c", "--interval-ms", "60000", "--message", "c"],
    ] {
        let mut full = vec!["cron"];
        full.extend(kind_args.iter().copied());
        let (out, err, status) = run(&cli, &full, Duration::from_secs(10));
        assert_ok(&out, &err, &status);
    }

    let jobs = list_jobs_json(&cli);
    let count_jobs: Vec<&serde_json::Value> = jobs
        .iter()
        .filter(|j| {
            j.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n.starts_with("e2e-cron-count-"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        count_jobs.len(),
        3,
        "expected 3 e2e-cron-count-* jobs, got {}: {jobs:?}",
        count_jobs.len()
    );
    // All three names must be present (order is next_run-sorted, so don't assume order).
    let names: Vec<&str> = count_jobs
        .iter()
        .filter_map(|j| j.get("name").and_then(|n| n.as_str()))
        .collect();
    for expected in ["e2e-cron-count-a", "e2e-cron-count-b", "e2e-cron-count-c"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_remove_decrements_count() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-rm-");

    // Add a job and capture its id.
    let name = "e2e-cron-rm-target";
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "add", "--name", name, "--schedule", "0 0 * * * *", "--message", "x",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    let jobs_before = list_jobs_json(&cli);
    let before = jobs_before
        .iter()
        .filter(|j| j.get("name").and_then(|n| n.as_str()) == Some(name))
        .count();
    assert_eq!(before, 1, "job not added: {jobs_before:?}");

    let id = jobs_before
        .iter()
        .find(|j| j.get("name").and_then(|n| n.as_str()) == Some(name))
        .and_then(|j| j.get("id").and_then(|i| i.as_str()))
        .expect("job id present")
        .to_string();

    let (out, err, status) = run(
        &cli,
        &["cron", "remove", &id, "--force"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("Removed"),
        "remove output should say Removed, got: {out}"
    );

    let jobs_after = list_jobs_json(&cli);
    let after = jobs_after
        .iter()
        .filter(|j| j.get("name").and_then(|n| n.as_str()) == Some(name))
        .count();
    assert_eq!(after, 0, "job should be gone after remove: {jobs_after:?}");
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_history_empty_for_new_job() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-hist-");

    let name = "e2e-cron-hist-job";
    let (out, _, status) = run(
        &cli,
        &[
            "cron", "add", "--name", name, "--schedule", "0 0 * * * *", "--message", "x",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, "", &status);

    let jobs = list_jobs_json(&cli);
    let id = jobs
        .iter()
        .find(|j| j.get("name").and_then(|n| n.as_str()) == Some(name))
        .and_then(|j| j.get("id").and_then(|i| i.as_str()))
        .expect("job id present")
        .to_string();

    let (out, err, status) = run(
        &cli,
        &["cron", "history", &id, "--limit", "5"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("No history") || out.to_lowercase().contains("no history"),
        "history for new job should say No history, got: {out}"
    );
}

// ---------------------------------------------------------------------------
// Input validation
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_invalid_cron_expr_rejects() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-bad-");

    let (out, err, status) = run(
        &cli,
        &[
            "cron", "add", "--name", "e2e-cron-bad-cron", "--schedule", "not a cron expr",
            "--message", "x",
        ],
        Duration::from_secs(10),
    );
    assert_err(&out, &err, &status);
    // The CLI surfaces the parse error from the cron crate; either stdout
    // or stderr is acceptable as long as the exit code is non-zero.
    let combined = format!("{out}{err}");
    assert!(
        combined.to_lowercase().contains("invalid")
            || combined.to_lowercase().contains("error")
            || combined.to_lowercase().contains("parse"),
        "expected an invalid-cron error message, got stdout={out} stderr={err}"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_at_invalid_timestamp_rejects() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);

    let (out, err, status) = run(
        &cli,
        &[
            "cron", "at", "--name", "e2e-cron-bad-time", "--at", "not-an-rfc3339-timestamp",
            "--message", "x",
        ],
        Duration::from_secs(10),
    );
    assert_err(&out, &err, &status);
    let combined = format!("{out}{err}");
    assert!(
        combined.to_lowercase().contains("invalid")
            || combined.to_lowercase().contains("rfc3339")
            || combined.to_lowercase().contains("timestamp"),
        "expected an invalid-timestamp error, got stdout={out} stderr={err}"
    );
}

// ---------------------------------------------------------------------------
// Daemon execution (mirrors cron_execution.ps1 + cron_idle_event.ps1)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_run_triggers_due_job() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-fire-");

    // Schedule an `at` job 2 seconds in the future, then wait for the
    // daemon's poll cycle (1s) + execution time. History must show a run.
    let near_future = (chrono::Utc::now() + chrono::Duration::seconds(2)).to_rfc3339();
    let name = "e2e-cron-fire-at";
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "at", "--name", name, "--at", &near_future, "--message", "fire",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    // Wait for the daemon to pick it up and run it. Total budget: 8s
    // (1s poll + execution latency + 6s headroom).
    let mut ran = false;
    let jobs = list_jobs_json(&cli);
    let id = jobs
        .iter()
        .find(|j| j.get("name").and_then(|n| n.as_str()) == Some(name))
        .and_then(|j| j.get("id").and_then(|i| i.as_str()))
        .expect("job id present")
        .to_string();
    for _ in 0..8 {
        std::thread::sleep(Duration::from_secs(1));
        let (hout, herr, hstatus) = run(
            &cli,
            &["cron", "history", &id, "--limit", "5"],
            Duration::from_secs(5),
        );
        if hstatus.success()
            && (hout.contains("success")
                || hout.contains("failed")
                || hout.contains("running"))
        {
            ran = true;
            break;
        }
        let _ = herr;
    }
    assert!(
        ran,
        "daemon did not record a run for {name} (id={id}) within 8s"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_announce_writes_file_on_run() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-announce-");

    // Announcements land at `<data_dir>/announcements/<job_id>_<ts>.json`.
    // `PekoCli` sets `PEKO_HOME=peko_dir`, but `default_data_dir()` (in
    // `src/common/paths.rs:65`) appends `/data` to PEKO_HOME, so the
    // daemon's `data_dir` resolves to `<peko_dir>/data`, not `<peko_dir>`.
    // Announcements therefore live at `<peko_dir>/data/announcements/`.
    let announce_dir = cli.peko_dir().join("data").join("announcements");
    // Clean any leftovers from a prior failed run.
    if announce_dir.exists() {
        let _ = std::fs::remove_dir_all(&announce_dir);
    }

    // 2s in the future; with --announce the engine writes a JSON file on completion.
    let near_future = (chrono::Utc::now() + chrono::Duration::seconds(2)).to_rfc3339();
    let name = "e2e-cron-announce-target";
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "at", "--name", name, "--at", &near_future, "--message", "announce me",
            "--announce",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    // Wait for the daemon's poll + run + announce write. 8s budget.
    let mut wrote = false;
    for _ in 0..8 {
        std::thread::sleep(Duration::from_secs(1));
        if announce_dir.exists()
            && std::fs::read_dir(&announce_dir)
                .map(|it| it.filter_map(|e| e.ok()).count() > 0)
                .unwrap_or(false)
        {
            wrote = true;
            break;
        }
    }
    assert!(
        wrote,
        "no announcement file appeared in {} within 8s",
        announce_dir.display()
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_idle_does_not_panic() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-idle-");

    // Mirrors cron_idle_event.ps1 TEST 1: scheduling an idle job must
    // succeed even if the daemon's idle-detection wiring is partial.
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "add-idle", "--name", "e2e-cron-idle-smoke", "--minutes", "1",
            "--message", "wakeup",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    // Daemon should still be alive and responsive after the add.
    let (_, _, status) = run(&cli, &["daemon", "status", "--json"], Duration::from_secs(5));
    assert!(
        status.success(),
        "daemon should still respond after add-idle"
    );
}

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_add_event_does_not_panic() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);
    remove_jobs_with_prefix(&cli, "e2e-cron-evt-");

    // Mirrors cron_idle_event.ps1 TEST 2: scheduling an event job must
    // succeed even if no event source is wired.
    let (out, err, status) = run(
        &cli,
        &[
            "cron", "add-event", "--name", "e2e-cron-evt-smoke", "--event-type", "internal",
            "--once", "--message", "react",
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    let (_, _, status) = run(&cli, &["daemon", "status", "--json"], Duration::from_secs(5));
    assert!(
        status.success(),
        "daemon should still respond after add-event"
    );
}

// ---------------------------------------------------------------------------
// Integration sanity: every test cleans up its own prefix.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon (Unix only)"]
fn cron_remove_idempotent_on_missing_job() {
    if skip_if_no_mock().is_none() {
        return;
    }
    let cli = PekoCli::new();
    let _daemon = CronDaemonGuard::spawn(&cli);

    // Removing a job that doesn't exist must NOT silently succeed; the
    // daemon should return an error and the CLI should surface a
    // non-zero exit. (The original `cron_basics.ps1` only checked the
    // happy path; this pins the failure path.)
    let (out, err, status) = run(
        &cli,
        &["cron", "remove", "cron_does_not_exist", "--force"],
        Duration::from_secs(10),
    );
    assert_err(&out, &err, &status);
    let combined = format!("{out}{err}");
    assert!(
        combined.to_lowercase().contains("not found")
            || combined.to_lowercase().contains("no such")
            || combined.to_lowercase().contains("does not exist"),
        "removing a missing job should report not-found, got stdout={out} stderr={err}"
    );
}
