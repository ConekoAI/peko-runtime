//! Tunnel Client — WebSocket Connection to PekoHub
//!
//! Manages the outbound WebSocket tunnel: connection, authentication,
//! heartbeat, request dispatch, and automatic reconnection with backoff.

use std::pin::Pin;
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
use crate::common::vault::Vault;

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
#[derive(Clone, Debug)]
pub struct TunnelHandle {
    tx: mpsc::UnboundedSender<TunnelMessage>,
}

impl TunnelHandle {
    /// Create a new handle from a sender (test-only).
    #[cfg(test)]
    pub fn new(tx: mpsc::UnboundedSender<TunnelMessage>) -> Self {
        Self { tx }
    }

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

/// Default maximum reconnect attempts before giving up and signalling degraded state.
/// With default exponential backoff (1/2/4/.../60s), 50 attempts ≈ 28 minutes of retries.
pub const DEFAULT_MAX_RECONNECT_ATTEMPTS: u32 = 50;

/// Health/status update emitted by the tunnel client after each connection
/// attempt. The owner (`AppState`) consumes these to keep its view of
/// tunnel health in sync, and ultimately to surface `peko daemon status`.
///
/// Issue #8: prior to this, the tunnel client had no way to tell the rest
/// of the daemon that it had failed repeatedly, so `deamon status` could
/// always report "connected" even after PekoHub went away.
#[derive(Debug, Clone)]
pub enum TunnelStatusUpdate {
    /// A connection was just established successfully.
    Connected,
    /// A connection attempt just failed; the client will keep retrying.
    /// `attempts` is the running count of consecutive failures.
    /// `last_error` is the error string from the latest attempt.
    Disconnected { attempts: u32, last_error: String },
    /// The reconnect-attempt cap was hit. The client has stopped retrying.
    /// `attempts` equals the cap; `last_error` is the final error.
    /// After this, `peko daemon status --json` reports `tunnel.state == "degraded"`.
    Degraded { attempts: u32, last_error: String },
}

/// Tunnel client that maintains a persistent WebSocket connection to PekoHub.
pub struct TunnelClient {
    hub_url: String,
    credential: PekoHubCredential,
    vault: Option<Arc<Vault>>,
    backoff: ExponentialBackoff,
    state: Arc<RwLock<TunnelState>>,
    /// Maximum number of consecutive reconnect attempts before giving up
    /// (issue #8: avoids infinite retry loop when PekoHub is permanently down).
    max_reconnect_attempts: u32,
    /// Optional callback for handling proxied requests
    request_handler: Option<
        Arc<
            dyn Fn(
                    TunnelMessage,
                    TunnelHandle,
                ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                + Send
                + Sync,
        >,
    >,
    /// Optional callback for receiving per-iteration status updates
    /// (`Connected` / `Disconnected` / `Degraded`). Used by `AppState::start_tunnel`
    /// to keep the daemon's view of tunnel health in sync and to mark the
    /// daemon as degraded when the reconnect cap is hit (issue #8).
    on_status: Option<
        Arc<
            dyn Fn(TunnelStatusUpdate) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                + Send
                + Sync,
        >,
    >,
}

impl TunnelClient {
    /// Create a new tunnel client with the default reconnect cap.
    #[must_use]
    pub fn new(credential: PekoHubCredential) -> Self {
        Self::new_with_options(credential, DEFAULT_MAX_RECONNECT_ATTEMPTS)
    }

    /// Create a new tunnel client with a custom reconnect-attempt cap.
    #[must_use]
    pub fn new_with(credential: PekoHubCredential, max_reconnect_attempts: u32) -> Self {
        Self::new_with_options(credential, max_reconnect_attempts)
    }

    /// Attach an explicit vault for resolving the tunnel private key.
    ///
    /// When unset, the client loads the vault from the default config directory.
    pub fn with_vault(mut self, vault: Arc<Vault>) -> Self {
        self.vault = Some(vault);
        self
    }

    fn new_with_options(credential: PekoHubCredential, max_reconnect_attempts: u32) -> Self {
        let hub_url = credential.url.clone();
        Self {
            hub_url,
            credential,
            vault: None,
            backoff: ExponentialBackoff::default_tunnel(),
            state: Arc::new(RwLock::new(TunnelState {
                heartbeat_seq: 0,
                last_ack_seq: 0,
                missed_heartbeats: 0,
                ready: false,
                heartbeat_interval_secs: 30,
            })),
            max_reconnect_attempts,
            request_handler: None,
            on_status: None,
        }
    }

    /// Set a callback for handling proxied requests
    pub fn on_request<F, Fut>(&mut self, handler: F)
    where
        F: Fn(TunnelMessage, TunnelHandle) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.request_handler = Some(Arc::new(move |msg, handle| Box::pin(handler(msg, handle))));
    }

    /// Set a callback for receiving per-iteration status updates
    /// (connected / disconnected / degraded). Used by `AppState` to keep
    /// the daemon's view of tunnel health in sync, and to mark the daemon
    /// as degraded once the reconnect-attempt cap is hit (issue #8).
    pub fn on_status<F, Fut>(&mut self, handler: F)
    where
        F: Fn(TunnelStatusUpdate) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.on_status = Some(Arc::new(move |update| Box::pin(handler(update))));
    }

    /// Get the configured reconnect-attempt cap.
    #[must_use]
    pub fn max_reconnect_attempts(&self) -> u32 {
        self.max_reconnect_attempts
    }

    /// Run the tunnel client loop until the reconnect-attempt cap is hit
    /// (or forever, if `max_reconnect_attempts` is `u32::MAX`).
    ///
    /// Issue #8: previously this looped unbounded, producing infinite log
    /// spam and no operator signal when PekoHub was permanently down.
    /// Now each consecutive failure is counted; once the cap is reached,
    /// the `on_degraded` callback fires once with the attempt count and
    /// last error, and `run()` returns.
    pub async fn run(mut self) {
        let mut consecutive_failures: u32 = 0;
        loop {
            match self.connect_and_serve().await {
                Ok(()) => {
                    self.backoff.reset();
                    consecutive_failures = 0;
                    info!("Tunnel connection closed gracefully");
                    if let Some(cb) = self.on_status.as_ref() {
                        cb(TunnelStatusUpdate::Connected).await;
                    }
                }
                Err(e) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    let err_msg = e.to_string();
                    if consecutive_failures >= self.max_reconnect_attempts {
                        error!(
                            "tunnel reconnect cap reached after {} attempts (last error: {}); \
                             entering degraded state",
                            consecutive_failures, err_msg
                        );
                        if let Some(cb) = self.on_status.as_ref() {
                            cb(TunnelStatusUpdate::Degraded {
                                attempts: consecutive_failures,
                                last_error: err_msg.clone(),
                            })
                            .await;
                        }
                        return;
                    }
                    let delay = self.backoff.next();
                    warn!(
                        "tunnel disconnected (attempt {}/{}): {}, retrying in {:?}",
                        consecutive_failures, self.max_reconnect_attempts, err_msg, delay
                    );
                    if let Some(cb) = self.on_status.as_ref() {
                        cb(TunnelStatusUpdate::Disconnected {
                            attempts: consecutive_failures,
                            last_error: err_msg.clone(),
                        })
                        .await;
                    }
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Run the tunnel client with a cancellation token
    pub async fn run_cancellable(self, cancel: tokio_util::sync::CancellationToken) {
        tokio::select! {
            () = self.run() => {},
            () = cancel.cancelled() => {
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

        // 2. Wait for TunnelChallenge. PekoHub (issue #1) issues a
        //    server-generated nonce after verifying the hello
        //    signature + allowlist membership; we sign it and reply.
        let challenge_msg = timeout(Duration::from_secs(10), read.next())
            .await
            .map_err(|_| {
                TunnelError::AuthFailed("Timeout waiting for TunnelChallenge".to_string())
            })?
            .ok_or(TunnelError::Closed)?
            .map_err(TunnelError::WebSocket)?;

        let challenge_nonce = match decode_tunnel_message(challenge_msg)? {
            TunnelMessage::TunnelChallenge { nonce } => {
                debug!("Received TunnelChallenge");
                nonce
            }
            TunnelMessage::Disconnect { reason } => {
                return Err(TunnelError::AuthFailed(format!(
                    "PekoHub rejected connection: {reason}"
                )));
            }
            other => {
                return Err(TunnelError::AuthFailed(format!(
                    "Expected TunnelChallenge, got: {:?}",
                    other
                )));
            }
        };

        // 3. Sign the challenge nonce and send the ack. We use the
        //    base64url nonce string as the signed payload — exactly
        //    what the server verifies (server's verifyDidKeySignature
        //    is text-mode over the same bytes).
        let challenge_signature = self.sign_challenge(&challenge_nonce)?;
        let ack = TunnelMessage::TunnelChallengeAck {
            nonce: challenge_nonce,
            signature: challenge_signature,
        };
        let ack_bytes = ack
            .to_bytes()
            .map_err(|e| TunnelError::AuthFailed(format!("Failed to serialize ack: {e}")))?;
        write
            .send(Message::Binary(ack_bytes))
            .await
            .map_err(TunnelError::WebSocket)?;
        debug!("Sent TunnelChallengeAck");

        // 4. Wait for TunnelReady
        let ready_msg = timeout(Duration::from_secs(10), read.next())
            .await
            .map_err(|_| TunnelError::AuthFailed("Timeout waiting for TunnelReady".to_string()))?
            .ok_or(TunnelError::Closed)?
            .map_err(TunnelError::WebSocket)?;

        let ready = decode_tunnel_message(ready_msg)?;

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
            )
            .await;
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
        request_handler: Option<
            &Arc<
                dyn Fn(
                        TunnelMessage,
                        TunnelHandle,
                    ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                    + Send
                    + Sync,
            >,
        >,
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
            | TunnelMessage::ExposureUpdate { .. }
            | TunnelMessage::StatusUpdate { .. }
            // Issue #29 (Slice A): cross-runtime a2a envelopes flow
            // through the same handler seam as proxied requests.
            // Slice C lands the actual `AgentToAgentRequest`
            // dispatcher branch (signature verify → session attribute
            // → local dispatch → `AgentToAgentResponse`); Slice B
            // lands the `AgentToAgentResponse` correlation on the
            // caller side. Until then the dispatcher will log a
            // `debug!` and drop, which is the safe default — no
            // surprise local dispatch, no silent send loop.
            | TunnelMessage::AgentToAgentRequest { .. }
            | TunnelMessage::AgentToAgentResponse { .. } => {
                if let Some(handler) = request_handler {
                    handler(msg, handle.clone()).await;
                } else {
                    debug!("No request handler registered, dropping message");
                }
            }
            TunnelMessage::RuntimeHello { .. }
            | TunnelMessage::TunnelChallenge { .. }
            | TunnelMessage::TunnelChallengeAck { .. } => {
                warn!("Unexpected control message received after auth");
            }
            TunnelMessage::TunnelReady { .. } => {
                // Forward TunnelReady to the handler so dispatcher can announce instances
                if let Some(handler) = request_handler {
                    handler(msg, handle.clone()).await;
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
        let nonce = BASE64.encode(nonce_bytes);

        let private_key_b64 = self
            .resolve_private_key()
            .map_err(|e| TunnelError::AuthFailed(format!("Failed to resolve private key: {e}")))?;

        // Decode private key
        let private_key_bytes = BASE64
            .decode(&private_key_b64)
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

    /// Sign a server-issued challenge nonce and return the base64
    /// signature. The nonce is signed as its UTF-8 byte string,
    /// matching how the server verifies it (text-mode
    /// `verifyDidKeySignature(did, nonce, signature)`).
    fn sign_challenge(&self, nonce: &str) -> Result<String, TunnelError> {
        let private_key_b64 = self
            .resolve_private_key()
            .map_err(|e| TunnelError::AuthFailed(format!("Failed to resolve private key: {e}")))?;

        let private_key_bytes = BASE64
            .decode(&private_key_b64)
            .map_err(|e| TunnelError::AuthFailed(format!("Invalid private key: {e}")))?;
        if private_key_bytes.len() != 32 {
            return Err(TunnelError::AuthFailed(
                "Private key must be 32 bytes".to_string(),
            ));
        }
        let mut sk_array = [0u8; 32];
        sk_array.copy_from_slice(&private_key_bytes);
        let signing_key = SigningKey::from_bytes(&sk_array);

        let signature = signing_key.sign(nonce.as_bytes());
        Ok(BASE64.encode(signature.to_bytes()))
    }

    /// Check if the tunnel is currently connected and authenticated
    pub async fn is_ready(&self) -> bool {
        self.state.read().await.ready
    }

    /// Resolve the tunnel private key, using the explicit vault if attached.
    fn resolve_private_key(&self) -> Result<String, anyhow::Error> {
        match &self.vault {
            Some(vault) => self.credential.resolve_private_key(vault),
            None => self.credential.resolve_private_key_default(),
        }
    }
}

/// Spawn a tunnel client in the background and return a handle
pub async fn spawn_tunnel<F, Fut>(credential: PekoHubCredential, request_handler: F) -> TunnelHandle
where
    F: Fn(TunnelMessage, TunnelHandle) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut client = TunnelClient::new(credential);
    client.on_request(request_handler);

    let (tx, _rx) = mpsc::unbounded_channel();
    let handle = TunnelHandle { tx };

    tokio::spawn(client.run());

    handle
}

/// Decode a single WebSocket message into a `TunnelMessage`.
///
/// The tunnel carries messages as either `Binary` (JSON bytes) or
/// `Text` frames; both are accepted for forward-compatibility.
fn decode_tunnel_message(msg: Message) -> Result<TunnelMessage, TunnelError> {
    match msg {
        Message::Binary(bytes) => TunnelMessage::from_bytes(&bytes)
            .map_err(|e| TunnelError::InvalidMessage(e.to_string())),
        Message::Text(text) => TunnelMessage::from_bytes(text.as_bytes())
            .map_err(|e| TunnelError::InvalidMessage(e.to_string())),
        Message::Close(frame) => Err(TunnelError::AuthFailed(format!(
            "Connection closed during auth: {:?}",
            frame
        ))),
        other => Err(TunnelError::AuthFailed(format!(
            "Unexpected message type during auth: {:?}",
            other
        ))),
    }
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
        };
        let client = TunnelClient::new(cred);
        assert!(!client.is_ready().await);
        assert_eq!(
            client.max_reconnect_attempts(),
            DEFAULT_MAX_RECONNECT_ATTEMPTS
        );
    }

    /// Issue #8: when the tunnel cannot reach PekoHub, `run()` must stop
    /// retrying after `max_reconnect_attempts` failures and emit a
    /// `Degraded` status update (not loop forever spamming logs).
    #[tokio::test]
    async fn test_run_caps_reconnect_attempts_and_emits_degraded() {
        // Point at a closed port so every connect attempt fails fast.
        // We don't need to wait for the full backoff window because
        // `run()` returns immediately once the cap is hit (no final sleep).
        let cred = PekoHubCredential {
            url: "ws://127.0.0.1:1/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
        };
        let mut client = TunnelClient::new_with(cred, 2);

        let captured: std::sync::Arc<tokio::sync::Mutex<Vec<TunnelStatusUpdate>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let captured_for_cb = captured.clone();
        client.on_status(move |update| {
            let captured = captured_for_cb.clone();
            async move {
                captured.lock().await.push(update);
            }
        });

        // Bound the test runtime in case of regression.
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), client.run()).await;
        assert!(
            result.is_ok(),
            "run() did not return within 10s — cap not enforced"
        );

        let updates = captured.lock().await.clone();
        // First failure: Disconnected{attempts:1}. Second failure hits the
        // cap and emits Degraded{attempts:2}.
        assert!(
            updates
                .iter()
                .any(|u| matches!(u, TunnelStatusUpdate::Degraded { attempts: 2, .. })),
            "expected Degraded{{attempts:2,..}} in {:?}",
            updates
        );
        assert!(
            updates
                .iter()
                .any(|u| matches!(u, TunnelStatusUpdate::Disconnected { attempts: 1, .. })),
            "expected at least one Disconnected{{attempts:1,..}} in {:?}",
            updates
        );
    }

    /// Issue #8: `new_with` honors a small cap so an integration test
    /// can verify degraded surfacing end-to-end without 28-minute waits.
    #[tokio::test]
    async fn test_new_with_custom_cap() {
        let cred = PekoHubCredential {
            url: "wss://example.com/v1/tunnel".to_string(),
            runtime_id: "did:key:z6MkTest".to_string(),
        };
        let client = TunnelClient::new_with(cred, 7);
        assert_eq!(client.max_reconnect_attempts(), 7);
    }
}
