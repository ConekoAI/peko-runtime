//! Connection Manager — Daemon Discovery, Auto-Start, and Socket Lifecycle
//!
//! This module handles the messy real-world concerns of finding and
//! maintaining a connection to the daemon:
//!
//! 1. **Discovery**: Find the daemon via env var, default path, or port
//! 2. **Auto-start**: Spawn the daemon if it's not running
//! 3. **Reconnection**: Handle transient failures
//!
//! `ConnectionManager` is separate from `DaemonClient` per SRP.
//! `DaemonClient` sends/receives packets; `ConnectionManager` handles
//! connection lifecycle.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
#[cfg(unix)]
use tokio::net::UnixDatagram;
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{default_pid_path, DAEMON_ADDR_ENV, DAEMON_SOCK_ENV, DEFAULT_HOST, DEFAULT_PORT};

/// Platform-specific socket handle
///
/// On Unix, this wraps a `UnixDatagram` (reliable, file-permission auth).
/// On Windows, this wraps a `UdpSocket` (unreliable, no auth).
///
/// UDP socket is wrapped in Arc so that `try_clone()` shares the same
/// underlying socket — this ensures responses from the daemon reach the
/// receiver task (which uses the cloned handle).
#[derive(Debug, Clone)]
pub enum ConnectionHandle {
    /// Unix domain datagram socket (Unix only)
    #[cfg(unix)]
    Unix {
        socket: UnixDatagram,
        path: PathBuf,
    },
    /// UDP socket (Windows fallback, or Unix opt-in)
    Udp {
        socket: Arc<UdpSocket>,
        addr: String,
    },
}

impl ConnectionHandle {
    /// Send a packet (raw bytes) to the daemon
    ///
    /// # Errors
    /// Returns error if send fails
    pub async fn send(&self, bytes: &[u8]) -> anyhow::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix { socket, path } => {
                socket.send_to(bytes, path).await?;
            }
            Self::Udp { socket, addr } => {
                socket.send_to(bytes, addr).await?;
            }
        }
        Ok(())
    }

    /// Receive a packet (raw bytes) from the daemon
    ///
    /// # Errors
    /// Returns error if receive fails or times out
    pub async fn recv(&self, buf: &mut [u8]) -> anyhow::Result<usize> {
        let len = match self {
            #[cfg(unix)]
            Self::Unix { socket, .. } => {
                socket.recv(buf).await?
            }
            Self::Udp { socket, .. } => {
                socket.recv(buf).await?
            }
        };
        Ok(len)
    }

    /// Receive with timeout
    ///
    /// # Errors
    /// Returns error if receive fails or times out
    pub async fn recv_timeout(&self, buf: &mut [u8], timeout: Duration) -> anyhow::Result<usize> {
        match tokio::time::timeout(timeout, self.recv(buf)).await {
            Ok(Ok(len)) => Ok(len),
            Ok(Err(e)) => Err(e),
            Err(_) => anyhow::bail!("Receive timed out after {:?}", timeout),
        }
    }

    /// Clone the handle (creates a new socket bound to ephemeral port/path)
    ///
    /// # Errors
    /// Returns error if socket creation fails
    pub async fn try_clone(&self) -> anyhow::Result<Self> {
        match self {
            #[cfg(unix)]
            Self::Unix { path, .. } => {
                // Use a unique temp file per clone to avoid races and leaks.
                // Include a random suffix so concurrent clones don't collide.
                let rnd: u32 = std::process::id().wrapping_add(rand::random());
                let tmp_path = std::env::temp_dir()
                    .join(format!("pekobot_cli_{}_{}.sock", std::process::id(), rnd));
                let socket = UnixDatagram::bind(&tmp_path)?;
                Ok(Self::Unix {
                    socket,
                    path: path.clone(),
                })
            }
            Self::Udp { socket, addr } => {
                // Share the same underlying UDP socket so that responses
                // from the daemon (sent to our ephemeral port) are received
                // by both the original handle and the clone.
                Ok(Self::Udp {
                    socket: Arc::clone(socket),
                    addr: addr.clone(),
                })
            }
        }
    }
}

/// Path to a lock file used to prevent concurrent auto-start attempts
fn auto_start_lock_path() -> PathBuf {
    default_pid_path().with_file_name("daemon_autostart.lock")
}

/// Manages daemon discovery and connection lifecycle
pub struct ConnectionManager;

impl ConnectionManager {
    /// Connect to the daemon, failing if it's not running.
    ///
    /// Use `connect_or_start()` if you want auto-start behavior.
    ///
    /// # Errors
    /// Returns error if daemon is not reachable
    pub async fn connect() -> anyhow::Result<ConnectionHandle> {
        Self::try_connect().await
    }

    /// Connect to the daemon, auto-starting it if necessary.
    ///
    /// Uses a file-based lock to prevent multiple concurrent auto-start
    /// attempts from spawning a storm of daemon processes.
    ///
    /// Resolution order:
    /// 1. `PEKOBOT_DAEMON_SOCK` env var (Unix socket)
    /// 2. `PEKOBOT_DAEMON_ADDR` env var (UDP host:port)
    /// 3. Default Unix socket at `~/.pekobot/run/daemon.sock`
    /// 4. Default UDP at `127.0.0.1:11435`
    ///
    /// If none are reachable, auto-starts the daemon and retries with a
    /// short timeout (max ~3s). Fails fast if the daemon cannot be started.
    ///
    /// # Errors
    /// Returns error if daemon cannot be reached or started
    pub async fn connect_or_start() -> anyhow::Result<ConnectionHandle> {
        // Try to connect without auto-start first (quick 500ms check)
        if let Ok(handle) = Self::try_connect_quick().await {
            return Ok(handle);
        }

        // Check if another process is already trying to auto-start
        let lock_path = auto_start_lock_path();
        if lock_path.exists() {
            // Another process is auto-starting; wait briefly for it
            warn!("Another process appears to be auto-starting the daemon, waiting...");
            for i in 0..10 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                if let Ok(handle) = Self::try_connect_quick().await {
                    info!("Connected to daemon after waiting for auto-start (attempt {})", i + 1);
                    return Ok(handle);
                }
            }
            // Lock file is stale — remove it and try ourselves
            let _ = std::fs::remove_file(&lock_path);
        }

        // Create lock file
        if let Some(parent) = lock_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&lock_path, std::process::id().to_string());

        // Double-check: maybe daemon started while we were checking
        if let Ok(handle) = Self::try_connect_quick().await {
            let _ = std::fs::remove_file(&lock_path);
            return Ok(handle);
        }

        // Daemon not running — try to auto-start
        info!("Daemon not running, attempting auto-start...");
        let start_result = Self::auto_start_daemon().await;

        // Retry with short timeout (max ~3s)
        for i in 0..10 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Ok(handle) = Self::try_connect_quick().await {
                info!("Connected to daemon after auto-start");
                let _ = std::fs::remove_file(&lock_path);
                return Ok(handle);
            }
            debug!("Connection attempt {} after auto-start...", i + 1);
        }

        let _ = std::fs::remove_file(&lock_path);

        if let Err(e) = start_result {
            anyhow::bail!(
                "Daemon is not running and auto-start failed: {e}\n\
                 Start it manually: pekobot daemon start --foreground"
            )
        } else {
            anyhow::bail!(
                "Daemon is not running and did not respond after auto-start.\n\
                 Start it manually: pekobot daemon start --foreground"
            )
        }
    }

    /// Try to connect to an already-running daemon (no auto-start)
    ///
    /// Uses a 2-second timeout for the ping handshake.
    ///
    /// # Errors
    /// Returns error if daemon is not reachable
    pub async fn try_connect() -> anyhow::Result<ConnectionHandle> {
        Self::try_connect_with_timeout(Duration::from_secs(2)).await
    }

    /// Quick connect check with a short timeout.
    ///
    /// Used by retry loops to avoid accumulating long waits.
    /// Uses a 200ms timeout for the ping handshake.
    ///
    /// # Errors
    /// Returns error if daemon is not reachable within the short timeout
    pub async fn try_connect_quick() -> anyhow::Result<ConnectionHandle> {
        Self::try_connect_with_timeout(Duration::from_millis(200)).await
    }

    async fn try_connect_with_timeout(ping_timeout: Duration) -> anyhow::Result<ConnectionHandle> {
        // 1. Try env var Unix socket
        if let Ok(sock_path) = std::env::var(DAEMON_SOCK_ENV) {
            debug!("Trying Unix socket from env: {}", sock_path);
            if let Ok(handle) = Self::connect_unix_with_timeout(&sock_path, ping_timeout).await {
                return Ok(handle);
            }
        }

        // 2. Try env var UDP address
        if let Ok(addr) = std::env::var(DAEMON_ADDR_ENV) {
            debug!("Trying UDP from env: {}", addr);
            if let Ok(handle) = Self::connect_udp_with_timeout(&addr, ping_timeout).await {
                return Ok(handle);
            }
        }

        // 3. Try default Unix socket (Unix only)
        #[cfg(unix)]
        {
            let default_sock = default_socket_path();
            debug!("Trying default Unix socket: {}", default_sock.display());
            if let Ok(handle) = Self::connect_unix_with_timeout(&default_sock.to_string_lossy(), ping_timeout).await {
                return Ok(handle);
            }
        }

        // 4. Try default UDP
        let default_addr = format!("{}:{}", DEFAULT_HOST, DEFAULT_PORT);
        debug!("Trying default UDP: {}", default_addr);
        if let Ok(handle) = Self::connect_udp_with_timeout(&default_addr, ping_timeout).await {
            return Ok(handle);
        }

        anyhow::bail!("No daemon found")
    }

    /// Connect via Unix domain socket
    ///
    /// # Errors
    /// Returns error on Unix if socket doesn't exist or connection fails
    #[cfg(unix)]
    async fn connect_unix(path: &str) -> anyhow::Result<ConnectionHandle> {
        Self::connect_unix_with_timeout(path, Duration::from_secs(2)).await
    }

    #[cfg(unix)]
    async fn connect_unix_with_timeout(path: &str, timeout: Duration) -> anyhow::Result<ConnectionHandle> {
        let path_buf = PathBuf::from(path);
        if !path_buf.exists() {
            anyhow::bail!("Unix socket does not exist: {}", path);
        }

        let tmp_path = std::env::temp_dir()
            .join(format!("pekobot_cli_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&tmp_path);
        let socket = UnixDatagram::bind(&tmp_path)
            .map_err(|e| anyhow::anyhow!("Failed to bind Unix socket: {e}"))?;

        // Test connectivity with a ping
        let ping = super::packet::RequestPacket::Ping { request_id: 0 };
        let ping_bytes = ping.to_bytes()?;
        socket.send_to(&ping_bytes, &path_buf).await?;

        let mut buf = vec![0u8; 65536];
        let len = tokio::time::timeout(timeout, socket.recv(&mut buf))
            .await
            .map_err(|_| anyhow::anyhow!("Unix socket ping timeout"))?
            .map_err(|e| anyhow::anyhow!("Unix socket recv error: {e}"))?;

        let response = super::packet::ResponsePacket::from_bytes(&buf[..len])?;
        match response {
            super::packet::ResponsePacket::Pong { .. } => {}
            _ => anyhow::bail!("Unexpected response to ping: {:?}", response),
        }

        Ok(ConnectionHandle::Unix {
            socket,
            path: path_buf,
        })
    }

    /// Connect via UDP
    ///
    /// # Errors
    /// Returns error if UDP ping fails
    async fn connect_udp(addr: &str) -> anyhow::Result<ConnectionHandle> {
        Self::connect_udp_with_timeout(addr, Duration::from_secs(2)).await
    }

    async fn connect_udp_with_timeout(addr: &str, timeout: Duration) -> anyhow::Result<ConnectionHandle> {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind UDP socket: {e}"))?;

        // Test connectivity with a ping
        let ping = super::packet::RequestPacket::Ping { request_id: 0 };
        let ping_bytes = ping.to_bytes()?;
        socket.send_to(&ping_bytes, addr).await?;

        let mut buf = vec![0u8; 65536];
        let len = tokio::time::timeout(timeout, socket.recv(&mut buf))
            .await
            .map_err(|_| anyhow::anyhow!("UDP ping timeout"))?
            .map_err(|e| anyhow::anyhow!("UDP recv error: {e}"))?;

        let response = super::packet::ResponsePacket::from_bytes(&buf[..len])?;
        match response {
            super::packet::ResponsePacket::Pong { .. } => {}
            _ => anyhow::bail!("Unexpected response to ping: {:?}", response),
        }

        Ok(ConnectionHandle::Udp {
            socket: Arc::new(socket),
            addr: addr.to_string(),
        })
    }

    /// Auto-start the daemon as a background process
    ///
    /// # Errors
    /// Returns error if daemon binary cannot be found or spawn fails
    async fn auto_start_daemon() -> anyhow::Result<()> {
        let exe_path = std::env::current_exe()
            .map_err(|e| anyhow::anyhow!("Cannot find pekobot binary: {e}"))?;

        info!("Auto-starting daemon: {}", exe_path.display());

        let mut cmd = Command::new(&exe_path);
        cmd.arg("daemon")
            .arg("start")
            .arg("--foreground")
            .env("PEKOBOT_DAEMON", "1")
            .kill_on_drop(false)
            // Suppress child output so it doesn't flood the parent terminal
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // On Windows, also suppress the new console window
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        // Inherit config/data dirs from current process if set
        for var in ["PEKOBOT_CONFIG_DIR", "PEKOBOT_DATA_DIR", "PEKOBOT_CACHE_DIR"] {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }

        let child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn daemon process: {e}")
        })?;

        if let Some(pid) = child.id() {
            let pid_path = default_pid_path();
            if let Some(parent) = pid_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&pid_path, pid.to_string());
            debug!("Wrote daemon PID {} to {}", pid, pid_path.display());
        }

        // Don't wait for the child — it daemonizes itself
        // The connect retry loop will handle waiting for readiness
        Ok(())
    }
}

/// Stub for non-Unix platforms
#[cfg(not(unix))]
impl ConnectionManager {
    #[allow(dead_code)]
    async fn connect_unix(_path: &str) -> anyhow::Result<ConnectionHandle> {
        anyhow::bail!("Unix sockets not supported on this platform")
    }

    #[allow(dead_code)]
    async fn connect_unix_with_timeout(_path: &str, _timeout: Duration) -> anyhow::Result<ConnectionHandle> {
        anyhow::bail!("Unix sockets not supported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_paths() {
        let sock = default_socket_path();
        assert!(sock.to_string_lossy().contains("daemon.sock"));

        let pid = default_pid_path();
        assert!(pid.to_string_lossy().contains("daemon.pid"));
    }
}
