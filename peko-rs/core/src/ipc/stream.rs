//! Packet Stream — Async Iterator Over Sequenced Response Packets
//!
//! Provides an ordered, heartbeat-aware stream of `ResponsePacket`s
//! from the daemon. Handles:
//! - Sequencing: reorders out-of-order packets
//! - Heartbeat: detects dead daemon
//! - Timeout: prevents hanging forever

use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{trace, warn};

use super::connection::ConnectionHandle;
use super::packet::ResponsePacket;
use peko_protocol::ipc::CLI_TIMEOUT_SECS;

/// Stream of response packets for a single request
///
/// Created by `DaemonClient::execute()` and similar methods.
/// Use as an async iterator: `while let Some(packet) = stream.next().await`
pub struct PacketStream {
    request_id: u64,
    rx: mpsc::UnboundedReceiver<ResponsePacket>,
}

impl PacketStream {
    /// Create a new packet stream
    pub(crate) fn new(request_id: u64, rx: mpsc::UnboundedReceiver<ResponsePacket>) -> Self {
        Self { request_id, rx }
    }

    /// Get the request ID
    #[must_use]
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// Receive the next packet
    ///
    /// Returns `None` when the stream is closed (Done/Error received, or daemon died).
    /// Also returns `None` if no packet is received within the CLI timeout period,
    /// to prevent hanging forever on a dead or stuck daemon.
    pub async fn next(&mut self) -> Option<ResponsePacket> {
        match tokio::time::timeout(Duration::from_secs(CLI_TIMEOUT_SECS), self.rx.recv()).await {
            Ok(packet) => packet,
            Err(_) => {
                warn!(
                    "PacketStream timeout for request {}: no packet received within {}s",
                    self.request_id, CLI_TIMEOUT_SECS
                );
                None
            }
        }
    }

    /// Collect all text chunks into a single string
    ///
    /// # Errors
    /// Returns error if the stream ends with an error or the daemon dies
    pub async fn collect_text(mut self) -> anyhow::Result<String> {
        let mut result = String::new();

        while let Some(packet) = self.next().await {
            match packet {
                ResponsePacket::Text { chunk, .. } => result.push_str(&chunk),
                ResponsePacket::Done { success, error, .. } => {
                    if success {
                        return Ok(result);
                    }
                    anyhow::bail!(error.unwrap_or_else(|| "Unknown error".to_string()));
                }
                ResponsePacket::Error { message, .. } => anyhow::bail!(message),
                ResponsePacket::Heartbeat { .. } => {
                    // Ignore heartbeats during collection
                }
                other => {
                    warn!("Unexpected packet in text stream: {}", other.variant_name());
                }
            }
        }

        anyhow::bail!("Stream closed unexpectedly")
    }
}

/// Background task that receives packets from the socket and feeds them
/// into the appropriate `PacketStream` (or creates new ones).
///
/// This runs on a per-connection basis.
pub(crate) struct StreamRouter {
    /// Active streams by request_id
    streams: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<u64, mpsc::UnboundedSender<ResponsePacket>>>,
    >,
}

impl StreamRouter {
    pub fn new() -> Self {
        Self {
            streams: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Register a new stream for a request
    pub async fn register(&self, request_id: u64) -> PacketStream {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut streams = self.streams.lock().await;
        streams.insert(request_id, tx);
        PacketStream::new(request_id, rx)
    }

    /// Route an incoming packet to its stream
    pub async fn route(&self, packet: ResponsePacket) {
        let request_id = packet.request_id();
        let streams = self.streams.lock().await;
        if let Some(tx) = streams.get(&request_id) {
            let _ = tx.send(packet);
        } else {
            warn!("Received packet for unknown request_id: {}", request_id);
        }
    }
}

/// Spawn a background receiver task that reads from the socket and
/// routes packets to the appropriate streams.
///
/// Returns a `StreamRouter` that can be used to register new streams,
/// plus a liveness handle for the receiver task. Long-lived clients
/// (e.g. an in-daemon adapter) should check the handle before sending
/// so a dead receiver fails fast instead of hanging until the
/// per-request timeout.
pub(crate) fn spawn_receiver(
    conn: ConnectionHandle,
) -> (StreamRouter, std::sync::Arc<tokio::task::JoinHandle<()>>) {
    spawn_receiver_with_timeout(conn, Duration::from_secs(CLI_TIMEOUT_SECS))
}

/// Like [`spawn_receiver`] with a custom idle cadence (test seam).
///
/// The idle timeout is NOT a liveness check: an idle datagram
/// connection is perfectly normal, so on timeout the task simply keeps
/// listening. It only exits on a genuine socket error (or panic).
/// (2026-08-07 field test, finding F1: the previous implementation
/// `break`ed on idle timeout, killing the long-lived in-daemon cron
/// adapter's receiver 60s after startup. Sends kept working, so jobs
/// were added but every response was lost — each tool call then hung
/// for the full per-request timeout.)
pub(crate) fn spawn_receiver_with_timeout(
    conn: ConnectionHandle,
    idle: Duration,
) -> (StreamRouter, std::sync::Arc<tokio::task::JoinHandle<()>>) {
    let router = StreamRouter::new();
    let router_clone = StreamRouter {
        streams: router.streams.clone(),
    };

    let handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        loop {
            match tokio::time::timeout(idle, conn.recv(&mut buf)).await {
                Err(_) => {
                    // Idle silence — keep listening.
                    trace!("IPC receiver idle for {idle:?}; still listening");
                    continue;
                }
                Ok(Err(e)) => {
                    // Genuine socket error: the connection is broken.
                    // Fail every pending stream loudly, then exit.
                    warn!("Socket receive error: {e}; IPC receiver exiting");
                    let streams = router_clone.streams.lock().await;
                    for (request_id, tx) in streams.iter() {
                        let _ = tx.send(ResponsePacket::Error {
                            request_id: *request_id,
                            message: format!("Daemon connection lost: {e}"),
                        });
                    }
                    break;
                }
                Ok(Ok(0)) => {
                    continue;
                }
                Ok(Ok(len)) => match ResponsePacket::from_bytes(&buf[..len]) {
                    Ok(packet) => {
                        trace!("Received packet: {:?}", packet);
                        router_clone.route(packet).await;
                    }
                    Err(e) => {
                        warn!("Failed to parse packet: {}", e);
                    }
                },
            }
        }
    });

    (router, std::sync::Arc::new(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stream_router_register_and_route() {
        let router = StreamRouter::new();
        let mut stream = router.register(42).await;

        // Simulate receiving a packet
        router
            .route(ResponsePacket::Text {
                request_id: 42,
                seq: 0,
                chunk: "hello".to_string(),
            })
            .await;

        let packet = stream.next().await.unwrap();
        match packet {
            ResponsePacket::Text { chunk, .. } => assert_eq!(chunk, "hello"),
            _ => panic!("Expected Text packet"),
        }
    }

    #[tokio::test]
    async fn test_stream_router_unknown_request() {
        let router = StreamRouter::new();
        // Route to unknown request_id — should not panic
        router
            .route(ResponsePacket::Text {
                request_id: 999,
                seq: 0,
                chunk: "hello".to_string(),
            })
            .await;
    }

    /// Finding F1 (2026-08-07 field test): the receiver must survive
    /// arbitrary idle silence. The pre-fix loop `break`ed after one
    /// idle timeout, permanently deafening long-lived clients.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_receiver_survives_idle_silence() {
        let dir = tempfile::tempdir().unwrap();
        let client_path = dir.path().join("client.sock");
        let server_path = dir.path().join("server.sock");

        let client_std = std::os::unix::net::UnixDatagram::bind(&client_path).unwrap();
        client_std.set_nonblocking(true).unwrap();
        let server_std = std::os::unix::net::UnixDatagram::bind(&server_path).unwrap();
        server_std.set_nonblocking(true).unwrap();

        let conn = ConnectionHandle::Unix {
            socket: std::sync::Arc::new(
                tokio::net::UnixDatagram::from_std(client_std).unwrap(),
            ),
            path: client_path.clone(),
        };
        let (router, handle) =
            spawn_receiver_with_timeout(conn, Duration::from_millis(50));

        // Idle for ~4x the cadence: the receiver must NOT exit.
        tokio::time::sleep(Duration::from_millis(210)).await;
        assert!(
            !handle.is_finished(),
            "receiver exited after idle silence (F1 regression)"
        );

        // And a late packet must still route to its stream.
        let mut stream = router.register(7).await;
        let server = tokio::net::UnixDatagram::from_std(server_std).unwrap();
        let packet = ResponsePacket::Text {
            request_id: 7,
            seq: 0,
            chunk: "late hello".to_string(),
        };
        server
            .send_to(&packet.to_bytes().unwrap(), &client_path)
            .await
            .unwrap();
        let got = stream.next().await.expect("packet after idle silence");
        match got {
            ResponsePacket::Text { chunk, .. } => assert_eq!(chunk, "late hello"),
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
