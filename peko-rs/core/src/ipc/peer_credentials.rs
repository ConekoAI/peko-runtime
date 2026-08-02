//! Peer-credential resolution for Unix datagram sockets.
//!
//! Linux: `SO_PEERCRED` returns `{ pid, uid, gid }`.
//! macOS: `LOCAL_PEERPID` returns just the PID; `getpeereid()` gives UID/GID.
//!
//! Session ID resolution (`getsid(pid)`) is portable across Linux/macOS
//! via libc.

use std::io;

/// PID + UID + GID of the connected peer, plus the session ID.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PeerCredentials {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
    pub sid: i32,
}

#[cfg(target_os = "linux")]
pub(crate) fn peer_credentials(fd: std::os::fd::RawFd) -> io::Result<PeerCredentials> {
    use std::mem::size_of;

    let mut pid: libc::pid_t = 0;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let mut len = size_of::<(libc::pid_t, libc::uid_t, libc::gid_t)>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut pid).cast::<libc::c_void>()
                as *mut _,
            &raw mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "peer credentials returned invalid PID",
        ));
    }
    let sid = unsafe { libc::getsid(pid) };
    if sid < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCredentials {
        pid,
        uid,
        gid,
        sid,
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn peer_credentials(_fd: std::os::fd::RawFd) -> io::Result<PeerCredentials> {
    // macOS's `LOCAL_PEERPID` requires the socket to be `connect()`-ed
    // before it returns a value, which doesn't fit peko's connectionless
    // datagram model (one server socket, many peers). The proper fix
    // uses `SCM_CREDS` ancillary data via `sendmsg`/`recvmsg`, which
    // requires a wider IPC protocol change. Until then, session-group
    // auth is a no-op on macOS — the existing transport-layer trust
    // (Unix socket file mode 0600) still applies.
    Err(io::Error::new(
        io::ErrorKind::Other,
        "peer_credentials: macOS requires SCM_CREDS ancillary data; \
         session-group auth is currently a no-op on macOS",
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn peer_credentials(_fd: std::os::fd::RawFd) -> io::Result<PeerCredentials> {
    Err(io::Error::new(
        io::ErrorKind::Other,
        "peer_credentials not supported on this platform",
    ))
}

/// Return the session ID of the calling process.
///
/// Uses `getsid(0)` which returns the SID of the calling process's
/// session leader. All descendants of a process inherit its SID
/// unless explicitly changed via `setsid()`, so two CLI processes
/// in the same shell return the same value here.
///
/// Used by:
/// - The daemon at startup (preauthorize its own SID for service-token
///   internal IPC).
/// - The CLI before reading its per-SID token file
///   (`auth-token-<sid>`).
///
/// Returns `None` on non-Unix platforms (Windows named-pipe DACL auth
/// is structurally different and does not use session IDs).
#[cfg(unix)]
pub(crate) fn getsid_self() -> Option<i32> {
    let sid = unsafe { libc::getsid(0) };
    if sid < 0 { None } else { Some(sid) }
}

#[cfg(not(unix))]
pub(crate) fn getsid_self() -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::fd::AsRawFd;

    /// Linux: SO_PEERCRED returns the peer's PID after a datagram has
    /// been received from that peer. We exercise the helper with a
    /// client→server send so the kernel records peer credentials.
    #[cfg(target_os = "linux")]
    #[test]
    fn peer_credentials_after_recv() {
        let srv_path =
            std::env::temp_dir().join(format!("peko_peer_srv_{}.sock", std::process::id()));
        let cli_path =
            std::env::temp_dir().join(format!("peko_peer_cli_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&srv_path);
        let _ = std::fs::remove_file(&cli_path);

        let srv = std::os::unix::net::UnixDatagram::bind(&srv_path).expect("bind srv");
        let cli = std::os::unix::net::UnixDatagram::bind(&cli_path).expect("bind cli");
        cli.send_to(b"hello", &srv_path).expect("send");
        let mut buf = [0u8; 16];
        let _ = srv.recv(&mut buf).expect("recv");

        let creds = peer_credentials(srv.as_raw_fd()).expect("peer_credentials");
        assert_eq!(creds.pid, std::process::id() as i32);
        assert!(creds.sid > 0, "sid should be positive, got {}", creds.sid);

        let _ = std::fs::remove_file(&srv_path);
        let _ = std::fs::remove_file(&cli_path);
    }
}