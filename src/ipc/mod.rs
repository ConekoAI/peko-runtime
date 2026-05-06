//! IPC Module — UDP/Unix Socket Communication Between CLI and Daemon
//!
//! This module replaces the HTTP API (Axum + reqwest) with a lightweight
//! datagram-based protocol. The daemon binds a socket; CLI commands send
//! packets to it.
//!
//! ## Transport
//!
//! - **Unix**: Unix domain datagram socket at `~/.pekobot/run/daemon.sock`
//! - **Windows**: UDP on `127.0.0.1:11435`
//!
//! ## Protocol
//!
//! Simple request/response with sequencing for streaming:
//! - CLI sends `RequestPacket` to daemon
//! - Daemon sends one or more `ResponsePacket`s back
//! - Streaming responses use `seq` numbers for ordering
//! - Heartbeat packets prevent hanging on dead daemon

pub mod client;
pub mod client_service;
pub mod connection;
pub mod packet;
pub mod server;
pub mod stream;

pub use client::DaemonClient;
pub use connection::{ConnectionHandle, ConnectionManager};
pub use packet::{RequestPacket, ResponsePacket};
pub use server::IpcServer;
pub use stream::PacketStream;

/// Default UDP port for daemon IPC
pub const DEFAULT_PORT: u16 = 11435;

/// Default host for UDP daemon IPC
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Environment variable to override Unix socket path
pub const DAEMON_SOCK_ENV: &str = "PEKOBOT_DAEMON_SOCK";

/// Environment variable to override UDP address
pub const DAEMON_ADDR_ENV: &str = "PEKOBOT_DAEMON_ADDR";

/// Daemon mode environment variable
pub const DAEMON_MODE_ENV: &str = "PEKOBOT_DAEMON";

/// Get the default socket path for the current platform
pub fn default_socket_path() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|d| d.join(".pekobot").join("run").join("daemon.sock"))
        .unwrap_or_else(|| {
            std::path::PathBuf::from(".pekobot")
                .join("run")
                .join("daemon.sock")
        })
}

/// Get the default PID file path
pub fn default_pid_path() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|d| d.join(".pekobot").join("run").join("daemon.pid"))
        .unwrap_or_else(|| {
            std::path::PathBuf::from(".pekobot")
                .join("run")
                .join("daemon.pid")
        })
}

/// Ensure the run directory exists
pub fn ensure_run_dir() -> std::io::Result<std::path::PathBuf> {
    let run_dir = dirs::home_dir()
        .map(|d| d.join(".pekobot").join("run"))
        .unwrap_or_else(|| std::path::PathBuf::from(".pekobot").join("run"));
    std::fs::create_dir_all(&run_dir)?;
    Ok(run_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_socket_path_contains_pekobot() {
        let path = default_socket_path();
        let s = path.to_string_lossy();
        assert!(s.contains(".pekobot"));
        assert!(s.contains("daemon.sock"));
    }

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_PORT, 11435);
        assert_eq!(DEFAULT_HOST, "127.0.0.1");
    }
}
