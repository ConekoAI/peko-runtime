//! Direct connection client — dials a peer runtime over IP/port.
//!
//! The transport is WebSocket (`ws://` / `wss://`) so the existing
//! `TunnelMessage` JSON framing can be reused verbatim. Endpoints may be
//! configured as `wss://host:port`, `tls://host:port`, `ws://host:port`,
//! `tcp://host:port`, or plain `host:port`.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config, tungstenite::client::IntoClientRequest, tungstenite::Message,
    Connector, MaybeTlsStream, WebSocketStream,
};

use crate::tunnel::a2a_pending::PendingA2aResponses;
use crate::tunnel::direct::handshake::{
    build_runtime_hello, build_tunnel_challenge_ack, verify_tunnel_challenge, verify_tunnel_ready,
    HandshakeError,
};
use crate::tunnel::direct::tls::{build_client_config, TlsError};
use crate::tunnel::direct::DirectTlsConfig;
use crate::tunnel::protocol::TunnelMessage;
use crate::tunnel::{TunnelHandle, TUNNEL_OUTBOUND_BUFFER_SIZE};

/// Errors that can occur in direct client connections.
#[derive(Debug, thiserror::Error)]
pub enum DirectConnectionError {
    #[error("Invalid endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("TLS configuration error: {0}")]
    Tls(#[from] TlsError),
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Handshake error: {0}")]
    Handshake(#[from] HandshakeError),
    #[error("Wire encoding error: {0}")]
    Wire(String),
    #[error("Connection closed unexpectedly")]
    Closed,
    #[error("Timeout waiting for handshake message")]
    HandshakeTimeout,
}

/// An established direct connection to a peer runtime.
#[derive(Clone, Debug)]
pub struct DirectConnection {
    pub handle: TunnelHandle,
    pub runtime_id: String,
}

/// Client that establishes a direct connection to a peer runtime.
#[derive(Debug)]
pub struct DirectClient;

impl DirectClient {
    /// Connect to a peer runtime at the given endpoint.
    ///
    /// `runtime_id` is the local runtime's DID; it is sent in the
    /// `RuntimeHello` and used to sign the challenge response.
    /// `endpoint` is normalized to a WebSocket URL.
    /// `tls` is optional per-peer TLS configuration.
    /// `signing_key` is the local runtime's Ed25519 signing key.
    /// `tls_required` is the global direct-mode TLS requirement; if true
    /// and the endpoint does not specify a secure scheme, the connection
    /// is upgraded to `wss://`.
    /// `pending` is the local a2a response correlation registry; incoming
    /// `AgentToAgentResponse` messages are completed there.
    pub async fn connect(
        endpoint: &str,
        runtime_id: &str,
        tls: Option<&DirectTlsConfig>,
        signing_key: Arc<SigningKey>,
        tls_required: bool,
        pending: Arc<PendingA2aResponses>,
    ) -> Result<DirectConnection, DirectConnectionError> {
        let (url, use_tls) = normalize_endpoint(endpoint, tls_required)?;

        let connector = if use_tls {
            let client_config = build_client_config(
                tls.and_then(|t| t.ca_path.as_deref()),
                tls.and_then(|t| t.cert_path.as_deref()),
                tls.and_then(|t| t.key_path.as_deref()),
                tls.and_then(|t| t.pinned_cert_sha256.as_deref()),
            )?;
            Some(Connector::Rustls(client_config))
        } else {
            None
        };

        let req = url
            .into_client_request()
            .map_err(|e| DirectConnectionError::InvalidEndpoint(e.to_string()))?;
        let (ws_stream, _response) = match connector {
            Some(connector) => connect_async_tls_with_config(req, None, false, Some(connector)).await?,
            None => tokio_tungstenite::connect_async(req).await?,
        };

        Self::finish_handshake(ws_stream, runtime_id, signing_key, pending).await
    }

    /// Run the outbound handshake over an already-established WebSocket.
    async fn finish_handshake(
        mut ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
        runtime_id: &str,
        signing_key: Arc<SigningKey>,
        pending: Arc<PendingA2aResponses>,
    ) -> Result<DirectConnection, DirectConnectionError> {
        // 1. Send RuntimeHello
        let hello = build_runtime_hello(runtime_id, &signing_key);
        ws_stream
            .send(Message::Binary(
                hello
                    .to_bytes()
                    .map_err(|e| DirectConnectionError::Wire(e.to_string()))?,
            ))
            .await?;

        // 2. Receive TunnelChallenge
        let challenge_msg = ws_stream
            .next()
            .await
            .ok_or(DirectConnectionError::Closed)??;
        let challenge_nonce = verify_tunnel_challenge(&decode_message(challenge_msg)?,
        )?;

        // 3. Send TunnelChallengeAck
        let ack = build_tunnel_challenge_ack(&challenge_nonce, &signing_key);
        ws_stream
            .send(Message::Binary(
                ack.to_bytes()
                    .map_err(|e| DirectConnectionError::Wire(e.to_string()))?,
            ))
            .await?;

        // 4. Receive TunnelReady
        let ready_msg = ws_stream
            .next()
            .await
            .ok_or(DirectConnectionError::Closed)??;
        verify_tunnel_ready(&decode_message(ready_msg)?)?;

        // The peer's runtime_id is not known until the server sends it.
        // For direct connections the server does not echo its runtime_id,
        // so callers that need it should pass the expected peer runtime_id
        // separately. We store an empty string here as a placeholder.
        let peer_runtime_id = String::new();

        let (tx, rx) = mpsc::channel(TUNNEL_OUTBOUND_BUFFER_SIZE);
        let handle = TunnelHandle::from_sender(tx);

        tokio::spawn(run_client_read_loop(ws_stream, rx, pending));

        Ok(DirectConnection {
            handle,
            runtime_id: peer_runtime_id,
        })
    }
}

/// Normalize a direct endpoint into a WebSocket URL and TLS flag.
///
/// Supported forms:
/// - `wss://host:port/path` → secure WebSocket
/// - `ws://host:port/path` → plaintext WebSocket
/// - `tls://host:port` → `wss://host:port/`
/// - `tcp://host:port` → `ws://host:port/`
/// - `host:port` → `wss://host:port/` if `tls_required`, else `ws://host:port/`
fn normalize_endpoint(
    endpoint: &str,
    tls_required: bool,
) -> Result<(String, bool), DirectConnectionError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(DirectConnectionError::InvalidEndpoint(
            "empty endpoint".to_string(),
        ));
    }

    if let Some(rest) = endpoint.strip_prefix("wss://") {
        return Ok((format!("wss://{rest}"), true));
    }
    if let Some(rest) = endpoint.strip_prefix("ws://") {
        if tls_required {
            return Err(DirectConnectionError::InvalidEndpoint(
                "plaintext ws:// is not allowed when direct.tls_required is true".to_string(),
            ));
        }
        return Ok((format!("ws://{rest}"), false));
    }
    if let Some(rest) = endpoint.strip_prefix("tls://") {
        return Ok((format!("wss://{rest}"), true));
    }
    if let Some(rest) = endpoint.strip_prefix("tcp://") {
        if tls_required {
            return Err(DirectConnectionError::InvalidEndpoint(
                "plaintext tcp:// is not allowed when direct.tls_required is true".to_string(),
            ));
        }
        return Ok((format!("ws://{rest}"), false));
    }

    // No scheme: default based on tls_required.
    if tls_required {
        Ok((format!("wss://{endpoint}"), true))
    } else {
        Ok((format!("ws://{endpoint}"), false))
    }
}

fn decode_message(msg: Message) -> Result<TunnelMessage, DirectConnectionError> {
    let bytes = match msg {
        Message::Binary(bytes) => bytes,
        Message::Text(text) => text.into_bytes(),
        Message::Close(frame) => {
            return Err(DirectConnectionError::Handshake(HandshakeError::UnexpectedMessage {
                expected: "handshake".to_string(),
                actual: format!("close: {frame:?}"),
            }))
        }
        other => {
            return Err(DirectConnectionError::Handshake(HandshakeError::UnexpectedMessage {
                expected: "handshake".to_string(),
                actual: format!("{other:?}"),
            }))
        }
    };
    TunnelMessage::from_bytes(&bytes).map_err(|e| DirectConnectionError::Wire(e.to_string()))
}

/// Background read loop for an outbound direct connection.
///
/// Reads `TunnelMessage`s from the WebSocket and forwards
/// `AgentToAgentResponse` messages into the local pending registry via
/// the dispatcher callback. The write side is driven by the returned
/// `TunnelHandle`.
///
/// For now this loop only drains the stream and logs non-response
/// messages. It will be wired into the dispatcher in a follow-up.
async fn run_client_read_loop(
    mut ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    mut rx: mpsc::Receiver<TunnelMessage>,
    pending: Arc<PendingA2aResponses>,
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
                        handle_direct_message(&bytes, &pending);
                    }
                    Some(Ok(Message::Text(text))) => {
                        handle_direct_message(text.as_bytes(), &pending);
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

fn handle_direct_message(bytes: &[u8], pending: &PendingA2aResponses) {
    match TunnelMessage::from_bytes(bytes) {
        Ok(TunnelMessage::AgentToAgentResponse { request_id, payload }) => {
            let completed = pending.complete(&request_id, payload);
            tracing::debug!(
                request_id = %request_id,
                completed,
                "received direct AgentToAgentResponse"
            );
        }
        Ok(other) => {
            tracing::debug!(message = ?other, "received direct message");
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode direct message");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_wss_endpoint() {
        let (url, tls) = normalize_endpoint("wss://192.168.1.10:11436/v1/direct", true).unwrap();
        assert_eq!(url, "wss://192.168.1.10:11436/v1/direct");
        assert!(tls);
    }

    #[test]
    fn normalize_tls_endpoint() {
        let (url, tls) = normalize_endpoint("tls://192.168.1.10:11436", true).unwrap();
        assert_eq!(url, "wss://192.168.1.10:11436");
        assert!(tls);
    }

    #[test]
    fn normalize_tcp_endpoint_when_tls_not_required() {
        let (url, tls) = normalize_endpoint("tcp://192.168.1.10:11436", false).unwrap();
        assert_eq!(url, "ws://192.168.1.10:11436");
        assert!(!tls);
    }

    #[test]
    fn normalize_tcp_endpoint_rejected_when_tls_required() {
        assert!(normalize_endpoint("tcp://192.168.1.10:11436", true).is_err());
    }

    #[test]
    fn normalize_bare_endpoint_defaults_to_tls() {
        let (url, tls) = normalize_endpoint("192.168.1.10:11436", true).unwrap();
        assert_eq!(url, "wss://192.168.1.10:11436");
        assert!(tls);
    }
}
