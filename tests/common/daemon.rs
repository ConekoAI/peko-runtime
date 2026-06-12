//! Daemon lifecycle for CLI integration tests.
//!
//! Spawns `peko daemon start --foreground` against an isolated [`PekoCli`],
//! polls until it's accepting IPC, and kills it on `Drop`.
//!
//! Foreground mode is critical: without it, `peko daemon start` daemonizes
//! and we lose the child handle, leaving an orphan daemon that ignores
//! `Drop` and pollutes the next test.

#![allow(dead_code)]

use std::io::Read;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use super::cli::PekoCli;

/// Owns a running `peko daemon` child. Killing on `Drop` is best-effort.
pub struct DaemonGuard {
    child: Child,
}

impl DaemonGuard {
    /// Spawn the daemon and wait until `peko daemon status --json` reports
    /// `running: true` (max 10s).
    ///
    /// Stdout is captured (for diagnostic dumps on timeout), but stderr
    /// goes to `Stdio::null()`. Capturing stderr is a deadlock risk: if
    /// the daemon writes more than the kernel pipe buffer (~64KB) and
    /// nobody reads, the daemon blocks on write. Disabling the capture
    /// drops that risk; we lose some diagnostics but the workflow's
    /// `Dump container logs` step captures the relevant pekohub-test
    /// / mock-llm output anyway.
    pub fn spawn(cli: &PekoCli) -> Self {
        let child = cli
            .cmd()
            .args(["daemon", "start", "--foreground"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn peko daemon start --foreground");

        let mut guard = Self { child };
        guard.wait_ready(cli, Duration::from_secs(10));
        guard
    }

    /// Poll `peko daemon status --json` until `running: true` or `timeout` elapses.
    /// Each poll itself is wrapped in a 2s hard timeout so a stuck peko
    /// subprocess can't hang the whole wait_ready loop.
    fn wait_ready(&mut self, cli: &PekoCli, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut last_status_json = String::new();
        loop {
            // 2s budget per individual status call (a stuck peko would
            // otherwise hang the loop until the outer 10s timeout).
            // The closure returns an owned Command: each method on
            // Command returns &mut Command, so the closure must use
            // a let-binding to materialise an owned value.
            let output = super::subprocess::run_with_timeout(
                || {
                    let mut c = cli.cmd();
                    c.args(["daemon", "status", "--json"])
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null());
                    c
                },
                &[],
                Duration::from_secs(2),
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
                // Drain the daemon's stdout pipe so we can surface what
                // it was saying. Stderr is null so nothing to drain.
                let mut daemon_stdout = String::new();
                if let Some(p) = self.child.stdout.as_mut() {
                    let _ = std::io::Read::read_to_string(p, &mut daemon_stdout);
                }
                panic!(
                    "peko daemon did not become ready in {:?} (sock: {})\n\
                     --- daemon stdout ---\n{daemon_stdout}\n\
                     --- last status JSON ---\n{last_status_json}\n\
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
