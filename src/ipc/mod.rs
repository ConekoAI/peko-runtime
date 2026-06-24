//! IPC Module — Datagram/Stream Communication Between CLI and Daemon
//!
//! This module replaces the HTTP API (Axum + reqwest) with a lightweight
//! request/response protocol. The daemon binds a transport; CLI commands
//! send packets to it.
//!
//! ## Transport
//!
//! - **Unix**: Unix domain datagram socket at `~/.peko/run/daemon.sock`
//!   (file mode 0600 — kernel-enforced peer identity).
//! - **Windows**: Named pipe at `\\.\pipe\peko-{username}` (ADR-038)
//!   with a DACL that grants the current user Generic All — kernel-enforced
//!   peer identity analogous to Unix 0600. Falls back to UDP on bind failure.
//! - **All platforms**: UDP on `127.0.0.1:11435` is the explicit-remote
//!   transport and the universal last-resort safety net.
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
pub mod errors;
pub mod packet;
pub mod pipe_security;
pub mod response_sink;
pub mod server;
pub mod stream;

pub use client::DaemonClient;
pub use connection::{ConnectionHandle, ConnectionManager};
pub use errors::unexpected_response;
pub use packet::{AuthCredential, AuthHeader, AuthenticatedRequest, RequestPacket, ResponsePacket};
pub use server::IpcServer;
pub use stream::PacketStream;

/// Default UDP port for daemon IPC
pub const DEFAULT_PORT: u16 = 11435;

/// Default host for UDP daemon IPC
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Environment variable to override Unix socket path
pub const DAEMON_SOCK_ENV: &str = "PEKO_DAEMON_SOCK";

/// Environment variable to override UDP address
pub const DAEMON_ADDR_ENV: &str = "PEKO_DAEMON_ADDR";

/// Environment variable to override the Windows named-pipe name (ADR-038).
///
/// On Unix this constant is unused (the env var is read but the
/// `ConnectionManager` discovery ladder never tries a pipe step).
pub const DAEMON_PIPE_ENV: &str = "PEKO_DAEMON_PIPE";

/// Daemon mode environment variable
pub const DAEMON_MODE_ENV: &str = "PEKO_DAEMON";

/// Get the default socket path for the current platform
pub fn default_socket_path() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|d| d.join(".peko").join("run").join("daemon.sock"))
        .unwrap_or_else(|| {
            std::path::PathBuf::from(".peko")
                .join("run")
                .join("daemon.sock")
        })
}

/// Get the default Windows named-pipe name (ADR-038).
///
/// The format is `\\.\pipe\peko-{username}` where `{username}` is
/// sanitised to characters valid in a Win32 pipe name (rejects `\`, `?`,
/// `*`, `<`, `>`, `|`, `"` and is capped at 64 characters to leave
/// headroom under the 256-char `MAX_PATH` limit for the full pipe path).
///
/// On Unix this function does not exist — callers that branch on
/// platform gate the call site with `#[cfg(windows)]`.
#[cfg(windows)]
pub fn default_pipe_name() -> String {
    use std::env;
    let user = env::var("USERNAME")
        .or_else(|_| env::var("USER"))
        .unwrap_or_else(|_| "default".to_string());
    let safe: String = user
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '?' | '*' | '<' | '>' | '|' | '"') {
                '_'
            } else {
                c
            }
        })
        .take(64)
        .collect();
    format!(r"\\.\pipe\peko-{safe}")
}

/// Get the default PID file path
pub fn default_pid_path() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|d| d.join(".peko").join("run").join("daemon.pid"))
        .unwrap_or_else(|| {
            std::path::PathBuf::from(".peko")
                .join("run")
                .join("daemon.pid")
        })
}

/// Ensure the run directory exists
pub fn ensure_run_dir() -> std::io::Result<std::path::PathBuf> {
    let run_dir = dirs::home_dir()
        .map(|d| d.join(".peko").join("run"))
        .unwrap_or_else(|| std::path::PathBuf::from(".peko").join("run"));
    std::fs::create_dir_all(&run_dir)?;
    Ok(run_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_socket_path_contains_peko() {
        let path = default_socket_path();
        let s = path.to_string_lossy();
        assert!(s.contains(".peko"));
        assert!(s.contains("daemon.sock"));
    }

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_PORT, 11435);
        assert_eq!(DEFAULT_HOST, "127.0.0.1");
        assert_eq!(DAEMON_SOCK_ENV, "PEKO_DAEMON_SOCK");
        assert_eq!(DAEMON_ADDR_ENV, "PEKO_DAEMON_ADDR");
        assert_eq!(DAEMON_PIPE_ENV, "PEKO_DAEMON_PIPE");
        assert_eq!(DAEMON_MODE_ENV, "PEKO_DAEMON");
    }

    #[cfg(windows)]
    #[test]
    fn test_default_pipe_name_format() {
        let name = default_pipe_name();
        assert!(
            name.starts_with(r"\\.\pipe\peko-"),
            "default pipe name must start with the Windows pipe prefix and `peko-`: {name}"
        );
        // The username suffix must be within the Win32 pipe-name length
        // budget. The full path is allowed up to 256 chars; we cap the
        // suffix at 64 (see `default_pipe_name`), so the full string is
        // bounded by 12 + 64 = 76 chars.
        assert!(
            name.len() <= 256,
            "pipe name exceeds Win32 MAX_PATH (256): {name}"
        );
    }
}
