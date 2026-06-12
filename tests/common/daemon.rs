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
    /// Spawn the daemon and wait until `peko daemon status` succeeds (max 10s).
    pub fn spawn(cli: &PekoCli) -> Self {
        let child = cli
            .cmd()
            .args(["daemon", "start", "--foreground"])
            // Daemon's logs would otherwise drown the test output. Capture
            // them so they're available via `Drop` for failure debugging.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn peko daemon start --foreground");

        let guard = Self { child };
        guard.wait_ready(cli, Duration::from_secs(10));
        guard
    }

    /// Poll `peko daemon status` until it exits 0 or `timeout` elapses.
    /// Panics if the daemon never becomes ready — surfacing a timeout here
    /// is what catches "daemon crashed on startup" in CI.
    fn wait_ready(&self, cli: &PekoCli, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let status = cli
                .cmd()
                .args(["daemon", "status"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if matches!(status, Ok(s) if s.success()) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "peko daemon did not become ready in {:?} (sock: {})",
                    timeout,
                    cli.daemon_sock().display()
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
