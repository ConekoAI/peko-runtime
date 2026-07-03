//! Direct cross-runtime connection integration tests.
//!
//! These tests spin up an inbound `DirectServer` and dial it with the
//! outbound `DirectClient`, verifying the runtime-identity handshake and
//! the ability to exchange `TunnelMessage`s over the resulting channel.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use peko::tunnel::a2a_pending::PendingA2aResponses;
use peko::tunnel::direct::{
    DirectConnectionManager, DirectMessageHandler, DirectServer, DirectTlsConfig,
};
use peko::tunnel::known_runtimes::{KnownRuntimes, TrustLevel};
use peko::tunnel::protocol::TunnelMessage;
use peko::tunnel::verifying_key_to_did_key;
use tokio::sync::RwLock;

fn runtime_id_for(signing_key: &SigningKey) -> String {
    verifying_key_to_did_key(&signing_key.verifying_key())
}

fn build_test_keypair() -> SigningKey {
    SigningKey::from_bytes(&[1u8; 32])
}

fn build_peer_keypair() -> SigningKey {
    SigningKey::from_bytes(&[2u8; 32])
}

fn plaintext_direct_config() -> peko::common::types::config::DirectNetworkConfig {
    peko::common::types::config::DirectNetworkConfig {
        enabled: true,
        bind_address: "127.0.0.1".to_string(),
        port: 0,
        tls_required: false,
        tls_cert_path: None,
        tls_key_path: None,
        tls_client_ca_path: None,
    }
}

#[tokio::test]
async fn direct_server_client_handshake_and_message_roundtrip() {
    // ── identities ──────────────────────────────────────────────
    let server_key = build_test_keypair();
    let client_key = build_peer_keypair();
    let server_runtime_id = runtime_id_for(&server_key);
    let client_runtime_id = runtime_id_for(&client_key);

    // ── server-side known runtimes: client must be Authorized ─────
    let mut known = KnownRuntimes::new();
    known.register_with_direct(
        &client_runtime_id,
        "Peer Runtime",
        None,
        None,
        peko::tunnel::known_runtimes::TransportPreference::Auto,
        None,
        TrustLevel::Authorized,
    );
    let known_runtimes = Arc::new(RwLock::new(known));

    // ── simple echo handler: respond to AgentToAgentRequest ───────
    let handler: DirectMessageHandler = Arc::new(
        move |msg: TunnelMessage, handle: peko::tunnel::TunnelHandle| {
            Box::pin(async move {
                if let TunnelMessage::AgentToAgentRequest {
                    request_id,
                    caller_runtime_id,
                    caller_principal_did,
                    target_principal_did,
                    session_id,
                    message,
                    signature: _,
                } = msg
                {
                    let response = TunnelMessage::AgentToAgentResponse {
                        request_id: request_id.clone(),
                        payload: format!("echo:{message}").into_bytes(),
                    };
                    if let Err(e) = handle.send(response) {
                        eprintln!("handler: failed to send response: {e}");
                    }
                    // Avoid unused variable warnings in release builds.
                    let _ = (
                        caller_runtime_id,
                        caller_principal_did,
                        target_principal_did,
                        session_id,
                    );
                }
            })
                as Pin<Box<dyn Future<Output = ()> + Send>>
        },
    );

    // ── start server ─────────────────────────────────────────────
    let server_config = plaintext_direct_config();
    let server = DirectServer::new(
        server_config,
        Arc::new(server_key),
        server_runtime_id.clone(),
        known_runtimes.clone(),
        handler,
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let bound_addr = server.start(cancel.clone()).await.expect("server starts");

    // ── client connects and sends a request ──────────────────────
    let pending = Arc::new(PendingA2aResponses::new());
    let manager = DirectConnectionManager::new(
        Arc::new(client_key),
        client_runtime_id.clone(),
        false,
        pending.clone(),
    );

    let endpoint = format!("ws://{bound_addr}");
    let request_id = "direct-test-1".to_string();
    let rx = pending.register(&request_id).expect("register");

    let handle = manager
        .get_or_connect(
            &server_runtime_id,
            &endpoint,
            None::<&DirectTlsConfig>,
        )
        .await
        .expect("client connects");

    let request = TunnelMessage::AgentToAgentRequest {
        request_id: request_id.clone(),
        caller_runtime_id: client_runtime_id.clone(),
        caller_principal_did: "did:peko:principal:caller".to_string(),
        target_principal_did: "did:peko:principal:target".to_string(),
        session_id: None,
        message: "hello direct".to_string(),
        signature: "dummy".to_string(),
    };
    handle.send(request).expect("send request");

    // ── await response ───────────────────────────────────────────
    let payload = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("response within timeout")
        .expect("response channel not dropped");
    assert_eq!(payload, b"echo:hello direct");

    // Cleanup: dropping the manager closes the connection; cancelling
    // the server stops the accept loop.
    cancel.cancel();
}

#[tokio::test]
async fn direct_server_rejects_unauthorized_peer() {
    let server_key = build_test_keypair();
    let client_key = build_peer_keypair();
    let server_runtime_id = runtime_id_for(&server_key);
    let client_runtime_id = runtime_id_for(&client_key);

    // Known runtimes is empty: the client is not Authorized.
    let known_runtimes = Arc::new(RwLock::new(KnownRuntimes::new()));

    let handler: DirectMessageHandler = Arc::new(|_msg, _handle| {
        Box::pin(async move {}) as Pin<Box<dyn Future<Output = ()> + Send>>
    });

    let server = DirectServer::new(
        plaintext_direct_config(),
        Arc::new(server_key),
        server_runtime_id.clone(),
        known_runtimes,
        handler,
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let bound_addr = server.start(cancel.clone()).await.expect("server starts");

    let pending = Arc::new(PendingA2aResponses::new());
    let result = DirectConnectionManager::new(
        Arc::new(client_key),
        client_runtime_id,
        false,
        pending,
    )
    .get_or_connect(
        &server_runtime_id,
        &format!("ws://{bound_addr}"),
        None::<&DirectTlsConfig>,
    )
    .await;

    assert!(
        result.is_err(),
        "connection to unauthorized peer must fail: {result:?}"
    );

    cancel.cancel();
}
