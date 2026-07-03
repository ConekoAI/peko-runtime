//! Transport selection integration tests for cross-runtime `principal_send`.
//!
//! These tests exercise the caller-side `select_transport` logic against a
//! real PekoHub backend: the callee's preference and advertised endpoint are
//! read from the hub directory API, while the local `KnownRuntimes` registry
//! contributes trust status and operator overrides only.
//!
//! They are marked `#[ignore]` because they require a PekoHub backend. The
//! integration CI runs them with `--include-ignored`.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::SigningKey;
use futures::{SinkExt, StreamExt};
use peko::tunnel::a2a_pending::PendingA2aResponses;
use peko::tunnel::direct::{
    routing::{select_transport, TransportChoice},
    DirectConnectionManager, DirectMessageHandler, DirectServer, DirectTlsConfig,
};
use peko::tunnel::hub_directory::{AgentDirectory, HubAgentDirectoryClient, ResolvedExposure};
use peko::tunnel::known_runtimes::{KnownRuntimes, TransportPreference, TrustLevel};
use peko::tunnel::protocol::{
    InstanceAnnouncePayload, InstanceExposure, InstanceStatus, InstanceType, TunnelMessage,
};
use peko::tunnel::verifying_key_to_did_key;
use rand::RngCore;
use tokio::sync::RwLock;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

mod common;
use common::PekohubBackend;

fn runtime_id_for(signing_key: &SigningKey) -> String {
    verifying_key_to_did_key(&signing_key.verifying_key())
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
        advertise_endpoint: None,
    }
}

/// Pre-seed a `runtimes` row so PekoHub's allowlist admits the handshake.
async fn seed_runtime_for_test(backend_url: &str, did: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    let user_namespace = format!(
        "directseed-{}",
        did.trim_start_matches("did:key:z")
            .chars()
            .take(12)
            .collect::<String>()
    );
    let user_resp = client
        .post(format!("{}/test/create-user", backend_url))
        .json(&serde_json::json!({
            "external_id": format!("seed-{did}"),
            "provider": "github",
            "namespace": user_namespace,
            "display_name": "Direct Seed User",
            "email": "seed@test.com"
        }))
        .send()
        .await
        .expect("Failed to create seed user");
    assert!(user_resp.status().is_success(), "Seed user creation failed");
    let user_body: serde_json::Value = user_resp.json().await.unwrap();
    let user_id = user_body["id"].as_i64().expect("No user id") as i32;

    let resp = client
        .post(format!("{}/test/create-runtime", backend_url))
        .json(&serde_json::json!({
            "runtime_did": did,
            "owner_id": user_id,
            "display_name": "Seeded Test Runtime"
        }))
        .send()
        .await
        .expect("Failed to call create-runtime");
    let status = resp.status();
    if !status.is_success() && status.as_u16() != 500 {
        panic!("create-runtime failed unexpectedly: {status}");
    }
}

/// Drive the full 3-step tunnel handshake.
async fn authenticate_tunnel(
    ws_url: &str,
    did: &str,
    signing_key: &SigningKey,
) -> (
    futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) {
    let (ws_stream, _) = connect_async(ws_url)
        .await
        .expect("Failed to connect to tunnel endpoint");
    let (mut write, mut read) = ws_stream.split();

    let mut nonce_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = BASE64.encode(nonce_bytes);
    let signature = common::crypto::sign_nonce(signing_key, &nonce);

    let hello = TunnelMessage::RuntimeHello {
        runtime_id: did.to_string(),
        nonce,
        signature,
    };
    write
        .send(Message::Binary(hello.to_bytes().unwrap()))
        .await
        .expect("Failed to send RuntimeHello");

    let challenge_msg = timeout(Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for TunnelChallenge")
        .expect("WebSocket closed before TunnelChallenge")
        .expect("WebSocket error");
    let challenge_nonce = match challenge_msg {
        Message::Binary(bytes) => TunnelMessage::from_bytes(&bytes).unwrap(),
        Message::Text(text) => TunnelMessage::from_bytes(text.as_bytes()).unwrap(),
        other => panic!("Expected Binary or Text for challenge, got: {:?}", other),
    };
    let challenge_nonce = match challenge_nonce {
        TunnelMessage::TunnelChallenge { nonce } => nonce,
        TunnelMessage::Disconnect { reason } => panic!("Tunnel rejected connection: {reason}"),
        other => panic!("Expected TunnelChallenge, got: {:?}", other),
    };

    let challenge_signature = common::crypto::sign_nonce(signing_key, &challenge_nonce);
    let ack = TunnelMessage::TunnelChallengeAck {
        nonce: challenge_nonce,
        signature: challenge_signature,
    };
    write
        .send(Message::Binary(ack.to_bytes().unwrap()))
        .await
        .expect("Failed to send TunnelChallengeAck");

    let ready_msg = timeout(Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for TunnelReady")
        .expect("WebSocket closed before TunnelReady")
        .expect("WebSocket error");
    let ready = match ready_msg {
        Message::Binary(bytes) => TunnelMessage::from_bytes(&bytes).unwrap(),
        Message::Text(text) => TunnelMessage::from_bytes(text.as_bytes()).unwrap(),
        other => panic!("Expected Binary or Text, got: {:?}", other),
    };
    match ready {
        TunnelMessage::TunnelReady {
            heartbeat_interval_secs,
        } => {
            assert!(heartbeat_interval_secs > 0);
        }
        TunnelMessage::Disconnect { reason } => panic!("Tunnel rejected connection: {reason}"),
        other => panic!("Expected TunnelReady, got: {:?}", other),
    }

    (write, read)
}

/// Authenticate as `target_runtime_did`, then announce a public principal.
async fn announce_public_principal(
    backend: &PekohubBackend,
    target_runtime_did: &str,
    target_signing_key: &SigningKey,
    principal_did: &str,
    preference: TransportPreference,
    direct_endpoint: Option<&str>,
) {
    seed_runtime_for_test(&backend.url, target_runtime_did).await;

    let (mut write, read) = authenticate_tunnel(
        &backend.ws_url,
        target_runtime_did,
        target_signing_key,
    )
    .await;
    let _read = read;

    let instance_id = uuid::Uuid::new_v5(
        &uuid::uuid!("a1b2c3d4-e5f6-47a8-b9c0-d1e2f3a4b5c6"),
        format!("{target_runtime_did}:direct-agent").as_bytes(),
    )
    .to_string();

    let announce = TunnelMessage::InstanceAnnounce {
        payload: InstanceAnnouncePayload {
            id: instance_id,
            instance_type: InstanceType::Principal,
            name: "direct-agent".to_string(),
            agent_did: None,
            bundle_ref: None,
            principal_did: Some(principal_did.to_string()),
            runtime_display_name: Some("Direct Test Runtime".to_string()),
            status: InstanceStatus::Online,
            exposure: InstanceExposure::Public,
            allowed_principals: None,
            capabilities: Some(vec!["chat".to_string()]),
            metadata: None,
            transport_preference: Some(preference),
            runtime_direct_endpoint: direct_endpoint.map(|s| s.to_string()),
        },
    };
    write
        .send(Message::Binary(announce.to_bytes().unwrap()))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Keep the tunnel alive until the caller drops the returned sink.
    let _keep = write;
}

/// Start a plaintext echo `DirectServer` that treats `caller_runtime_id` as
/// an authorized peer.
async fn start_echo_direct_server(
    target_signing_key: Arc<SigningKey>,
    caller_runtime_id: &str,
) -> (SocketAddr, CancellationToken) {
    let mut known = KnownRuntimes::new();
    known.register_with_direct(
        caller_runtime_id,
        "Caller Runtime",
        None,
        None,
        TransportPreference::Auto,
        None,
        TrustLevel::Authorized,
    );
    let known_runtimes = Arc::new(RwLock::new(known));

    let handler: DirectMessageHandler = Arc::new(
        move |msg: TunnelMessage, handle: peko::tunnel::TunnelHandle| {
            Box::pin(async move {
                if let TunnelMessage::AgentToAgentRequest {
                    request_id,
                    message,
                    ..
                } = msg
                {
                    let response = TunnelMessage::AgentToAgentResponse {
                        request_id: request_id.clone(),
                        payload: format!("echo:{message}").into_bytes(),
                    };
                    if let Err(e) = handle.send(response) {
                        eprintln!("handler: failed to send response: {e}");
                    }
                }
            }) as Pin<Box<dyn Future<Output = ()> + Send>>
        },
    );

    let target_runtime_id = runtime_id_for(&target_signing_key);
    let server = DirectServer::new(
        plaintext_direct_config(),
        target_signing_key,
        target_runtime_id,
        known_runtimes,
        handler,
    );
    let cancel = CancellationToken::new();
    let bound_addr = server
        .start(cancel.clone())
        .await
        .expect("server starts");
    (bound_addr, cancel)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend"]
async fn direct_preference_uses_directory_endpoint() {
    let backend = PekohubBackend::start().await;
    let target_key = Arc::new(SigningKey::from_bytes(&[1u8; 32]));
    let caller_key = Arc::new(SigningKey::from_bytes(&[2u8; 32]));
    let target_runtime_id = runtime_id_for(&target_key);
    let caller_runtime_id = runtime_id_for(&caller_key);

    // Spin up the callee direct server before announcing its endpoint.
    let (server_addr, server_cancel) =
        start_echo_direct_server(target_key.clone(), &caller_runtime_id).await;
    let advertised_endpoint = format!("ws://{server_addr}");

    let principal_did = "did:peko:principal:direct-target-001";
    announce_public_principal(
        &backend,
        &target_runtime_id,
        &target_key,
        principal_did,
        TransportPreference::Direct,
        Some(&advertised_endpoint),
    )
    .await;

    // Resolve via the real hub directory API.
    let directory = HubAgentDirectoryClient::new(&backend.url);
    let resolution = directory
        .resolve_by_did(principal_did)
        .await
        .expect("directory resolves the public principal");
    assert_eq!(resolution.runtime_id, target_runtime_id);
    assert_eq!(resolution.exposure, ResolvedExposure::Public);

    // Caller authorizes the target runtime and selects direct transport.
    let mut known = KnownRuntimes::new();
    known.register_with_direct(
        &target_runtime_id,
        "Target Runtime",
        None,
        None,
        TransportPreference::Auto,
        None,
        TrustLevel::Authorized,
    );
    let known_runtimes = Arc::new(RwLock::new(known));

    let transport = {
        let guard = known_runtimes.read().await;
        select_transport(
            &resolution.runtime_id,
            resolution.direct_endpoint.as_deref(),
            resolution.transport_preference,
            &*guard,
        )
    };
    assert_eq!(
        transport,
        TransportChoice::Direct {
            endpoint: advertised_endpoint.clone()
        }
    );

    // Connect over direct transport and exchange a message.
    let pending = Arc::new(PendingA2aResponses::new());
    let manager = DirectConnectionManager::new(
        caller_key,
        caller_runtime_id.clone(),
        false,
        pending.clone(),
    );

    let request_id = "direct-policy-test-1".to_string();
    let rx = pending.register(&request_id).expect("register");

    let handle = manager
        .get_or_connect(
            &target_runtime_id,
            &advertised_endpoint,
            None::<&DirectTlsConfig>,
        )
        .await
        .expect("client connects to advertised endpoint");

    let request = TunnelMessage::AgentToAgentRequest {
        request_id: request_id.clone(),
        caller_runtime_id,
        caller_principal_did: "did:peko:principal:caller".to_string(),
        target_principal_did: principal_did.to_string(),
        session_id: None,
        message: "hello direct".to_string(),
        signature: "dummy".to_string(),
    };
    handle.send(request).expect("send request");

    let payload = timeout(Duration::from_secs(5), rx)
        .await
        .expect("response within timeout")
        .expect("response channel not dropped");
    assert_eq!(payload, b"echo:hello direct");

    server_cancel.cancel();
}

#[tokio::test]
#[ignore = "requires PekoHub backend"]
async fn tunnel_preference_avoids_direct() {
    let backend = PekohubBackend::start().await;
    let target_key = Arc::new(SigningKey::from_bytes(&[3u8; 32]));
    let target_runtime_id = runtime_id_for(&target_key);
    let principal_did = "did:peko:principal:tunnel-target-001";

    announce_public_principal(
        &backend,
        &target_runtime_id,
        &target_key,
        principal_did,
        TransportPreference::Tunnel,
        Some("wss://ignored.example.com:11436"),
    )
    .await;

    let directory = HubAgentDirectoryClient::new(&backend.url);
    let resolution = directory
        .resolve_by_did(principal_did)
        .await
        .expect("directory resolves the public principal");

    let mut known = KnownRuntimes::new();
    known.register_with_direct(
        &target_runtime_id,
        "Target Runtime",
        None,
        None,
        TransportPreference::Auto,
        None,
        TrustLevel::Authorized,
    );

    let transport = select_transport(
        &resolution.runtime_id,
        resolution.direct_endpoint.as_deref(),
        resolution.transport_preference,
        &known,
    );
    assert_eq!(transport, TransportChoice::Tunnel);
}

#[tokio::test]
#[ignore = "requires PekoHub backend"]
async fn direct_preference_without_endpoint_is_unavailable() {
    let backend = PekohubBackend::start().await;
    let target_key = Arc::new(SigningKey::from_bytes(&[4u8; 32]));
    let target_runtime_id = runtime_id_for(&target_key);
    let principal_did = "did:peko:principal:no-endpoint-001";

    announce_public_principal(
        &backend,
        &target_runtime_id,
        &target_key,
        principal_did,
        TransportPreference::Direct,
        None,
    )
    .await;

    let directory = HubAgentDirectoryClient::new(&backend.url);
    let resolution = directory
        .resolve_by_did(principal_did)
        .await
        .expect("directory resolves the public principal");

    let mut known = KnownRuntimes::new();
    known.register_with_direct(
        &target_runtime_id,
        "Target Runtime",
        None,
        None,
        TransportPreference::Auto,
        None,
        TrustLevel::Authorized,
    );

    let transport = select_transport(
        &resolution.runtime_id,
        resolution.direct_endpoint.as_deref(),
        resolution.transport_preference,
        &known,
    );
    assert!(
        matches!(transport, TransportChoice::Unavailable { .. }),
        "expected Unavailable when direct is requested but no endpoint is advertised, got {:?}",
        transport
    );
}

#[tokio::test]
#[ignore = "requires PekoHub backend"]
async fn direct_preference_unauthorized_is_unavailable() {
    let backend = PekohubBackend::start().await;
    let target_key = Arc::new(SigningKey::from_bytes(&[5u8; 32]));
    let target_runtime_id = runtime_id_for(&target_key);
    let principal_did = "did:peko:principal:unauth-001";

    announce_public_principal(
        &backend,
        &target_runtime_id,
        &target_key,
        principal_did,
        TransportPreference::Direct,
        Some("wss://unauth.example.com:11436"),
    )
    .await;

    let directory = HubAgentDirectoryClient::new(&backend.url);
    let resolution = directory
        .resolve_by_did(principal_did)
        .await
        .expect("directory resolves the public principal");

    // Known runtimes is empty: the peer is not Authorized.
    let known = KnownRuntimes::new();
    let transport = select_transport(
        &resolution.runtime_id,
        resolution.direct_endpoint.as_deref(),
        resolution.transport_preference,
        &known,
    );
    assert!(
        matches!(transport, TransportChoice::Unavailable { .. }),
        "expected Unavailable when direct is requested but peer is not authorized, got {:?}",
        transport
    );
}

#[tokio::test]
#[ignore = "requires PekoHub backend"]
async fn known_runtimes_endpoint_overrides_directory() {
    let backend = PekohubBackend::start().await;
    let target_key = Arc::new(SigningKey::from_bytes(&[6u8; 32]));
    let target_runtime_id = runtime_id_for(&target_key);
    let principal_did = "did:peko:principal:override-001";

    announce_public_principal(
        &backend,
        &target_runtime_id,
        &target_key,
        principal_did,
        TransportPreference::Direct,
        Some("wss://advertised.example.com:11436"),
    )
    .await;

    let directory = HubAgentDirectoryClient::new(&backend.url);
    let resolution = directory
        .resolve_by_did(principal_did)
        .await
        .expect("directory resolves the public principal");

    let mut known = KnownRuntimes::new();
    known.register_with_direct(
        &target_runtime_id,
        "Target Runtime",
        None,
        Some("wss://operator-override.example.com:11436".to_string()),
        TransportPreference::Auto,
        None,
        TrustLevel::Authorized,
    );

    let transport = select_transport(
        &resolution.runtime_id,
        resolution.direct_endpoint.as_deref(),
        resolution.transport_preference,
        &known,
    );
    assert_eq!(
        transport,
        TransportChoice::Direct {
            endpoint: "wss://operator-override.example.com:11436".to_string()
        }
    );
}
