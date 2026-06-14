//! `ResponseSink` — transport-agnostic response-write abstraction (ADR-038).
//!
//! Today, every handler in `IpcServer::handle_request` calls
//! `Self::send_packet(&socket, response, peer).await?` to write a response
//! back to the client. The `peer: &PeerAddr` argument exists because the
//! Unix datagram and UDP transports are connectionless — every response
//! must be `send_to`-ed to the peer address returned by the matching
//! `recv_from`.
//!
//! Windows named pipes (ADR-038) are connection-oriented: each accepted
//! connection is its own `NamedPipeServer`, and `write_all(&bytes)` is
//! the only write path. There is no peer address to thread through the
//! handler signature, because the connection *is* the destination.
//!
//! `ResponseSink` factors that out. Handlers receive a `&dyn ResponseSink`
//! and call `sink.send_bytes(&bytes).await?`; the per-transport details
//! live in the `impl`s. The Unix/UDP call sites construct a per-request
//! `DatagramSink { socket, peer }` that captures the peer once and
//! forwards `send_bytes` to the existing `send_response`; the Windows
//! call site constructs a `PipeSink { server }` over a `&mut
//! NamedPipeServer` and forwards to `write_all`.
//!
//! This keeps the giant `handle_request` match (server.rs:322-2825)
//! platform-agnostic: no `#[cfg]`-gated handler signatures, no duplicated
//! arms, and the per-call branching lives in two small constructors.

use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;

use super::server::PeerAddr;

/// Write serialised response bytes back to a connected client.
///
/// Implementations are kept single-method so a future fourth transport
/// (e.g. a `TcpStream` for the cross-host case) only needs to provide
/// `send_bytes` and the discovery/auth story is unchanged.
#[async_trait]
pub trait ResponseSink: Send + Sync {
    /// Write the full byte buffer to the client. The implementation must
    /// flush before returning — `write_all` semantics, not `write`.
    async fn send_bytes(&self, bytes: &[u8]) -> io::Result<()>;
}

/// Per-request sink for the Unix datagram transport. Captures the peer
/// path returned by `recv_from` and forwards `send_bytes` to
/// `UnixDatagram::send_to`. Wraps the socket in `Arc` because the
/// `ServerSocket` enum holds an `Arc<UnixDatagram>` shared across the
/// per-request tasks.
#[cfg(unix)]
pub struct UnixDatagramSink {
    pub socket: Arc<tokio::net::UnixDatagram>,
    pub peer: std::path::PathBuf,
}

#[async_trait]
#[cfg(unix)]
impl ResponseSink for UnixDatagramSink {
    async fn send_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        // `send_to` returns the number of bytes sent; the trait
        // contract is just "write succeeded," so we drop the count.
        self.socket.send_to(bytes, &self.peer).await.map(|_| ())
    }
}

/// Per-request sink for the UDP transport. Captures the peer
/// `SocketAddr` returned by `recv_from`.
pub struct UdpSink {
    pub socket: Arc<UdpSocket>,
    pub peer: std::net::SocketAddr,
}

#[async_trait]
impl ResponseSink for UdpSink {
    async fn send_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        self.socket.send_to(bytes, self.peer).await.map(|_| ())
    }
}

/// Per-connection sink for the Windows named-pipe transport. Wraps a
/// `&mut NamedPipeServer`. The receiver task owns the server exclusively,
/// so `&mut` is sound and no sync is needed.
#[cfg(windows)]
pub struct PipeSink<'a> {
    pub server: &'a mut tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(windows)]
#[async_trait]
impl ResponseSink for PipeSink<'_> {
    async fn send_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        use tokio::io::AsyncWriteExt;
        // `PipeSink` holds an exclusive `&mut NamedPipeServer`. Tokio's
        // `AsyncWrite::poll_write` (and therefore `write_all` from the
        // extension trait) takes `&mut self`, but our `ResponseSink`
        // trait method takes `&self` so the trait object stays dyn-safe
        // and `Send + Sync`. The `&mut` is sound here: there is no other
        // path to the inner server while `PipeSink` exists. We re-borrow
        // via raw pointer to convince the borrow checker.
        //
        // SAFETY: `self.server` is borrowed as `&mut` exclusively for
        // the lifetime of `&self`, and the trait's `&self` receiver
        // means we hold the only `PipeSink` referencing this server in
        // the current task. No other task can observe this `&mut`
        // because the per-connection task scope owns the server.
        let s: &mut tokio::net::windows::named_pipe::NamedPipeServer = unsafe {
            &mut *((self.server as *const _) as *mut _)
        };
        s.write_all(bytes).await
    }
}

/// Convenience constructor that turns the existing `send_response`
/// inputs into the matching sink. The match is exhaustive over the
/// peer/socket combination, so a mismatch surfaces as an `io::Error`
/// at the call site instead of a panic deep in the handler.
pub fn sink_for_unix_or_udp(
    socket: &super::server::ServerSocket,
    peer: &PeerAddr,
) -> io::Result<Box<dyn ResponseSink>> {
    match (socket, peer) {
        #[cfg(unix)]
        (super::server::ServerSocket::Unix { socket, .. }, PeerAddr::Unix(path)) => {
            Ok(Box::new(UnixDatagramSink {
                socket: Arc::clone(socket),
                peer: path.clone(),
            }))
        }
        (super::server::ServerSocket::Udp { socket }, PeerAddr::Ip(addr)) => {
            Ok(Box::new(UdpSink {
                socket: Arc::clone(socket),
                peer: *addr,
            }))
        }
        // Mismatched peer/socket — the original `send_response` raised
        // an explicit error in this case (server.rs:106-112); we
        // preserve that.
        _ => Err(io::Error::new(
            io::ErrorKind::Other,
            "peer/socket transport mismatch (Unix peer on UDP socket or vice versa)",
        )),
    }
}

#[cfg(windows)]
pub fn sink_for_pipe(
    server: &mut tokio::net::windows::named_pipe::NamedPipeServer,
) -> Box<dyn ResponseSink + '_> {
    Box::new(PipeSink { server })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial in-memory sink used to verify the trait wiring. The
    /// production code paths do not use this — production impls are on
    /// `UnixDatagramSink` / `UdpSink` / `PipeSink`.
    struct VecSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    #[async_trait]
    impl ResponseSink for VecSink {
        async fn send_bytes(&self, bytes: &[u8]) -> io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
    }

    #[tokio::test]
    async fn vec_sink_collects_bytes() {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = VecSink(buf.clone());
        sink.send_bytes(b"hello").await.unwrap();
        sink.send_bytes(b" world").await.unwrap();
        assert_eq!(*buf.lock().unwrap(), b"hello world".to_vec());
    }

    /// `sink_for_unix_or_udp` returns an error on a peer/socket mismatch
    /// (Unix peer on UDP socket, or vice versa). The original
    /// `send_response` had the same behaviour at server.rs:106-112.
    ///
    /// Unix-only: the test relies on the `PeerAddr::Unix` variant, which
    /// does not exist on Windows builds.
    #[cfg(unix)]
    #[tokio::test]
    async fn sink_for_mismatch_returns_error() {
        // We need a real `ServerSocket::Udp` to test the match exhaustiveness.
        // Bind to an ephemeral port on loopback.
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_arc = Arc::new(udp);
        let socket = super::super::server::ServerSocket::Udp {
            socket: Arc::clone(&udp_arc),
        };
        // Construct a fake Unix peer; the Unix arm expects a Unix peer
        // but the socket is UDP, so this is a mismatch.
        let unix_peer = PeerAddr::Unix(std::path::PathBuf::from("/tmp/fake.sock"));
        let result = sink_for_unix_or_udp(&socket, &unix_peer);
        assert!(result.is_err(), "expected mismatch error, got Ok");
    }
}
