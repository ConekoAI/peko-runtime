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
    /// Passphrase for the encrypted vault created for this test.
    vault_passphrase: String,
    /// Windows-only: per-test unique named pipe. `None` on Unix.
    #[cfg(windows)]
    pipe_name: String,
    /// If true, do not strip `MINIMAX_API_KEY` / `KIMI_API_KEY` from the
    /// daemon's environment and enable `PEKO_TEST_RESOLVER_BOOTSTRAP=1`
    /// so real-LLM tests can resolve API keys without an OS keychain.
    /// Default false keeps mock-tier tests safe from leaking env vars.
    allow_real_llm_keys: bool,
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

        // Create an encrypted vault with a test passphrase so the daemon
        // can load provider keys / identity keys / tunnel keys without an OS
        // keychain in CI/headless environments. Each test has its own tempdir,
        // so a shared passphrase is safe.
        let vault_passphrase = "peko-test-vault-passphrase".to_string();
        let vault_path = home.path().join(".peko").join("vault.enc");
        peko::common::vault::Vault::with_passphrase(
            &vault_path,
            &secrecy::SecretString::new(vault_passphrase.clone().into()),
        )
        .expect("create test vault");

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
            vault_passphrase,
            #[cfg(windows)]
            pipe_name,
            allow_real_llm_keys: false,
        }
    }

    /// Allow real-LLM API keys to flow through to spawned subprocesses.
    ///
    /// Call this for tests that intentionally exercise minimax/kimi. It
    /// enables the daemon's env-var keychain bootstrap and prevents
    /// `PekoCli::cmd` from stripping `MINIMAX_API_KEY` / `KIMI_API_KEY`.
    #[must_use]
    pub fn allow_real_llm_keys(mut self) -> Self {
        self.allow_real_llm_keys = true;
        self
    }

    /// Absolute path to the isolated `HOME`.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// Passphrase used for the encrypted vault created for this test.
    ///
    /// Subcommands spawned via [`PekoCli::cmd`] already receive this as
    /// `PEKO_MASTER_PASSPHRASE`. Tests that mutate the vault directly can use
    /// this value to stay in sync with the CLI environment.
    pub fn vault_passphrase(&self) -> &str {
        &self.vault_passphrase
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

    /// Install the platform-specific IPC endpoint env var into the current
    /// process so in-test `DaemonClient::connect()` calls reach the isolated
    /// daemon instead of the user's default socket/pipe.
    pub fn install_ipc_endpoint_env(&self) {
        #[cfg(unix)]
        {
            std::env::set_var("PEKO_DAEMON_SOCK", self.daemon_sock());
        }
        #[cfg(windows)]
        {
            std::env::set_var("PEKO_DAEMON_PIPE", &self.pipe_name);
        }
    }

    /// Build a `Command` that runs the `peko` binary with the isolated env.
    ///
    /// Sets `HOME`, `USERPROFILE` (Windows), `PEKO_HOME`, the
    /// platform-specific IPC override (`PEKO_DAEMON_SOCK` on Unix,
    /// `PEKO_DAEMON_PIPE` on Windows), and changes the subprocess current
    /// working directory to the isolated `HOME`. The CWD isolation prevents
    /// commands like `config init` (which writes `peko.toml` relative to
    /// CWD) from polluting the project root and causing flaky environmental
    /// failures.
    ///
    /// By default `MINIMAX_API_KEY` / `KIMI_API_KEY` are stripped so a
    /// leaking env can't switch a mock-tier test to a paid provider.
    /// Call [`Self::allow_real_llm_keys`] for tests that intentionally
    /// exercise real providers.
    pub fn cmd(&self) -> Command {
        let bin = env!("CARGO_BIN_EXE_peko");
        let mut c = Command::new(bin);
        c.env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("PEKO_HOME", self.peko_dir())
            .env("PEKO_MASTER_PASSPHRASE", &self.vault_passphrase)
            .env("PEKO_IDENTITY_PASSPHRASE", &self.vault_passphrase)
            .current_dir(self.home.path());

        // v3 provider key bootstrap: in CI / headless test runners the
        // OS keychain isn't available, so the daemon can fall back to
        // conventional `*_API_KEY` env vars when
        // `PEKO_TEST_RESOLVER_BOOTSTRAP=1` is set.
        //
        // Mock-LLM tests: `MOCK_LLM_URL` is set; we seed the catalog with
        // a `mock-llm` entry and export the matching `MOCK_LLM_API_KEY`.
        //
        // Real-LLM tests: `allow_real_llm_keys` is true; we keep
        // `MINIMAX_API_KEY` / `KIMI_API_KEY` in the env and flip the
        // bootstrap flag so the daemon can read them.
        if std::env::var_os("MOCK_LLM_URL").is_some() {
            c.env("PEKO_TEST_RESOLVER_BOOTSTRAP", "1");
            c.env("MOCK_LLM_API_KEY", "mock-llm-test-key");
        } else if self.allow_real_llm_keys {
            c.env("PEKO_TEST_RESOLVER_BOOTSTRAP", "1");
        }

        if !self.allow_real_llm_keys {
            // Strip real-LLM keys so a leaking env can't switch a mock-tier
            // test to a paid provider mid-run.
            c.env_remove("MINIMAX_API_KEY");
            c.env_remove("KIMI_API_KEY");
        }

        // Platform-specific IPC endpoint override.
        #[cfg(unix)]
        {
            c.env("PEKO_DAEMON_SOCK", self.daemon_sock());
        }
        #[cfg(windows)]
        {
            c.env("PEKO_DAEMON_PIPE", &self.pipe_name);
        }

        c
    }
}
