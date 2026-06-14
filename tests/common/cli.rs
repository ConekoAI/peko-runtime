//! `peko` CLI test driver — isolated per-test `HOME`, lazy access to the built binary.
//!
//! Each test creates one [`PekoCli`], gets a tempdir under it, and spawns `peko`
//! subcommands via [`PekoCli::cmd`]. The subprocess sees `HOME` / `USERPROFILE`
//! pointing at the tempdir, so both the CLI client and any daemon it spawns
//! agree on `<tempdir>/.peko/...` for config, data, and the IPC endpoint.
//!
//! The IPC endpoint is per-platform (ADR-021, ADR-038):
//!   - **Unix**: `<peko_dir>/run/daemon.sock` (Unix domain datagram socket).
//!   - **Windows**: a unique per-test named pipe (e.g.
//!     `\\.\pipe\peko-test-{pid}-{uuid}`) — see [`PekoCli::daemon_endpoint`].
//!
//! On Windows we override `PEKO_DAEMON_PIPE` to a per-test unique name so
//! concurrent tests don't collide on the global pipe namespace.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Per-test handle: owns a tempdir used as the isolated `HOME` for all
/// spawned `peko` subprocesses, and knows where the built binary lives.
pub struct PekoCli {
    home: TempDir,
    /// Windows-only: per-test unique named pipe. `None` on Unix.
    #[cfg(windows)]
    pipe_name: String,
}

impl PekoCli {
    /// Create a fresh isolated environment. Drop the returned value to clean up.
    pub fn new() -> Self {
        let home = TempDir::new().expect("create tempdir for PekoCli HOME");
        // Pre-create the standard PEKO directory structure so the daemon
        // can write to its cron DB, telemetry, cache, etc. without hitting
        // "No such file or directory" on first access. Matches what
        // `peko agent create` would set up in production.
        for sub in [".peko", ".peko/run", ".peko/data", ".peko/cache"] {
            std::fs::create_dir_all(home.path().join(sub))
                .unwrap_or_else(|e| panic!("create {sub}: {e}"));
        }
        // Per-test unique named pipe on Windows. The PID + a short random
        // suffix guarantees uniqueness across concurrent test processes.
        #[cfg(windows)]
        let pipe_name = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            // Win32 pipe-name max is 256 chars; `\\.\pipe\peko-test-` is 18,
            // pid is ≤10, separator is 1, nanos is ≤20, so well under budget.
            format!(r"\\.\pipe\peko-test-{}-{}", std::process::id(), nanos)
        };
        Self {
            home,
            #[cfg(windows)]
            pipe_name,
        }
    }

    /// Absolute path to the isolated `HOME`.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// `<HOME>/.peko` — what `default_config_dir()` and the daemon both resolve to.
    pub fn peko_dir(&self) -> PathBuf {
        self.home.path().join(".peko")
    }

    /// Unix-only: absolute path the daemon will bind to and the client will
    /// connect to. Returns `<peko_dir>/run/daemon.sock`.
    #[cfg(unix)]
    pub fn daemon_sock(&self) -> PathBuf {
        self.peko_dir().join("run").join("daemon.sock")
    }

    /// Windows-only: the per-test unique named-pipe name. Set as
    /// `PEKO_DAEMON_PIPE` on every spawned command so the daemon binds
    /// here instead of the global `\\.\pipe\peko-{user}` default.
    #[cfg(windows)]
    pub fn daemon_pipe(&self) -> &str {
        &self.pipe_name
    }

    /// Cross-platform endpoint description for panic messages and
    /// diagnostics. Prefer this over the platform-specific accessors in
    /// new test code.
    pub fn daemon_endpoint(&self) -> String {
        #[cfg(unix)]
        {
            self.daemon_sock().display().to_string()
        }
        #[cfg(windows)]
        {
            self.pipe_name.clone()
        }
    }

    /// Build a `Command` that runs the `peko` binary with the isolated env.
    ///
    /// Sets `HOME`, `USERPROFILE` (Windows), `PEKO_HOME`, the
    /// platform-specific IPC override (`PEKO_DAEMON_SOCK` on Unix,
    /// `PEKO_DAEMON_PIPE` on Windows), and unsets `MINIMAX_API_KEY` so a
    /// leaking env can't switch the test to the real provider mid-run.
    pub fn cmd(&self) -> Command {
        let bin = env!("CARGO_BIN_EXE_peko");
        let mut c = Command::new(bin);
        c.env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("PEKO_HOME", self.peko_dir())
            .env_remove("MINIMAX_API_KEY");

        // Platform-specific IPC endpoint override.
        #[cfg(unix)]
        {
            // We don't currently set PEKO_DAEMON_SOCK explicitly — the
            // daemon's default discovery uses default_socket_path() which
            // resolves to <HOME>/.peko/run/daemon.sock, matching the
            // tempdir-based HOME. The test runs as the same user that
            // created the tempdir, so the socket's 0600 mode is fine.
        }
        #[cfg(windows)]
        {
            c.env("PEKO_DAEMON_PIPE", &self.pipe_name);
        }

        c
    }
}
