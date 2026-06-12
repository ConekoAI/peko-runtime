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

        let mut guard = Self { child };
        guard.wait_ready(cli, Duration::from_secs(10));
        guard
    }

    /// Poll `peko daemon status --json` until `running: true` or `timeout` elapses.
    /// Panics if the daemon never becomes ready — surfacing a timeout here
    /// is what catches "daemon crashed on startup" in CI.
    ///
    /// Why --json: `peko daemon status` exits 0 in BOTH the running and
    /// not-running branches (so checking exit code is meaningless). Parsing
    /// the JSON's `running: true` field is the only reliable signal.
    fn wait_ready(&mut self, cli: &PekoCli, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let output = cli
                .cmd()
                .args(["daemon", "status", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output();
            let running = match &output {
                Ok(out) if out.status.success() => serde_json::from_slice::<serde_json::Value>(&out.stdout)
                    .ok()
                    .and_then(|v| v.get("running").and_then(|r| r.as_bool()))
                    .unwrap_or(false),
                _ => false,
            };
            if running {
                return;
            }
            if Instant::now() >= deadline {
                // Drain captured child pipes so the panic message can
                // surface what the daemon process said (or didn't say)
                // before timing out. Common causes: data_dir not
                // pre-created, IPC bind failure, missing env.
                let mut stdout = String::new();
                if let Some(p) = self.child.stdout.as_mut() {
                    let _ = p.read_to_string(&mut stdout);
                }
                let mut stderr = String::new();
                if let Some(p) = self.child.stderr.as_mut() {
                    let _ = p.read_to_string(&mut stderr);
                }
                let last_status_json = match &output {
                    Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
                    Err(_) => String::new(),
                };
                panic!(
                    "peko daemon did not become ready in {:?} (sock: {})\n\
                     --- daemon stdout ---\n{stdout}\n\
                     --- daemon stderr ---\n{stderr}\n\
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
