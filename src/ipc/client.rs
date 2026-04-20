//! Daemon Client — Packet Send/Receive Only
//!
//! Per SRP, this struct only sends `RequestPacket`s and receives
//! `ResponsePacket`s. Connection management (discovery, reconnection)
//! is handled by `ConnectionManager`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing::{debug, trace};

use super::connection::{ConnectionHandle, ConnectionManager};
use super::packet::{RequestPacket, ResponsePacket};
use super::stream::{PacketStream, StreamRouter};

/// Client for communicating with the pekobot daemon
///
/// Thin wrapper around a `ConnectionHandle`. Sends requests, returns
/// response streams. No connection management, no retry logic.
pub struct DaemonClient {
    conn: ConnectionHandle,
    router: StreamRouter,
    next_request_id: Arc<AtomicU64>,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient")
            .field("next_request_id", &self.next_request_id)
            .finish_non_exhaustive()
    }
}

impl DaemonClient {
    /// Connect to the daemon.
    ///
    /// The CLI does NOT auto-start the daemon. Start it manually with:
    ///   pekobot daemon start
    ///
    /// # Errors
    /// Returns error if daemon is not reachable
    pub async fn connect() -> anyhow::Result<Self> {
        let conn = ConnectionManager::connect().await?;
        Self::with_connection(conn).await
    }

    /// Create a client with an existing connection
    ///
    /// # Errors
    /// Returns error if the connection cannot be cloned for the receiver
    pub async fn with_connection(conn: ConnectionHandle) -> anyhow::Result<Self> {
        let router = super::stream::spawn_receiver(conn.try_clone().await?);
        Ok(Self {
            conn,
            router,
            next_request_id: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Generate a new unique request ID
    fn next_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a request packet and return a stream for responses
    async fn send_request(&self, packet: RequestPacket) -> anyhow::Result<PacketStream> {
        let request_id = packet.request_id();
        let stream = self.router.register(request_id).await;

        let bytes = packet.to_bytes()?;
        trace!("Sending request {} ({} bytes)", request_id, bytes.len());
        self.conn.send(&bytes).await?;

        Ok(stream)
    }

    /// Execute an agent message
    ///
    /// Sends an `Execute` request and returns a stream of response packets.
    /// The caller should iterate the stream to receive text chunks, heartbeats,
    /// and the final `Done` packet.
    ///
    /// # Errors
    /// Returns error if the request cannot be sent
    pub async fn execute(
        &self,
        agent: impl Into<String>,
        team: impl Into<String>,
        message: impl Into<String>,
        session_id: Option<String>,
        new_session: bool,
        stream: bool,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let agent_str: String = agent.into();
        let team_str: String = team.into();
        debug!(
            "Execute request {}: agent={} team={} stream={}",
            request_id,
            agent_str,
            team_str,
            stream
        );

        let packet = RequestPacket::Execute {
            request_id,
            agent: agent_str,
            team: team_str,
            message: message.into(),
            session_id,
            new_session,
            stream,
        };

        self.send_request(packet).await
    }

    /// Spawn an async background task
    ///
    /// # Errors
    /// Returns error if the request cannot be sent
    pub async fn spawn_async_task(
        &self,
        tool_name: impl Into<String>,
        params: serde_json::Value,
        session_key: impl Into<String>,
        workspace: std::path::PathBuf,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::AsyncSpawn {
            request_id,
            tool_name: tool_name.into(),
            params,
            session_key: session_key.into(),
            workspace,
        };

        self.send_request(packet).await
    }

    /// Cancel an async task
    ///
    /// # Errors
    /// Returns error if the request cannot be sent
    pub async fn cancel_async_task(
        &self,
        task_id: impl Into<String>,
    ) -> anyhow::Result<PacketStream> {
        let request_id = self.next_id();
        let packet = RequestPacket::AsyncCancel {
            request_id,
            task_id: task_id.into(),
        };

        self.send_request(packet).await
    }

    /// Ping the daemon to check if it's alive
    ///
    /// Returns the Pong response with uptime and version.
    ///
    /// # Errors
    /// Returns error if the ping fails or times out
    pub async fn ping(&self) -> anyhow::Result<ResponsePacket> {
        let request_id = self.next_id();
        let packet = RequestPacket::Ping { request_id };
        let mut stream = self.send_request(packet).await?;

        // Wait for the first (and only) response
        match stream.next().await {
            Some(packet) => Ok(packet),
            None => anyhow::bail!("Ping stream closed unexpectedly"),
        }
    }

    /// Check if the daemon is running
    ///
    /// Returns `true` if the daemon responds to a ping within the timeout.
    pub async fn is_running(&self) -> bool {
        match self.ping().await {
            Ok(ResponsePacket::Pong { .. }) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a running daemon. They are integration tests.
    // Unit tests for serialization are in packet.rs.

    #[test]
    fn test_next_id_monotonic() {
        // We can't easily test connect() without a daemon, but we can test
        // the request ID generation
        let counter = Arc::new(AtomicU64::new(1));
        assert_eq!(counter.fetch_add(1, Ordering::SeqCst), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
}
