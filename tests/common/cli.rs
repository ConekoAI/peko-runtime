//! `peko` CLI test driver — isolated per-test `HOME`, lazy access to the built binary.
//!
//! Each test creates one [`PekoCli`], gets a tempdir under it, and spawns `peko`
//! subcommands via [`PekoCli::cmd`]. The subprocess sees `HOME` / `USERPROFILE`
//! pointing at the tempdir, so both the CLI client and any daemon it spawns
//! agree on `<tempdir>/.peko/...` for config, data, and the IPC socket.
//!
//! Tests that use this module are cfg-gated to `unix` because the daemon's
//! IPC server is Unix-only (see src/ipc/server.rs).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Per-test handle: owns a tempdir used as the isolated `HOME` for all
/// spawned `peko` subprocesses, and knows where the built binary lives.
pub struct PekoCli {
    home: TempDir,
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
        Self { home }
    }

    /// Absolute path to the isolated `HOME`.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// `<HOME>/.peko` — what `default_config_dir()` and the daemon both resolve to.
    pub fn peko_dir(&self) -> PathBuf {
        self.home.path().join(".peko")
    }

    /// Absolute path the daemon will bind to and the client will connect to.
    pub fn daemon_sock(&self) -> PathBuf {
        self.peko_dir().join("run").join("daemon.sock")
    }

    /// Build a `Command` that runs the `peko` binary with the isolated env.
    ///
    /// Sets `HOME`, `USERPROFILE` (Windows), `PEKO_HOME`, and unsets
    /// `MINIMAX_API_KEY` so a leaking env can't switch the test to the real
    /// provider mid-run.
    pub fn cmd(&self) -> Command {
        let bin = env!("CARGO_BIN_EXE_peko");
        let mut c = Command::new(bin);
        c.env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("PEKO_HOME", self.peko_dir())
            .env_remove("MINIMAX_API_KEY");
        c
    }
}
