//! Connection Manager — Daemon Discovery and Socket Lifecycle
//!
//! This module handles the real-world concerns of finding and
//! maintaining a connection to the daemon:
//!
//! 1. **Discovery**: Find the daemon via env var, default path, or port
//! 2. **Reconnection**: Handle transient failures
//!
//! `ConnectionManager` is separate from `DaemonClient` per SRP.
//! `DaemonClient` sends/receives packets; `ConnectionManager` handles
//! connection lifecycle.
//!
//! The CLI does NOT auto-start the daemon. Like Docker, the daemon must be
//! started explicitly by the user (`peko daemon start`). This avoids:
//! - Privilege boundary issues (daemon may need elevated permissions)
//! - Ambiguity about where to start the daemon (local vs remote)
//! - Unexpected resource consumption from background processes
//! - System stability issues from implicit service startup
//!
//! Discovery ladder (ADR-021 + ADR-038):
//!   1. `PEKO_DAEMON_SOCK` env var → Unix socket (Unix only)
//!   2. `PEKO_DAEMON_ADDR` env var → UDP address
//!   3. `PEKO_DAEMON_PIPE` env var → Windows named pipe (Windows only)
//!   4. Default Unix socket / default named pipe (per platform)
//!   5. Default UDP — the universal last-resort safety net

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
#[cfg(unix)]
use tokio::net::UnixDatagram;
use tracing::debug;

#[cfg(test)]
use super::default_pid_path;
#[cfg(windows)]
use super::{default_pipe_name, DAEMON_PIPE_ENV};
use super::{default_socket_path, DAEMON_ADDR_ENV, DAEMON_SOCK_ENV, DEFAULT_HOST, DEFAULT_PORT};

/// Platform-specific socket handle
///
/// On Unix, this wraps a `UnixDatagram` (reliable, file-permission auth).
/// On Windows, this wraps a `NamedPipeClient` (reliable, kernel-enforced
/// DACL auth per ADR-038) or a `UdpSocket` (unreliable, no auth — the
/// universal last-resort fallback).
///
/// Unix/UDP sockets are wrapped in `Arc` so that cloning the handle
/// shares the same underlying socket — this ensures responses from the
/// daemon reach the receiver task (which uses the cloned handle).
/// Windows named-pipe clients are wrapped in `Arc<Mutex<…>>` because
/// each `try_clone` would otherwise open a new connection (the kernel
/// doesn't support sharing a single pipe handle), but the receiver
/// task needs to read from the same connection the sender wrote to. The
/// mutex is uncontended in practice because the call pattern is
/// sequential send-then-receive.
///
/// Mirrors the server-side `ServerSocket` enum in `server.rs`.
#[derive(Debug, Clone)]
pub enum ConnectionHandle {
    /// Unix domain datagram socket (Unix only)
    #[cfg(unix)]
    Unix {
        socket: Arc<UnixDatagram>,
        path: PathBuf,
    },
    /// UDP socket (Windows fallback, or Unix opt-in)
    Udp {
        socket: Arc<UdpSocket>,
        addr: String,
    },
    /// Windows named-pipe client (ADR-038).
    #[cfg(windows)]
    NamedPipe {
        client: Arc<tokio::sync::Mutex<tokio::net::windows::named_pipe::NamedPipeClient>>,
        name: String,
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
            #[cfg(windows)]
            Self::NamedPipe { client, .. } => {
                use tokio::io::AsyncWriteExt;
                let mut g = client.lock().await;
                g.write_all(bytes).await?;
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
            Self::Unix { socket, .. } => socket.recv(buf).await?,
            Self::Udp { socket, .. } => socket.recv(buf).await?,
            #[cfg(windows)]
            Self::NamedPipe { client, .. } => {
                use tokio::io::AsyncReadExt;
                let mut g = client.lock().await;
                g.read(buf).await?
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

    /// Clone the handle (shares the same underlying socket)
    ///
    /// # Errors
    /// Returns error if socket creation fails
    pub async fn try_clone(&self) -> anyhow::Result<Self> {
        match self {
            #[cfg(unix)]
            Self::Unix { socket, path } => {
                // CRITICAL: share the same underlying `Arc<UnixDatagram>`,
                // not a freshly-bound temp socket. The daemon learns the
                // CLI's reply path from the first `recv_from` on the
                // request it receives, and `send_to`s responses back to
                // *that* path. If we bound a new path here, the receiver
                // task spawned by `spawn_receiver` would sit on a socket
                // the daemon has no idea exists, and `peko send` would
                // hang forever waiting for response packets.
                //
                // Mirrors the UDP branch below: one socket, many handles.
                Ok(Self::Unix {
                    socket: Arc::clone(socket),
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
            #[cfg(windows)]
            Self::NamedPipe { client, name } => {
                // Share the same underlying `NamedPipeClient` via `Arc`
                // clone. The mutex serialises send/recv — uncontended in
                // practice because the existing `PacketStream` pattern
                // is sequential send-then-receive.
                Ok(Self::NamedPipe {
                    client: Arc::clone(client),
                    name: name.clone(),
                })
            }
        }
    }
}

/// Manages daemon discovery and connection lifecycle
pub struct ConnectionManager;

impl ConnectionManager {
    /// Connect to the daemon, failing if it's not running.
    ///
    /// The CLI does NOT auto-start the daemon. Start it manually with:
    ///   peko daemon start
    ///
    /// # Errors
    /// Returns error if daemon is not reachable
    pub async fn connect() -> anyhow::Result<ConnectionHandle> {
        Self::try_connect().await
    }

    /// Try to connect to an already-running daemon.
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
            #[cfg(unix)]
            if let Ok(handle) = Self::connect_unix_with_timeout(&sock_path, ping_timeout).await {
                return Ok(handle);
            }
            // On non-Unix, fall through (the env var is silently ignored).
        }

        // 2. Try env var UDP address
        if let Ok(addr) = std::env::var(DAEMON_ADDR_ENV) {
            debug!("Trying UDP from env: {}", addr);
            if let Ok(handle) = Self::connect_udp_with_timeout(&addr, ping_timeout).await {
                return Ok(handle);
            }
        }

        // 3. Try env var Windows named pipe (ADR-038)
        #[cfg(windows)]
        {
            if let Ok(name) = std::env::var(DAEMON_PIPE_ENV) {
                debug!("Trying Windows named pipe from env: {}", name);
                if let Ok(handle) =
                    Self::connect_pipe_with_timeout(name.clone(), ping_timeout).await
                {
                    return Ok(handle);
                }
            }
        }

        // 4. Try default Unix socket (Unix) or default pipe (Windows)
        #[cfg(unix)]
        {
            let default_sock = default_socket_path();
            debug!("Trying default Unix socket: {}", default_sock.display());
            if let Ok(handle) =
                Self::connect_unix_with_timeout(&default_sock.to_string_lossy(), ping_timeout).await
            {
                return Ok(handle);
            }
        }
        #[cfg(windows)]
        {
            let default_pipe = default_pipe_name();
            debug!("Trying default Windows named pipe: {}", default_pipe);
            if let Ok(handle) = Self::connect_pipe_with_timeout(default_pipe, ping_timeout).await {
                return Ok(handle);
            }
        }

        // 5. Try default UDP — the universal last-resort safety net
        let default_addr = format!("{}:{}", DEFAULT_HOST, DEFAULT_PORT);
        debug!("Trying default UDP: {}", default_addr);
        if let Ok(handle) = Self::connect_udp_with_timeout(&default_addr, ping_timeout).await {
            return Ok(handle);
        }

        anyhow::bail!("No daemon found")
    }

    #[cfg(unix)]
    async fn connect_unix_with_timeout(
        path: &str,
        timeout: Duration,
    ) -> anyhow::Result<ConnectionHandle> {
        let path_buf = PathBuf::from(path);
        if !path_buf.exists() {
            anyhow::bail!("Unix socket does not exist: {}", path);
        }

        let tmp_path = std::env::temp_dir().join(format!("PEKO_cli_{}.sock", std::process::id()));
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
            socket: Arc::new(socket),
            path: path_buf,
        })
    }

    /// Connect to a Windows named pipe (ADR-038) and round-trip a Ping.
    ///
    /// Mirrors `connect_unix_with_timeout` and `connect_udp_with_timeout`
    /// in shape. Tokio's `ClientOptions::open` is synchronous (it calls
    /// `CreateFileW`), so we wrap the whole open-and-ping in
    /// `spawn_blocking` to keep the async runtime non-blocked, then
    /// apply a timeout.
    #[cfg(windows)]
    async fn connect_pipe_with_timeout(
        name: String,
        timeout: Duration,
    ) -> anyhow::Result<ConnectionHandle> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let name_for_blocking = name.clone();
        let join = tokio::task::spawn_blocking(move || -> std::io::Result<NamedPipeHandle> {
            let client =
                tokio::net::windows::named_pipe::ClientOptions::new().open(&name_for_blocking)?;
            Ok(NamedPipeHandle {
                client,
                name: name_for_blocking,
            })
        });

        let mut handle = match tokio::time::timeout(timeout, join).await {
            Ok(Ok(Ok(h))) => h,
            Ok(Ok(Err(e))) => {
                anyhow::bail!("Named pipe connect error ({name}): {e}");
            }
            Ok(Err(join_err)) => {
                anyhow::bail!("Named pipe connect task panicked: {join_err}");
            }
            Err(_) => {
                anyhow::bail!("Named pipe connect timeout: {name}");
            }
        };

        // Test connectivity with a Ping. We hold `handle.client` outside
        // the Mutex for the duration of the write+read; the receiver
        // task only starts after this function returns and the handle
        // is wrapped in the Mutex, so there is no contention.
        let ping = super::packet::RequestPacket::Ping { request_id: 0 };
        let ping_bytes = ping.to_bytes()?;
        handle
            .client
            .write_all(&ping_bytes)
            .await
            .map_err(|e| anyhow::anyhow!("Named pipe write error: {e}"))?;

        let mut buf = vec![0u8; 65536];
        let len = tokio::time::timeout(timeout, handle.client.read(&mut buf))
            .await
            .map_err(|_| anyhow::anyhow!("Named pipe read timeout"))?
            .map_err(|e| anyhow::anyhow!("Named pipe read error: {e}"))?;

        let response = super::packet::ResponsePacket::from_bytes(&buf[..len])?;
        match response {
            super::packet::ResponsePacket::Pong { .. } => {}
            _ => anyhow::bail!("Unexpected response to ping: {:?}", response),
        }

        Ok(ConnectionHandle::NamedPipe {
            client: Arc::new(tokio::sync::Mutex::new(handle.client)),
            name: handle.name,
        })
    }

    async fn connect_udp_with_timeout(
        addr: &str,
        timeout: Duration,
    ) -> anyhow::Result<ConnectionHandle> {
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
}

/// Local helper struct used by `connect_pipe_with_timeout` to shuttle the
/// freshly-opened `NamedPipeClient` and its name out of the blocking
/// `spawn_blocking` closure. On non-Windows builds the struct is unused
/// and the type alias resolves to a unit stub.
#[cfg(windows)]
struct NamedPipeHandle {
    client: tokio::net::windows::named_pipe::NamedPipeClient,
    name: String,
}

#[cfg(all(not(windows), not(unix)))]
struct NamedPipeHandle {}

/// Stub for non-Unix platforms
#[cfg(not(unix))]
impl ConnectionManager {
    #[allow(dead_code)]
    async fn connect_unix(_path: &str) -> anyhow::Result<ConnectionHandle> {
        anyhow::bail!("Unix sockets not supported on this platform")
    }

    #[allow(dead_code)]
    async fn connect_unix_with_timeout(
        _path: &str,
        _timeout: Duration,
    ) -> anyhow::Result<ConnectionHandle> {
        anyhow::bail!("Unix sockets not supported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::default_socket_path;

    #[test]
    fn test_default_paths() {
        let sock = default_socket_path();
        assert!(sock.to_string_lossy().contains("daemon.sock"));

        let pid = default_pid_path();
        assert!(pid.to_string_lossy().contains("daemon.pid"));
    }
}
