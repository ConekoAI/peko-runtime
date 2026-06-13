//! Daemon lifecycle for CLI integration tests.
//!
//! Spawns `peko daemon start --foreground` against an isolated [`PekoCli`],
//! polls until it's accepting IPC, and kills it on `Drop`.
//!
//! Foreground mode is critical: without it, `peko daemon start` daemonizes
//! and we lose the child handle, leaving an orphan daemon that ignores
//! `Drop` and pollutes the next test.

#![allow(dead_code)]

use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use super::cli::PekoCli;

/// Owns a running `peko daemon` child. Killing on `Drop` is best-effort.
pub struct DaemonGuard {
    child: Child,
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
        let child = cli
            .cmd()
            .args(["daemon", "start", "--foreground"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn peko daemon start --foreground");

        let mut guard = Self { child };
        guard.wait_ready(cli, Duration::from_secs(30));
        guard
    }

    /// Poll `peko daemon status --json` until `running: true` or `timeout` elapses.
    /// Each poll itself is wrapped in a 5s hard timeout so a stuck peko
    /// subprocess can't hang the whole wait_ready loop.
    ///
    /// Why 5s per poll (not 2s): when the daemon's Unix socket isn't bound
    /// yet, the CLI's `ConnectionManager::try_connect()` falls through to
    /// a UDP fallback that itself uses a hard 2s `recv` timeout (see
    /// `src/ipc/connection.rs`). A 2s poll budget races that and kills the
    /// child *before* it can print the "not running" JSON, so wait_ready
    /// would never see a useful result. 5s gives the CLI room to time out
    /// the UDP fallback and print its JSON cleanly.
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
                Duration::from_secs(5),
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
                    "peko daemon did not become ready in {:?} (sock: {})\n\
                     --- last status JSON ---\n{last_status_json}\n\
                     --- last poll result ---\n{output:?}\n\
                     --- end ---",
                    timeout,
                    cli.daemon_sock().display(),
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
