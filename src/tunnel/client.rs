//! Tunnel Client — WebSocket Connection to PekoHub
//!
//! Manages the outbound WebSocket tunnel: connection, authentication,
//! heartbeat, request dispatch, and automatic reconnection with backoff.

use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use futures::{SinkExt, StreamExt};
use rand::RngCore;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{interval, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, trace, warn};

use super::backoff::ExponentialBackoff;
use super::credential::PekoHubCredential;
use super::protocol::TunnelMessage;

/// Errors that can occur in the tunnel client
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Connection closed unexpectedly")]
    Closed,
    #[error("Authentication failed: {0}")]
    AuthFailed(String),
    #[error("Invalid message received: {0}")]
    InvalidMessage(String),
    #[error("Heartbeat timeout")]
    HeartbeatTimeout,
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Handle to an active tunnel connection, used to send responses back to PekoHub.
#[derive(Clone)]
pub struct TunnelHandle {
    tx: mpsc::UnboundedSender<TunnelMessage>,
}

impl TunnelHandle {
    /// Send a message through the tunnel
    pub fn send(&self, msg: TunnelMessage) -> anyhow::Result<()> {
        self.tx
            .send(msg)
            .map_err(|_| anyhow::anyhow!("Tunnel channel closed"))
    }

    /// Send a proxied response back to PekoHub
    pub fn send_response(&self, request_id: String, payload: Vec<u8>) -> anyhow::Result<()> {
        self.send(TunnelMessage::ProxiedResponse {
            request_id,
            payload,
        })
    }

    /// Send a streaming chunk back to PekoHub
    pub fn send_stream_chunk(
        &self,
        request_id: String,
        seq: u32,
        payload: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.send(TunnelMessage::StreamChunk {
            request_id,
            seq,
            payload,
        })
    }

    /// Send a stream end marker back to PekoHub
    pub fn send_stream_end(&self, request_id: String) -> anyhow::Result<()> {
        self.send(TunnelMessage::StreamEnd { request_id })
    }
}

/// Shared state for the tunnel client
struct TunnelState {
    /// Last heartbeat sequence number sent
    heartbeat_seq: u64,
    /// Last heartbeat sequence acknowledged
    last_ack_seq: u64,
    /// Missed heartbeat count
    missed_heartbeats: u32,
    /// Whether the tunnel is authenticated and ready
    ready: bool,
    /// Heartbeat interval from server
    heartbeat_interval_secs: u32,
}

/// Tunnel client that maintains a persistent WebSocket connection to PekoHub.
pub struct TunnelClient {
    hub_url: String,
    credential: PekoHubCredential,
    backoff: ExponentialBackoff,
    state: Arc<RwLock<TunnelState>>,
    /// Optional callback for handling proxied requests
    request_handler: Option<Arc<dyn Fn(TunnelMessage, TunnelHandle) + Send + Sync>>,
}

impl TunnelClient {
    /// Create a new tunnel client
    #[must_use]
    pub fn new(credential: PekoHubCredential) -> Self {
        let hub_url = credential.url.clone();
        Self {
            hub_url,
            credential,
            backoff: ExponentialBackoff::default_tunnel(),
            state: Arc::new(RwLock::new(TunnelState {
                heartbeat_seq: 0,
                last_ack_seq: 0,
                missed_heartbeats: 0,
                ready: false,
                heartbeat_interval_secs: 30,
            })),
            request_handler: None,
        }
    }

    /// Set a callback for handling proxied requests
    pub fn on_request<F>(&mut self, handler: F)
    where
        F: Fn(TunnelMessage, TunnelHandle) + Send + Sync + 'static,
    {
        self.request_handler = Some(Arc::new(handler));
    }

    /// Run the tunnel client loop (reconnects forever until cancelled)
    pub async fn run(mut self) {
        loop {
            match self.connect_and_serve().await {
                Ok(()) => {
                    self.backoff.reset();
                    info!("Tunnel connection closed gracefully");
                }
                Err(e) => {
                    let delay = self.backoff.next();
                    warn!("tunnel disconnected: {}, retrying in {:?}", e, delay);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Run the tunnel client with a cancellation token
    pub async fn run_cancellable(self, cancel: tokio_util::sync::CancellationToken) {
        tokio::select! {
            _ = self.run() => {},
            _ = cancel.cancelled() => {
                info!("Tunnel client cancelled");
            }
        }
    }

    /// Attempt a single connection and serve until disconnect
    async fn connect_and_serve(&mut self) -> Result<(), TunnelError> {
        info!("Connecting to PekoHub tunnel: {}", self.hub_url);

        let (ws_stream, response) = connect_async(&self.hub_url).await?;
        info!("WebSocket connected, status: {:?}", response.status());

        let (mut write, mut read) = ws_stream.split();

        // 1. Send RuntimeHello
        let hello = self.build_runtime_hello()?;
        let hello_bytes = hello
            .to_bytes()
            .map_err(|e| TunnelError::AuthFailed(format!("Failed to serialize hello: {e}")))?;
        write
            .send(Message::Binary(hello_bytes))
            .await
            .map_err(TunnelError::WebSocket)?;
        debug!("Sent RuntimeHello");

        // 2. Wait for TunnelReady
        let ready_msg = timeout(Duration::from_secs(10), read.next())
            .await
            .map_err(|_| TunnelError::AuthFailed("Timeout waiting for TunnelReady".to_string()))?
            .ok_or(TunnelError::Closed)?
            .map_err(TunnelError::WebSocket)?;

        let ready = match ready_msg {
            Message::Binary(bytes) => TunnelMessage::from_bytes(&bytes)
                .map_err(|e| TunnelError::InvalidMessage(e.to_string()))?,
            Message::Text(text) => TunnelMessage::from_bytes(text.as_bytes())
                .map_err(|e| TunnelError::InvalidMessage(e.to_string()))?,
            Message::Close(frame) => {
                return Err(TunnelError::AuthFailed(format!(
                    "Connection closed before auth: {:?}",
                    frame
                )));
            }
            _ => {
                return Err(TunnelError::AuthFailed(
                    "Unexpected message type during auth".to_string(),
                ));
            }
        };

        let heartbeat_interval = match ready {
            TunnelMessage::TunnelReady {
                heartbeat_interval_secs,
            } => {
                info!(
                    "Tunnel authenticated, heartbeat interval: {}s",
                    heartbeat_interval_secs
                );
                {
                    let mut state = self.state.write().await;
                    state.ready = true;
                    state.heartbeat_interval_secs = heartbeat_interval_secs;
                }
                heartbeat_interval_secs
            }
            TunnelMessage::Disconnect { reason } => {
                return Err(TunnelError::AuthFailed(format!(
                    "PekoHub rejected connection: {reason}"
                )));
            }
            other => {
                return Err(TunnelError::AuthFailed(format!(
                    "Expected TunnelReady, got: {:?}",
                    other
                )));
            }
        };

        // 3. Forward TunnelReady to handler so dispatcher can announce instances
        let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<TunnelMessage>();
        let handle = TunnelHandle {
            tx: internal_tx.clone(),
        };

        if let Some(handler) = &self.request_handler {
            handler(
                TunnelMessage::TunnelReady {
                    heartbeat_interval_secs: heartbeat_interval,
                },
                handle.clone(),
            );
        }

        // 4. Start heartbeat + read loops
        let state = self.state.clone();
        let request_handler = self.request_handler.clone();

        // Heartbeat loop
        let heartbeat_tx = internal_tx.clone();
        let heartbeat_state = state.clone();
        let heartbeat_handle = tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(heartbeat_interval as u64));
            loop {
                tick.tick().await;
                let seq = {
                    let mut s = heartbeat_state.write().await;
                    s.heartbeat_seq += 1;
                    s.heartbeat_seq
                };
                let msg = TunnelMessage::Heartbeat { seq };
                if heartbeat_tx.send(msg).is_err() {
                    break;
                }
                trace!("Sent heartbeat seq={}", seq);
            }
        });

        // Read loop: process incoming WebSocket messages
        let read_state = state.clone();
        let read_handle = tokio::spawn(async move {
            loop {
                match read.next().await {
                    Some(Ok(Message::Binary(bytes))) => match TunnelMessage::from_bytes(&bytes) {
                        Ok(msg) => {
                            Self::handle_incoming_message(
                                msg,
                                &read_state,
                                &internal_tx,
                                request_handler.as_ref(),
                                &handle,
                            )
                            .await;
                        }
                        Err(e) => {
                            warn!("Failed to parse tunnel message: {}", e);
                        }
                    },
                    Some(Ok(Message::Text(text))) => {
                        match TunnelMessage::from_bytes(text.as_bytes()) {
                            Ok(msg) => {
                                Self::handle_incoming_message(
                                    msg,
                                    &read_state,
                                    &internal_tx,
                                    request_handler.as_ref(),
                                    &handle,
                                )
                                .await;
                            }
                            Err(e) => {
                                warn!("Failed to parse tunnel message: {}", e);
                            }
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        info!("WebSocket closed: {:?}", frame);
                        break;
                    }
                    Some(Ok(Message::Ping(_data))) => {
                        trace!("Received WebSocket ping");
                        let _ = internal_tx.send(TunnelMessage::HeartbeatAck { seq: 0 });
                    }
                    Some(Ok(Message::Pong(_))) => {
                        trace!("Received WebSocket pong");
                    }
                    Some(Ok(Message::Frame(_))) => {}
                    Some(Err(e)) => {
                        error!("WebSocket read error: {}", e);
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended");
                        break;
                    }
                }
            }
        });

        // Write loop: send messages from internal channel to WebSocket
        let write_handle = tokio::spawn(async move {
            while let Some(msg) = internal_rx.recv().await {
                let bytes = match msg.to_bytes() {
                    Ok(b) => b,
                    Err(e) => {
                        error!("Failed to serialize tunnel message: {}", e);
                        continue;
                    }
                };
                if let Err(e) = write.send(Message::Binary(bytes)).await {
                    error!("WebSocket write error: {}", e);
                    break;
                }
            }
        });

        // Wait for any task to finish (indicates disconnect)
        tokio::select! {
            _ = heartbeat_handle => {},
            _ = read_handle => {},
            _ = write_handle => {},
        }

        {
            let mut s = state.write().await;
            s.ready = false;
        }

        info!("Tunnel connection closed");
        Ok(())
    }

    /// Handle an incoming tunnel message
    async fn handle_incoming_message(
        msg: TunnelMessage,
        state: &Arc<RwLock<TunnelState>>,
        _internal_tx: &mpsc::UnboundedSender<TunnelMessage>,
        request_handler: Option<&Arc<dyn Fn(TunnelMessage, TunnelHandle) + Send + Sync>>,
        handle: &TunnelHandle,
    ) {
        match msg {
            TunnelMessage::HeartbeatAck { seq } => {
                trace!("Received heartbeat ack seq={}", seq);
                let mut s = state.write().await;
                s.last_ack_seq = seq;
                s.missed_heartbeats = 0;
            }
            TunnelMessage::Heartbeat { seq } => {
                trace!("Received heartbeat ping seq={}", seq);
                // Echo back as ack
                let _ = handle.send(TunnelMessage::HeartbeatAck { seq });
            }
            TunnelMessage::Disconnect { reason } => {
                info!("Received disconnect: {}", reason);
            }
            TunnelMessage::ProxiedRequest { .. }
            | TunnelMessage::ProxiedResponse { .. }
            | TunnelMessage::StreamChunk { .. }
            | TunnelMessage::StreamEnd { .. }
            | TunnelMessage::InstanceAnnounce { .. }
            | TunnelMessage::InstanceHeartbeat { .. }
            | TunnelMessage::InstanceDeregister { .. }
            | TunnelMessage::ExposureUpdate { .. } => {
                if let Some(handler) = request_handler {
                    handler(msg, handle.clone());
                } else {
                    debug!("No request handler registered, dropping message");
                }
            }
            TunnelMessage::RuntimeHello { .. } => {
                warn!("Unexpected control message received after auth");
            }
            TunnelMessage::TunnelReady { .. } => {
                // Forward TunnelReady to the handler so dispatcher can announce instances
                if let Some(handler) = request_handler {
                    handler(msg, handle.clone());
                } else {
                    debug!("No request handler registered, dropping TunnelReady");
                }
            }
        }
    }

    /// Build the RuntimeHello message with signed nonce
    fn build_runtime_hello(&self) -> Result<TunnelMessage, TunnelError> {
        let mut nonce_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = BASE64.encode(&nonce_bytes);

        // Decode private key
        let private_key_bytes = BASE64
            .decode(&self.credential.private_key)
            .map_err(|e| TunnelError::AuthFailed(format!("Invalid private key: {e}")))?;
        if private_key_bytes.len() != 32 {
            return Err(TunnelError::AuthFailed(
                "Private key must be 32 bytes".to_string(),
            ));
        }
        let mut sk_array = [0u8; 32];
        sk_array.copy_from_slice(&private_key_bytes);
        let signing_key = SigningKey::from_bytes(&sk_array);

        // Sign nonce
        let signature = signing_key.sign(nonce.as_bytes());
        let signature_b64 = BASE64.encode(signature.to_bytes());

        Ok(TunnelMessage::RuntimeHello {
            runtime_id: self.credential.runtime_id.clone(),
            nonce,
            signature: signature_b64,
        })
    }

    /// Check if the tunnel is currently connected and authenticated
    pub async fn is_ready(&self) -> bool {
        self.state.read().await.ready
    }
}

/// Spawn a tunnel client in the background and return a handle
pub async fn spawn_tunnel(
    credential: PekoHubCredential,
    request_handler: impl Fn(TunnelMessage, TunnelHandle) + Send + Sync + 'static,
) -> TunnelHandle {
    let mut client = TunnelClient::new(credential);
    client.on_request(request_handler);

    let (tx, _rx) = mpsc::unbounded_channel();
    let handle = TunnelHandle { tx };

    tokio::spawn(client.run());

    handle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tunnel_handle_send() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = TunnelHandle { tx };

        handle.send(TunnelMessage::Heartbeat { seq: 1 }).unwrap();

        match rx.recv().await {
            Some(TunnelMessage::Heartbeat { seq }) => assert_eq!(seq, 1),
            _ => panic!("Expected heartbeat message"),
        }
    }

    #[tokio::test]
    async fn test_tunnel_client_creation() {
        let cred = PekoHubCredential {
            url: "wss://example.com/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
            private_key: BASE64.encode(&[0u8; 32]),
        };
        let client = TunnelClient::new(cred);
        assert!(!client.is_ready().await);
    }
}
