//! Daemon lifecycle for CLI integration tests.
//!
//! Spawns `peko daemon start --foreground` against an isolated [`PekoCli`],
//! polls until it's accepting IPC, and kills it on `Drop`.
//!
//! Foreground mode is critical: without it, `peko daemon start` daemonizes
//! and we lose the child handle, leaving an orphan daemon that ignores
//! `Drop` and pollutes the next test.
//!
//! Note that even in foreground mode the child is *not* the daemon: since
//! PR #309 the foreground command spawns the `peko-daemon` binary and waits
//! on it. `Drop` therefore signals the real daemon PID from the run dir's
//! `daemon.pid` as well as the wrapper, because `Child::kill()` alone
//! (SIGKILL) cannot be forwarded and would orphan the daemon.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use super::cli::PekoCli;

/// Owns a running `peko daemon` child. Killing on `Drop` is best-effort.
pub struct DaemonGuard {
    child: Child,
    /// `<peko_dir>/run/daemon.pid`, which the foreground wrapper
    /// populates with the real `peko-daemon` PID.
    pid_file: PathBuf,
}

impl DaemonGuard {
    /// Spawn the daemon and wait until `peko daemon status --json` reports
    /// `running: true` (max 30s).
    ///
    /// **Both stdout AND stderr go to `Stdio::null()`.** Capturing either
    /// in a `Stdio::piped()` is a deadlock risk: if the daemon writes
    /// more than the kernel pipe buffer (~64KB) and nobody reads, the
    /// daemon blocks on its next write — and from the test's
    /// perspective the daemon "isn't ready" forever, with no stderr to
    /// diagnose. Disabling both captures drops that risk; we lose
    /// some diagnostics but the workflow's `Dump container logs` step
    /// captures the relevant pekohub-test / mock-llm output anyway
    /// (those are the services doing the real work).
    pub fn spawn(cli: &PekoCli) -> Self {
        // v3 mock-LLM bootstrap: if `MOCK_LLM_URL` is set, seed the
        // catalog with a `mock-llm` entry pointing at the URL before
        // the daemon starts. `PekoCli::cmd` exports the matching
        // `PEKO_TEST_RESOLVER_BOOTSTRAP=1` + `MOCK_LLM_API_KEY` so
        // the daemon can find the API key without a real keychain.
        if let Some(mock_url) = std::env::var_os("MOCK_LLM_URL") {
            super::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url.to_string_lossy());
        }

        let debug_out =
            std::fs::File::create("/tmp/peko-daemon-debug.out").expect("create daemon debug out");
        let debug_err =
            std::fs::File::create("/tmp/peko-daemon-debug.err").expect("create daemon debug err");
        let child = cli
            .cmd()
            .args(["daemon", "start", "--foreground", "-vv"])
            .stdout(Stdio::from(debug_out))
            .stderr(Stdio::from(debug_err))
            .spawn()
            .expect("spawn peko daemon start --foreground");

        let pid_file = cli.peko_dir().join("run").join("daemon.pid");
        let mut guard = Self { child, pid_file };
        guard.wait_ready(cli, Duration::from_secs(30));
        guard
    }

    /// Poll `peko daemon status --json` until `running: true` or `timeout` elapses.
    /// Each poll itself is wrapped in a 6s hard timeout so a stuck peko
    /// subprocess can't hang the whole wait_ready loop.
    ///
    /// Why 6s per poll (not 2s, not 5s): the CLI's `ConnectionManager::try_connect`
    /// ([src/ipc/connection.rs](src/ipc/connection.rs)) tries Unix-default first
    /// (2s recv timeout), then UDP-default (another 2s recv timeout), before
    /// giving up and printing the "not running" JSON. So a single status call
    /// can take ~4s when the daemon is still binding its socket. A 6s budget
    /// fits that worst case with a little headroom for the JSON to flush.
    ///
    /// Why `try_run_with_timeout` (not `run_with_timeout`): the latter
    /// panics on timeout, which would unwind through this entire loop
    /// after one stuck poll. We want the loop to retry until the outer
    /// deadline, so we use the soft variant that returns `Err`.
    fn wait_ready(&mut self, cli: &PekoCli, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut last_status_json = String::new();
        loop {
            let output = super::subprocess::try_run_with_timeout(
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
                Ok(_) | Err(_) => last_status_json,
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
            if Instant::now() >= deadline {
                panic!(
                    "peko daemon did not become ready in {:?} (endpoint: {})\n\
                     --- last status JSON ---\n{last_status_json}\n\
                     --- last poll result ---\n{output:?}\n\
                     --- end ---",
                    timeout,
                    cli.daemon_endpoint(),
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Ask a PID to shut down gracefully. Best effort — the PID may already
/// be gone, and `Drop` falls back to `Child::kill()` regardless.
fn terminate_pid(pid: u32) {
    #[cfg(unix)]
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string()])
        .status();
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // Stop the real daemon before the wrapper. `Child::kill()` sends
        // SIGKILL, which the wrapper has no chance to forward, so relying
        // on it alone leaves `peko-daemon` running — reparented to init,
        // unreachable over IPC, and never shut down.
        if let Ok(raw) = std::fs::read_to_string(&self.pid_file) {
            if let Ok(pid) = raw.trim().parse::<u32>() {
                terminate_pid(pid);
            }
        }
        terminate_pid(self.child.id());

        // Brief grace period so the graceful shutdown can take effect
        // before the hard kill.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
