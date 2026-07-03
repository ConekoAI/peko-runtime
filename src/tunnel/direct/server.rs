//! Direct connection server — accepts inbound connections from peer runtimes.
//!
//! The server binds a TCP port (optionally TLS-wrapped) and runs the
//! identity handshake for every accepted connection. Once authenticated,
//! the connection is handed to a dispatcher callback as a `TunnelHandle`.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{
    accept_async_with_config, tungstenite::protocol::WebSocketConfig, tungstenite::Message,
    WebSocketStream,
};

use crate::common::types::config::DirectNetworkConfig;
use crate::tunnel::direct::handshake::{
    build_tunnel_challenge, build_tunnel_ready, verify_runtime_hello,
    verify_tunnel_challenge_ack, HandshakeError,
};
use crate::tunnel::direct::tls::build_server_config;
use crate::tunnel::known_runtimes::{KnownRuntimes, TrustLevel};
use crate::tunnel::protocol::TunnelMessage;
use crate::tunnel::{TunnelHandle, TUNNEL_OUTBOUND_BUFFER_SIZE};

/// Handler type for inbound direct messages.
pub type DirectMessageHandler = Arc<
    dyn Fn(TunnelMessage, TunnelHandle) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Errors that can occur in the direct server.
#[derive(Debug, thiserror::Error)]
pub enum DirectServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Handshake error: {0}")]
    Handshake(#[from] HandshakeError),
    #[error("Wire encoding error: {0}")]
    Wire(String),
    #[error("Peer {0} is not authorized")]
    NotAuthorized(String),
    #[error("Missing TLS certificate or key")]
    MissingTlsCredentials,
}

/// Stream type for an accepted direct connection.
///
/// Tokio-tungstenite's `MaybeTlsStream` only models the client side, so
/// we provide a small server-side enum that delegates `AsyncRead` and
/// `AsyncWrite` to either a plaintext TCP stream or a TLS stream.
enum ServerStream {
    Plain(TcpStream),
    Tls(tokio_rustls::server::TlsStream<TcpStream>),
}

impl tokio::io::AsyncRead for ServerStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for ServerStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match &mut *self {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Server that accepts inbound direct connections from authorized peer runtimes.
pub struct DirectServer {
    config: DirectNetworkConfig,
    signing_key: Arc<SigningKey>,
    runtime_id: String,
    known_runtimes: Arc<RwLock<KnownRuntimes>>,
    handler: DirectMessageHandler,
}

impl DirectServer {
    /// Create a new direct server from configuration.
    pub fn new(
        config: DirectNetworkConfig,
        signing_key: Arc<SigningKey>,
        runtime_id: String,
        known_runtimes: Arc<RwLock<KnownRuntimes>>,
        handler: DirectMessageHandler,
    ) -> Self {
        Self {
            config,
            signing_key,
            runtime_id,
            known_runtimes,
            handler,
        }
    }

    /// Start the server and return its bound address.
    pub async fn start(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<SocketAddr, DirectServerError> {
        let addr = format!("{}:{}", self.config.bind_address, self.config.port);
        let listener = TcpListener::bind(&addr).await?;
        let bound_addr = listener.local_addr()?;
        tracing::info!("Direct server listening on {}", bound_addr);

        let acceptor = if self.config.tls_required {
            let cert_path = self
                .config
                .tls_cert_path
                .as_ref()
                .ok_or(DirectServerError::MissingTlsCredentials)?;
            let key_path = self
                .config
                .tls_key_path
                .as_ref()
                .ok_or(DirectServerError::MissingTlsCredentials)?;
            let client_ca_path = self.config.tls_client_ca_path.as_deref();
            let server_config = build_server_config(cert_path, key_path, client_ca_path)
                .map_err(|e| DirectServerError::Tls(e.to_string()))?;
            Some(tokio_rustls::TlsAcceptor::from(server_config))
        } else {
            None
        };

        let server = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        tracing::info!("Direct server shutting down");
                        break;
                    }
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, peer_addr)) => {
                                let server = server.clone();
                                let acceptor = acceptor.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = server.handle_connection(stream, peer_addr, acceptor).await {
                                        tracing::warn!(%peer_addr, error = %e, "direct connection failed");
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "direct accept failed");
                            }
                        }
                    }
                }
            }
        });

        Ok(bound_addr)
    }

    async fn handle_connection(
        &self,
        stream: TcpStream,
        peer_addr: SocketAddr,
        acceptor: Option<tokio_rustls::TlsAcceptor>,
    ) -> Result<(), DirectServerError> {
        let stream: ServerStream = match acceptor {
            Some(acceptor) => {
                let tls_stream = acceptor.accept(stream).await.map_err(|e| {
                    DirectServerError::Tls(format!("TLS accept failed: {e}"))
                })?;
                ServerStream::Tls(tls_stream)
            }
            None => ServerStream::Plain(stream),
        };

        let mut ws_stream = accept_async_with_config(stream, Some(WebSocketConfig::default()))
            .await?;

        // 1. Receive RuntimeHello
        let hello_msg = ws_stream
            .next()
            .await
            .ok_or(DirectServerError::Handshake(HandshakeError::MissingField(
                "runtime_hello".to_string(),
            )))??;
        let peer_runtime_id =
            verify_runtime_hello(&decode_message(hello_msg)?)?;

        // 2. Authorize peer
        {
            let registry = self.known_runtimes.read().await;
            let peer = registry.find(&peer_runtime_id);
            if !matches!(peer.map(|p| p.trust_level), Some(TrustLevel::Authorized)) {
                return Err(DirectServerError::NotAuthorized(peer_runtime_id));
            }
        }
        tracing::info!(%peer_addr, %peer_runtime_id, "direct peer authenticated");

        // 3. Send TunnelChallenge
        let challenge = build_tunnel_challenge();
        let challenge_nonce = if let TunnelMessage::TunnelChallenge { nonce } = &challenge {
            nonce.clone()
        } else {
            unreachable!()
        };
        ws_stream
            .send(Message::Binary(
                challenge
                    .to_bytes()
                    .map_err(|e| DirectServerError::Wire(e.to_string()))?,
            ))
            .await?;

        // 4. Receive TunnelChallengeAck
        let ack_msg = ws_stream
            .next()
            .await
            .ok_or(DirectServerError::Handshake(HandshakeError::MissingField(
                "tunnel_challenge_ack".to_string(),
            )))??;
        verify_tunnel_challenge_ack(
            &decode_message(ack_msg)?, &peer_runtime_id, &challenge_nonce)?;

        // 5. Send TunnelReady
        let ready = build_tunnel_ready();
        ws_stream
            .send(Message::Binary(
                ready
                    .to_bytes()
                    .map_err(|e| DirectServerError::Wire(e.to_string()))?,
            ))
            .await?;

        // 6. Build handle and run read loop
        let (tx, rx) = mpsc::channel(TUNNEL_OUTBOUND_BUFFER_SIZE);
        let handle = TunnelHandle::from_sender(tx.clone());
        let handler = self.handler.clone();
        tokio::spawn(run_server_read_loop(ws_stream, rx, handler, handle));

        Ok(())
    }
}

impl Clone for DirectServer {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            signing_key: self.signing_key.clone(),
            runtime_id: self.runtime_id.clone(),
            known_runtimes: self.known_runtimes.clone(),
            handler: self.handler.clone(),
        }
    }
}

fn decode_message(msg: Message) -> Result<TunnelMessage, DirectServerError> {
    let bytes = match msg {
        Message::Binary(bytes) => bytes,
        Message::Text(text) => text.into_bytes(),
        Message::Close(frame) => {
            return Err(DirectServerError::Handshake(HandshakeError::UnexpectedMessage {
                expected: "handshake".to_string(),
                actual: format!("close: {frame:?}"),
            }))
        }
        other => {
            return Err(DirectServerError::Handshake(HandshakeError::UnexpectedMessage {
                expected: "handshake".to_string(),
                actual: format!("{other:?}"),
            }))
        }
    };
    TunnelMessage::from_bytes(&bytes).map_err(|e| DirectServerError::Wire(e.to_string()))
}

/// Background read loop for an inbound direct connection.
async fn run_server_read_loop(
    mut ws_stream: WebSocketStream<ServerStream>,
    mut rx: mpsc::Receiver<TunnelMessage>,
    handler: DirectMessageHandler,
    handle: TunnelHandle,
) {
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                let bytes = match msg.to_bytes() {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to encode direct message");
                        break;
                    }
                };
                if ws_stream.send(Message::Binary(bytes)).await.is_err() {
                    break;
                }
            }
            incoming = ws_stream.next() => {
                match incoming {
                    Some(Ok(Message::Binary(bytes))) => {
                        dispatch_direct_message(&bytes, &handler, handle.clone()).await;
                    }
                    Some(Ok(Message::Text(text))) => {
                        dispatch_direct_message(text.as_bytes(), &handler, handle.clone()).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "direct connection read error");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn dispatch_direct_message(
    bytes: &[u8],
    handler: &DirectMessageHandler,
    handle: TunnelHandle,
) {
    match TunnelMessage::from_bytes(bytes) {
        Ok(msg) => {
            handler(msg, handle).await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode direct message");
        }
    }
}
