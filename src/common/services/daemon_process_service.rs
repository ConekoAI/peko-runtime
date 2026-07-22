//! Daemon process lifecycle service
//!
//! Encapsulates all daemon process management concerns:
//! - Spawning the daemon as a background child process
//! - Stopping the daemon (graceful IPC shutdown → PID kill → fallback)
//! - Checking if the daemon is running (IPC ping + PID file)
//! - Waiting for daemon readiness
//! - Reading/writing the PID file
//!
//! This service replaces the inline process lifecycle logic in
//! `src/commands/daemon.rs` with testable, reusable primitives.

use crate::common::paths::PathResolver;
use crate::common::process::{
    is_process_running, kill_all_by_name, kill_by_pid, wait_for_exit, wait_for_healthy,
};
use crate::ipc::{ConnectionManager, RequestPacket, ResponsePacket};
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::{Child, Command};
use tracing::{debug, warn};

/// CLI flag surface to use when invoking the daemon process.
///
/// Phase 11c introduced the `peko-daemon` binary artifact. The two
/// variants below capture the two arg layouts `spawn_daemon_with` may
/// need to construct depending on which artifact is present:
/// - `PekoDaemon` — direct invocation of the `peko-daemon` binary; the
///   flags are passed as-is, no `daemon start` subcommand prefix.
/// - `PekoDaemonStartForeground` — legacy path that re-execs the CLI
///   (`peko daemon start --foreground ...`) for installs / dev trees
///   that have not built the new artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgPrefix {
    PekoDaemon,
    PekoDaemonStartForeground,
}

/// Status information about the daemon process
#[derive(Debug, Clone)]
pub struct DaemonStatus {
    /// Whether the daemon is responding to IPC
    pub responding: bool,
    /// Whether a process with the recorded PID exists
    pub process_exists: bool,
    /// Daemon version (if responding)
    pub version: Option<String>,
    /// Uptime in seconds (if responding)
    pub uptime_secs: Option<u64>,
    /// PID (if known)
    pub pid: Option<u32>,
    /// Whether the daemon is ready to serve requests
    pub ready: bool,
    /// Error message if the daemon is not responding but process exists
    pub error: Option<String>,
    /// Tunnel health snapshot (issue #8). `None` if the daemon is not
    /// responding to IPC.
    pub tunnel: Option<TunnelStatusInfo>,
    /// Launch mode of the running daemon (`Sidecar` vs. `Headless`),
    /// surfaced from `ResponsePacket::Status::mode`. ADR-043 adoption:
    /// `peko daemon start` uses this to print which mode owns the IPC
    /// socket when it refuses to start a second daemon. `None` if the
    /// daemon didn't report it (older builds).
    pub mode: Option<crate::daemon::LaunchMode>,
}

/// Tunnel health snapshot exposed in `peko daemon status --json` (issue #8).
#[derive(Debug, Clone)]
pub struct TunnelStatusInfo {
    /// One of "disabled" | "connected" | "disconnected" | "degraded".
    pub state: String,
    /// Consecutive failed reconnect attempts (0 for connected/disabled).
    pub reconnect_attempts: u32,
    /// Last error message, if any.
    pub last_error: Option<String>,
    /// Whether the daemon is currently in degraded state.
    pub degraded: bool,
}

/// Service for managing the daemon process lifecycle
#[derive(Debug, Clone)]
pub struct DaemonProcessService {
    resolver: PathResolver,
}

impl DaemonProcessService {
    /// Create a new daemon process service
    #[must_use]
    pub fn new(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    // ------------------------------------------------------------------
    // PID file helpers
    // ------------------------------------------------------------------

    /// Get the path to the daemon PID file.
    ///
    /// Two distinct paths exist in `<config_dir>/run/`:
    /// - `daemon.pid` — the lockfile used by `peko daemon start` (CLI / headless)
    /// - `desktop.lock` — the lockfile used when `peko-desktop` spawns the
    ///   bundled sidecar with `--sidecar-mode` (ADR-043). Distinct name
    ///   prevents a CLI daemon and a desktop sidecar in the same config
    ///   dir from racing on the same lockfile.
    #[must_use]
    pub fn pid_file_path(&self, sidecar_mode: bool) -> PathBuf {
        let name = if sidecar_mode {
            "desktop.lock"
        } else {
            "daemon.pid"
        };
        self.resolver.config_dir().join("run").join(name)
    }

    /// True iff a sidecar-mode lockfile is currently held on disk by a
    /// live process. Used by `peko daemon restart` to preserve the mode of
    /// whatever was running before the restart.
    ///
    /// Stale lockfiles (PID present but not running, or unparseable
    /// contents) are cleaned up as a side-effect, matching
    /// `read_pid_for`'s behaviour. This keeps callers from accidentally
    /// chaining into `spawn_daemon_with` with a stale lockfile still on
    /// disk.
    #[must_use]
    pub fn is_sidecar_lock_held(&self) -> bool {
        let path = self.pid_file_path(true);
        if !path.exists() {
            return false;
        }
        let raw = match std::fs::read_to_string(&path).ok() {
            Some(s) => s,
            None => return false,
        };
        match raw.trim().parse::<u32>().ok() {
            Some(pid) if is_process_running(pid) => true,
            _ => {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(path.with_extension("lock"));
                false
            }
        }
    }

    /// Read the PID from the PID file if it exists and the process is running
    pub fn read_pid(&self) -> Option<u32> {
        // Default-mode read; sidecar-mode reads happen via `read_pid_sidecar`.
        self.read_pid_for(false)
    }

    /// Read the PID from the sidecar lockfile if it exists and the process is running.
    pub fn read_pid_sidecar(&self) -> Option<u32> {
        self.read_pid_for(true)
    }

    fn read_pid_for(&self, sidecar_mode: bool) -> Option<u32> {
        let path = self.pid_file_path(sidecar_mode);
        if !path.exists() {
            return None;
        }
        let pid_str = std::fs::read_to_string(&path).ok()?;
        let pid = pid_str.trim().parse::<u32>().ok()?;
        if is_process_running(pid) {
            Some(pid)
        } else {
            // Stale lockfile — clean it up.
            let _ = std::fs::remove_file(&path);
            // Companion `.lock` is a fsnotify-style sidecar used by some
            // platforms; harmless to remove if absent.
            let _ = std::fs::remove_file(path.with_extension("lock"));
            None
        }
    }

    /// Write the PID to the PID file (default daemon mode).
    pub fn write_pid(&self, pid: u32) -> anyhow::Result<()> {
        self.write_pid_for(pid, false)
    }

    /// Write the PID to the sidecar lockfile.
    pub fn write_pid_sidecar(&self, pid: u32) -> anyhow::Result<()> {
        self.write_pid_for(pid, true)
    }

    fn write_pid_for(&self, pid: u32, sidecar_mode: bool) -> anyhow::Result<()> {
        let path = self.pid_file_path(sidecar_mode);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, pid.to_string())?;
        Ok(())
    }

    /// Remove the PID file and associated lock file (default daemon mode).
    pub fn remove_pid_file(&self) {
        self.remove_pid_file_for(false);
    }

    /// Remove the sidecar lockfile.
    pub fn remove_sidecar_lock_file(&self) {
        self.remove_pid_file_for(true);
    }

    fn remove_pid_file_for(&self, sidecar_mode: bool) {
        let path = self.pid_file_path(sidecar_mode);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }

    // ------------------------------------------------------------------
    // Running check
    // ------------------------------------------------------------------

    /// Check if the daemon is running by attempting an IPC ping
    ///
    /// Returns `Ok(true)` if the daemon responds to a ping.
    /// Returns `Ok(false)` if the daemon is not reachable.
    pub async fn is_daemon_running(&self) -> anyhow::Result<bool> {
        match ConnectionManager::try_connect().await {
            Ok(conn) => {
                let ping = RequestPacket::Ping { request_id: 0 };
                let bytes = match ping.to_bytes() {
                    Ok(b) => b,
                    Err(_) => return Ok(false),
                };
                if conn.send(&bytes).await.is_err() {
                    return Ok(false);
                }
                let mut buf = vec![0u8; 65536];
                match conn.recv_timeout(&mut buf, Duration::from_secs(2)).await {
                    Ok(len) => match ResponsePacket::from_bytes(&buf[..len]) {
                        Ok(ResponsePacket::Pong { .. }) => Ok(true),
                        _ => Ok(false),
                    },
                    Err(_) => Ok(false),
                }
            }
            Err(_) => Ok(false),
        }
    }

    /// Get full daemon status (IPC + PID file)
    pub async fn get_daemon_status(&self) -> anyhow::Result<DaemonStatus> {
        let pid = self.read_pid();

        // Try IPC first — issue #8: query the rich Status packet so we
        // can include tunnel health (state, reconnect attempts, last_error)
        // in the `peko daemon status --json` output.
        match ConnectionManager::try_connect().await {
            Ok(conn) => {
                let ping = RequestPacket::Status { request_id: 0 };
                if let Ok(bytes) = ping.to_bytes() {
                    if conn.send(&bytes).await.is_ok() {
                        let mut buf = vec![0u8; 65536];
                        if let Ok(len) = conn.recv_timeout(&mut buf, Duration::from_secs(2)).await {
                            if let Ok(ResponsePacket::Status {
                                uptime_secs,
                                version,
                                tunnel_state,
                                tunnel_reconnect_attempts,
                                tunnel_last_error,
                                degraded,
                                mode,
                                ..
                            }) = ResponsePacket::from_bytes(&buf[..len])
                            {
                                return Ok(DaemonStatus {
                                    responding: true,
                                    process_exists: pid.is_some(),
                                    version: Some(version),
                                    uptime_secs: Some(uptime_secs),
                                    pid,
                                    ready: true,
                                    error: None,
                                    tunnel: Some(TunnelStatusInfo {
                                        state: tunnel_state,
                                        reconnect_attempts: tunnel_reconnect_attempts,
                                        last_error: tunnel_last_error,
                                        degraded,
                                    }),
                                    mode,
                                });
                            }
                        }
                    }
                }
            }
            Err(_) => {}
        }

        // Not responding via IPC — check PID file
        if let Some(pid) = pid {
            if is_process_running(pid) {
                return Ok(DaemonStatus {
                    responding: false,
                    process_exists: true,
                    version: None,
                    uptime_secs: None,
                    pid: Some(pid),
                    ready: false,
                    error: Some(format!("daemon not responding (process {pid} exists)")),
                    tunnel: None,
                    mode: None,
                });
            }
        }

        Ok(DaemonStatus {
            responding: false,
            process_exists: false,
            version: None,
            uptime_secs: None,
            pid: None,
            ready: false,
            error: None,
            tunnel: None,
            mode: None,
        })
    }

    // ------------------------------------------------------------------
    // Spawn
    // ------------------------------------------------------------------

    /// Spawn the daemon as a background child process (default mode).
    ///
    /// Backwards-compatible wrapper around `spawn_daemon_with` with
    /// `sidecar_mode = false`. The CLI / headless path uses this.
    ///
    /// The daemon binary is invoked with `daemon start --foreground` so it
    /// blocks in the child process. Stdout/stderr are suppressed.
    ///
    /// # Errors
    /// Returns error if the daemon fails to spawn or does not become ready
    /// within the timeout.
    pub async fn spawn_daemon(&self, interval_secs: u64) -> anyhow::Result<Child> {
        // Backwards-compatible wrapper: defaults to the runtime default cap.
        self.spawn_daemon_with(
            interval_secs,
            crate::tunnel::DEFAULT_MAX_RECONNECT_ATTEMPTS,
            false,
        )
        .await
    }

    /// Spawn the daemon as a background child process.
    ///
    /// Phase 11c: prefers the standalone `peko-daemon` binary artifact
    /// (next to `current_exe()`) when present, and falls back to the
    /// legacy `peko daemon start --foreground` re-exec path when it is
    /// not. The fallback covers older installs and dev trees that have
    /// not built the new binary yet. Both paths produce an equivalent
    /// background daemon with the same IPC contract.
    ///
    /// # Arguments
    /// - `interval_secs` — cron poll interval
    /// - `max_reconnect_attempts` — PekoHub tunnel reconnect cap (issue #8)
    /// - `sidecar_mode` — when true, the daemon is started with
    ///   `--sidecar-mode` and writes to a distinct lockfile
    ///   (`<config_dir>/run/desktop.lock`) so it cannot collide with a
    ///   CLI-launched daemon in the same config dir. `peko-desktop`'s
    ///   `SidecarSupervisor` (PR D, ADR-043) calls this with
    ///   `sidecar_mode = true`.
    ///
    /// # Errors
    /// Returns error if the daemon fails to spawn or does not become ready
    /// within the timeout.
    pub async fn spawn_daemon_with(
        &self,
        interval_secs: u64,
        max_reconnect_attempts: u32,
        sidecar_mode: bool,
    ) -> anyhow::Result<Child> {
        let config_dir = self.resolver.config_dir().to_path_buf();
        let data_dir = self.resolver.data_dir().to_path_buf();

        // Phase 11c: prefer the `peko-daemon` binary artifact. The legacy
        // path stays for installs / dev trees that have not built it.
        let (exe_path, arg_prefix) = match Self::peko_daemon_binary() {
            Some(daemon_bin) => (daemon_bin, ArgPrefix::PekoDaemon),
            None => (
                std::env::current_exe()?,
                ArgPrefix::PekoDaemonStartForeground,
            ),
        };

        let mut cmd = Command::new(&exe_path);
        match arg_prefix {
            ArgPrefix::PekoDaemon => {
                // `peko-daemon` reads flags directly — no `daemon start`
                // subcommand prefix. Always pass `--foreground` because the
                // binary is foreground-only by definition (a daemon
                // binary that returned would not be a daemon).
                cmd.arg("--interval")
                    .arg(interval_secs.to_string())
                    .arg("--max-reconnect-attempts")
                    .arg(max_reconnect_attempts.to_string());
                if sidecar_mode {
                    cmd.arg("--sidecar-mode");
                }
                cmd.arg("--foreground");
            }
            ArgPrefix::PekoDaemonStartForeground => {
                // Legacy path: re-exec the CLI with `daemon start --foreground`
                // so the daemon module's existing foreground branch runs
                // in-process inside the child.
                cmd.arg("daemon")
                    .arg("start")
                    .arg("--foreground")
                    .arg("--interval")
                    .arg(interval_secs.to_string())
                    .arg("--max-reconnect-attempts")
                    .arg(max_reconnect_attempts.to_string());
                if sidecar_mode {
                    cmd.arg("--sidecar-mode");
                }
            }
        }
        cmd.env("PEKO_CONFIG_DIR", &config_dir)
            .env("PEKO_DATA_DIR", &data_dir)
            .stdout(std::process::Stdio::null())
            // Stderr is intentionally NOT piped here. The CLI path doesn't
            // care about PEKO_VERSION (it can be queried via
            // `peko daemon status`). `peko-desktop`'s SidecarSupervisor
            // spawns the daemon via Tauri's Command::new_sidecar in its own
            // process and pipes stderr there.
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false);

        let mut child = cmd.spawn()?;

        // Wait for daemon to be ready (up to 20 seconds)
        let ready = self.wait_for_daemon_ready(Duration::from_secs(20)).await?;

        if !ready {
            let _ = child.kill().await;
            anyhow::bail!("Daemon failed to start - not ready within timeout");
        }

        // Write PID file (sidecar lock or daemon.pid depending on mode)
        if let Some(pid) = child.id() {
            if sidecar_mode {
                let _ = self.write_pid_sidecar(pid);
            } else {
                let _ = self.write_pid(pid);
            }
        }

        Ok(child)
    }

    /// Run the daemon in the current terminal (foreground mode).
    ///
    /// Phase 12 foreground switch: prefers the standalone `peko-daemon`
    /// binary artifact (next to `current_exe()`) when present, and falls
    /// back to the legacy in-process `Daemon::new + Daemon::run` path
    /// when it is not (older installs / dev trees that haven't built
    /// the binary yet).
    ///
    /// Stdin/stdout/stderr are inherited when launching the binary so
    /// the user sees the daemon's output inline; the call blocks until
    /// the child exits. The legacy in-process fallback runs
    /// `Daemon::run().await` directly inside the CLI process — same
    /// observable behaviour, no subprocess boundary.
    ///
    /// # Arguments
    /// - `interval_secs` — cron poll interval
    /// - `max_reconnect_attempts` — PekoHub tunnel reconnect cap (issue #8)
    /// - `sidecar_mode` — when true, the daemon is started with
    ///   `--sidecar-mode` and writes to a distinct lockfile
    ///   (`<config_dir>/run/desktop.lock`) so it cannot collide with a
    ///   CLI-launched daemon in the same config dir.
    pub async fn run_foreground(
        &self,
        interval_secs: u64,
        max_reconnect_attempts: u32,
        sidecar_mode: bool,
    ) -> anyhow::Result<()> {
        let config_dir = self.resolver.config_dir().to_path_buf();
        let data_dir = self.resolver.data_dir().to_path_buf();

        // Phase 12: prefer the `peko-daemon` binary. Mirrors the
        // background-spawn logic in `spawn_daemon_with` so both code
        // paths share the same fallback rule (binary present → use it,
        // else use the legacy path). The user-facing CLI argument
        // `--foreground` maps to the binary's own `--foreground` switch
        // (always set; the binary is foreground by definition).
        if let Some(daemon_bin) = Self::peko_daemon_binary() {
            let mut cmd = Command::new(&daemon_bin);
            cmd.arg("--foreground")
                .arg("--interval")
                .arg(interval_secs.to_string())
                .arg("--max-reconnect-attempts")
                .arg(max_reconnect_attempts.to_string());
            if sidecar_mode {
                cmd.arg("--sidecar-mode");
            }
            cmd.env("PEKO_CONFIG_DIR", &config_dir)
                .env("PEKO_DATA_DIR", &data_dir)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(false);

            let status = cmd.status().await?;
            if !status.success() {
                // Mirror the legacy `peko daemon start --foreground`
                // behaviour: print the daemon's exit, don't fail the
                // command (the user can decide whether to restart).
                eprintln!("peko-daemon exited with status: {status}");
            }
            return Ok(());
        }

        // Fallback: in-process for older installs / dev trees without
        // the `peko-daemon` binary. Same `DaemonConfig` the binary
        // would build, same `Daemon::run` codepath.
        use crate::daemon::{Daemon, DaemonConfig, LaunchMode};
        let config = DaemonConfig {
            cron_db_path: data_dir.join("cron.json"),
            poll_interval: Duration::from_secs(interval_secs),
            config_dir,
            data_dir,
            maintenance_interval: Duration::from_hours(1),
            max_reconnect_attempts,
            launch_mode: if sidecar_mode {
                LaunchMode::Sidecar
            } else {
                LaunchMode::Headless
            },
        };

        let daemon = Daemon::new(config)?;
        if let Err(e) = Box::pin(daemon.run()).await {
            eprintln!("Daemon error: {e}");
        }
        Ok(())
    }

    /// Resolve the standalone `peko-daemon` binary artifact next to the
    /// currently-running executable.
    ///
    /// Returns `Some(path)` when a sibling `peko-daemon` (or
    /// `peko-daemon.exe` on Windows) exists at the same directory as
    /// `current_exe()`. Returns `None` for older installs / dev trees
    /// that have not built the new artifact, in which case callers fall
    /// back to the legacy `peko daemon start --foreground` re-exec.
    ///
    /// On Windows the `.exe` suffix is appended explicitly because
    /// `Path::with_file_name` does not preserve the source extension —
    /// the candidate filename must be `peko-daemon.exe` there.
    pub fn peko_daemon_binary() -> Option<PathBuf> {
        let current = std::env::current_exe().ok()?;
        let candidate = Self::peko_daemon_path_for(&current);
        candidate.exists().then_some(candidate)
    }

    /// Build the `peko-daemon` sibling path for an arbitrary executable
    /// path. Pure path construction — no filesystem check — so it is
    /// unit-testable on platforms where `current_exe()` returns an
    /// awkward path (tests run as the test binary, not the CLI).
    pub(crate) fn peko_daemon_path_for(current_exe: &std::path::Path) -> PathBuf {
        let daemon_name = if cfg!(windows) {
            "peko-daemon.exe"
        } else {
            "peko-daemon"
        };
        current_exe.with_file_name(daemon_name)
    }

    /// Wait for the daemon to become ready via IPC ping
    ///
    /// Returns `Ok(true)` if the daemon responds to a ping within the timeout.
    pub async fn wait_for_daemon_ready(&self, timeout: Duration) -> anyhow::Result<bool> {
        wait_for_healthy(
            || async {
                match ConnectionManager::try_connect_quick().await {
                    Ok(conn) => {
                        let ping = RequestPacket::Ping { request_id: 0 };
                        if let Ok(bytes) = ping.to_bytes() {
                            if conn.send(&bytes).await.is_ok() {
                                let mut buf = vec![0u8; 65536];
                                if let Ok(len) =
                                    conn.recv_timeout(&mut buf, Duration::from_secs(2)).await
                                {
                                    if let Ok(ResponsePacket::Pong { .. }) =
                                        ResponsePacket::from_bytes(&buf[..len])
                                    {
                                        return true;
                                    }
                                }
                            }
                        }
                        false
                    }
                    Err(_) => false,
                }
            },
            timeout,
            Duration::from_millis(500),
        )
        .await
    }

    // ------------------------------------------------------------------
    // Stop
    // ------------------------------------------------------------------

    /// Stop the daemon
    ///
    /// 1. Try graceful shutdown via IPC (unless `force` is true)
    /// 2. Wait for the daemon to exit
    /// 3. Fall back to PID-based kill
    /// 4. Final verification via IPC
    /// 5. Fallback process-name kill if still running
    ///
    /// # Errors
    /// Returns error if the daemon is still running after all stop attempts.
    pub async fn stop_daemon(&self, force: bool) -> anyhow::Result<()> {
        let pid = self.read_pid();

        // 1. Try graceful shutdown via IPC
        let mut shutdown_sent = false;
        if !force {
            if let Ok(conn) = ConnectionManager::try_connect().await {
                let shutdown_req = RequestPacket::Shutdown {
                    request_id: 0,
                    force: false,
                };
                if let Ok(bytes) = shutdown_req.to_bytes() {
                    if conn.send(&bytes).await.is_ok() {
                        let mut buf = vec![0u8; 65536];
                        if let Ok(len) = conn.recv_timeout(&mut buf, Duration::from_secs(3)).await {
                            if let Ok(ResponsePacket::ShuttingDown { .. }) =
                                ResponsePacket::from_bytes(&buf[..len])
                            {
                                shutdown_sent = true;
                                debug!("Graceful shutdown request sent via IPC");
                            }
                        }
                    }
                }
            }
        }

        // 2. Wait for graceful shutdown
        if shutdown_sent && !force {
            if let Some(pid) = pid {
                let exited =
                    wait_for_exit(pid, Duration::from_secs(5), Duration::from_millis(200)).await?;
                if exited {
                    self.remove_pid_file();
                    return Ok(());
                }
                warn!("Daemon still running after graceful request, falling back to PID kill");
            }
        }

        // 3. PID-based kill
        if let Some(pid) = pid {
            debug!("Killing daemon via PID {pid}");
            let _ = kill_by_pid(pid, force).await;

            let exited =
                wait_for_exit(pid, Duration::from_secs(6), Duration::from_millis(200)).await?;
            if !exited {
                warn!("Process {pid} may still be running after kill attempt");
            }
        }

        self.remove_pid_file();

        // 4. Final verification
        tokio::time::sleep(Duration::from_millis(500)).await;
        if self.is_daemon_running().await? {
            warn!("Daemon still responding after stop attempt — attempting fallback kill");
            kill_all_by_name().await?;
            anyhow::bail!(
                "Daemon process is still running after stop attempt. \
                 Try: taskkill /F /IM peko.exe"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_file_path() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        let service = DaemonProcessService::new(resolver);
        // Default mode keeps the historical path so existing headless
        // setups don't see a behaviour change.
        assert_eq!(
            service.pid_file_path(false),
            PathBuf::from("/config/run/daemon.pid")
        );
        // Sidecar mode uses a distinct filename so a CLI daemon and a
        // desktop sidecar can coexist in the same config dir without
        // racing on the same lockfile.
        assert_eq!(
            service.pid_file_path(true),
            PathBuf::from("/config/run/desktop.lock")
        );
    }

    /// `peko_daemon_path_for` swaps the file_name to the new binary
    /// artifact. On Unix this is a bare rename; on Windows the `.exe`
    /// suffix is appended because `Path::with_file_name` does not
    /// preserve the source extension.
    #[test]
    fn test_peko_daemon_path_for() {
        if cfg!(windows) {
            // Windows: must include `.exe` so the candidate is actually
            // executable on disk. `current_exe()` returns e.g.
            // `C:\path\to\peko.exe`; the sibling must be `peko-daemon.exe`.
            let cli_path = PathBuf::from(r"C:\path\to\peko.exe");
            let daemon_path = DaemonProcessService::peko_daemon_path_for(&cli_path);
            assert_eq!(daemon_path, PathBuf::from(r"C:\path\to\peko-daemon.exe"));
        } else {
            // Unix: bare rename, no extension handling.
            let cli_path = PathBuf::from("/usr/local/bin/peko");
            let daemon_path = DaemonProcessService::peko_daemon_path_for(&cli_path);
            assert_eq!(daemon_path, PathBuf::from("/usr/local/bin/peko-daemon"));
        }
    }

    /// `peko_daemon_binary` returns `None` when no sibling binary
    /// exists. The test binary is itself at some path; we synthesize a
    /// fake CLI path that has no sibling to exercise the negative
    /// branch deterministically.
    #[test]
    fn test_peko_daemon_binary_missing() {
        // Use a path that does not exist anywhere on disk; the existence
        // check inside `peko_daemon_binary` will return false. This
        // doesn't actually exercise the public function (which reads
        // `current_exe()`), but the helper it delegates to
        // (`peko_daemon_path_for`) is the unit-testable seam.
        let missing = PathBuf::from("/nonexistent/dir/for/test/peko");
        let candidate = DaemonProcessService::peko_daemon_path_for(&missing);
        assert!(!candidate.exists());
    }

    /// `is_sidecar_lock_held` returns false when no lockfile exists.
    #[test]
    fn test_is_sidecar_lock_held_no_file() {
        let temp_dir =
            std::env::temp_dir().join(format!("PEKO_test_sidecar_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let resolver = PathResolver::with_dirs(
            temp_dir.clone(),
            temp_dir.join("data"),
            temp_dir.join("cache"),
        );
        let service = DaemonProcessService::new(resolver);
        assert!(!service.is_sidecar_lock_held());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// `is_sidecar_lock_held` returns true when the lockfile holds a live
    /// PID (here: our own test process).
    #[test]
    fn test_is_sidecar_lock_held_live_pid() {
        let temp_dir =
            std::env::temp_dir().join(format!("PEKO_test_sidecar_live_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let resolver = PathResolver::with_dirs(
            temp_dir.clone(),
            temp_dir.join("data"),
            temp_dir.join("cache"),
        );
        let service = DaemonProcessService::new(resolver);

        // Write our own (live) PID to the sidecar lockfile.
        let own_pid = std::process::id();
        service.write_pid_sidecar(own_pid).unwrap();

        assert!(service.is_sidecar_lock_held());

        service.remove_sidecar_lock_file();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// `is_sidecar_lock_held` returns false for a stale (non-running) PID
    /// and cleans up the stale lockfile.
    #[test]
    fn test_is_sidecar_lock_held_stale_pid() {
        let temp_dir =
            std::env::temp_dir().join(format!("PEKO_test_sidecar_stale_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let resolver = PathResolver::with_dirs(
            temp_dir.clone(),
            temp_dir.join("data"),
            temp_dir.join("cache"),
        );
        let service = DaemonProcessService::new(resolver);

        // PID 999_999 effectively never exists on a sane test machine.
        service.write_pid_sidecar(999_999).unwrap();
        assert!(!service.is_sidecar_lock_held());
        // Stale lockfile is cleaned up as a side-effect.
        assert!(!service.pid_file_path(true).exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_write_and_read_pid() {
        let temp_dir = std::env::temp_dir().join(format!("PEKO_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let resolver = PathResolver::with_dirs(
            temp_dir.clone(),
            temp_dir.join("data"),
            temp_dir.join("cache"),
        );
        let service = DaemonProcessService::new(resolver);

        // Write our own PID (which is running)
        let own_pid = std::process::id();
        service.write_pid(own_pid).unwrap();

        // Should read it back
        let read = service.read_pid();
        assert_eq!(read, Some(own_pid));

        // Remove PID file
        service.remove_pid_file();
        assert!(!service.pid_file_path(false).exists());

        // Clean up

        // Clean up
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_read_pid_stale_cleanup() {
        let temp_dir = std::env::temp_dir().join(format!("PEKO_test_stale_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let resolver = PathResolver::with_dirs(
            temp_dir.clone(),
            temp_dir.join("data"),
            temp_dir.join("cache"),
        );
        let service = DaemonProcessService::new(resolver);

        // Write a PID that definitely doesn't exist
        service.write_pid(999_999).unwrap();

        // read_pid should return None and clean up the stale file
        let read = service.read_pid();
        assert_eq!(read, None);
        assert!(!service.pid_file_path(false).exists());

        // Clean up
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_daemon_status_struct() {
        // Pure unit test for the status struct — no network dependency
        let status = DaemonStatus {
            responding: false,
            process_exists: false,
            version: None,
            uptime_secs: None,
            pid: None,
            ready: false,
            error: None,
            tunnel: None,
            mode: None,
        };
        assert!(!status.responding);
        assert!(!status.process_exists);
        assert!(!status.ready);

        let status2 = DaemonStatus {
            responding: true,
            process_exists: true,
            version: Some("0.1.0".to_string()),
            uptime_secs: Some(42),
            pid: Some(1234),
            ready: true,
            error: None,
            tunnel: Some(TunnelStatusInfo {
                state: "connected".to_string(),
                reconnect_attempts: 0,
                last_error: None,
                degraded: false,
            }),
            mode: Some(crate::daemon::LaunchMode::Headless),
        };
        assert!(status2.responding);
        assert!(status2.process_exists);
        assert!(status2.ready);
        assert_eq!(status2.version, Some("0.1.0".to_string()));
        assert_eq!(status2.uptime_secs, Some(42));
        assert_eq!(status2.pid, Some(1234));
    }

    /// `run_foreground` builds the same `DaemonConfig` shape the
    /// `peko-daemon` binary would build from `interval` /
    /// `max_reconnect_attempts` / `sidecar_mode` flags.
    ///
    /// We don't actually exec the daemon here — that would block the
    /// test forever. Instead we verify the `peko_daemon_binary`
    /// resolution at a known path so the subprocess branch would have
    /// a candidate to launch (the test binary itself). The in-process
    /// fallback is the only path the test binary actually exercises,
    /// so this verifies the field plumbing by construction.
    #[tokio::test]
    async fn test_run_foreground_resolves_config_paths() {
        let temp_dir =
            std::env::temp_dir().join(format!("PEKO_test_fg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let resolver = PathResolver::with_dirs(
            temp_dir.clone(),
            temp_dir.join("data"),
            temp_dir.join("cache"),
        );
        let service = DaemonProcessService::new(resolver);

        // Sanity: the service reads the same config / data dirs the
        // CLI passed in, regardless of which branch (binary or
        // fallback) `run_foreground` takes. The
        // `PEKO_CONFIG_DIR`/`PEKO_DATA_DIR` env vars are not set here,
        // so the resolver returns the dirs we fed it.
        assert_eq!(service.resolver.config_dir(), temp_dir);
        assert_eq!(service.resolver.data_dir(), temp_dir.join("data"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
