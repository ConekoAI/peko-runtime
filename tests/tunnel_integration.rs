//! Tunnel Integration Tests (Layer 2)
//!
//! End-to-end tests for the runtime↔PekoHub WebSocket tunnel.
//!
//! These tests are marked `#[ignore]` because they require:
//!   - Node.js 22+ with tsx installed  (local mode)
//!   - OR a running PekoHub test container (container mode via PEKOHUB_URL)
//!
//! Run locally:
//!   cd peko-runtime
//!   cargo test --test tunnel_integration -- --ignored
//!
//! Run in container:
//!   PEKOHUB_URL=http://pekohub-test:3000 cargo test --test tunnel_integration -- --ignored

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::SigningKey;
use futures::{SinkExt, StreamExt};
use rand::RngCore;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use peko::tunnel::known_runtimes::TransportPreference;
use peko::tunnel::protocol::{
    InstanceAnnouncePayload, InstanceExposure, InstanceHeartbeatPayload, InstanceStatus,
    InstanceType, TunnelMessage,
};

mod common;
use common::{generate_jwt, generate_runtime_identity, sign_nonce, PekohubBackend};

/// Pre-seed a `runtimes` row so PekoHub's allowlist (issue #1)
/// admits the handshake. The test backend's `/test/create-user`
/// requires `owner_id`, so we create a throwaway user with a
/// unique namespace and then point the runtime at it. Idempotent
/// — re-running with the same DID returns success on the second
/// call (the create-runtime endpoint is plain INSERT, so we
/// ignore non-2xx here, which is fine for tests).
async fn seed_runtime_for_test(backend_url: &str, did: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Create a throwaway user
    let user_namespace = format!(
        "tunnelseed-{}",
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
            "display_name": "Tunnel Seed User",
            "email": "seed@test.com"
        }))
        .send()
        .await
        .expect("Failed to create seed user");
    assert!(user_resp.status().is_success(), "Seed user creation failed");
    let user_body: serde_json::Value = user_resp.json().await.unwrap();
    let user_id = user_body["id"].as_i64().expect("No user id") as i32;

    // Pre-seed the runtime. The endpoint is a plain INSERT so a
    // re-seed returns 500; we treat that as success.
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

/// Authenticate a tunnel connection and return the WebSocket split.
///
/// Drives the full 3-step handshake (pekohub issue #1):
///   1. send `RuntimeHello` with a runtime-picked nonce
///   2. read the server-issued `TunnelChallenge` and reply with a
///      signed `TunnelChallengeAck`
///   3. read the server's `TunnelReady`
///
/// NOTE: callers MUST have already inserted a `runtimes` row for the
/// given DID (via `/test/create-runtime`) — the server-side allowlist
/// closes the socket with 1008 if the runtime is unknown.
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

    // 1. RuntimeHello
    let mut nonce_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = BASE64.encode(nonce_bytes);
    let signature = sign_nonce(signing_key, &nonce);

    let hello = TunnelMessage::RuntimeHello {
        runtime_id: did.to_string(),
        nonce,
        signature,
    };
    write
        .send(Message::Binary(hello.to_bytes().unwrap()))
        .await
        .expect("Failed to send RuntimeHello");

    // 2. Wait for the server's challenge
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
        TunnelMessage::Disconnect { reason } => {
            panic!("Tunnel rejected connection: {reason}");
        }
        other => panic!("Expected TunnelChallenge, got: {:?}", other),
    };

    // 3. Sign the challenge nonce and send the ack
    let challenge_signature = sign_nonce(signing_key, &challenge_nonce);
    let ack = TunnelMessage::TunnelChallengeAck {
        nonce: challenge_nonce,
        signature: challenge_signature,
    };
    write
        .send(Message::Binary(ack.to_bytes().unwrap()))
        .await
        .expect("Failed to send TunnelChallengeAck");

    // 4. Wait for TunnelReady
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
        TunnelMessage::Disconnect { reason } => {
            panic!("Tunnel rejected connection: {reason}");
        }
        other => panic!("Expected TunnelReady, got: {:?}", other),
    }

    (write, read)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_tunnel_handshake_and_heartbeat() {
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();
    seed_runtime_for_test(&backend.url, &did).await;

    let (mut write, mut read) = authenticate_tunnel(&backend.ws_url, &did, &signing_key).await;

    // Send heartbeat, expect ack
    let heartbeat = TunnelMessage::Heartbeat { seq: 1 };
    write
        .send(Message::Binary(heartbeat.to_bytes().unwrap()))
        .await
        .unwrap();

    let ack_msg = timeout(Duration::from_secs(5), read.next())
        .await
        .expect("Timeout waiting for heartbeat ack")
        .expect("WebSocket closed")
        .expect("WebSocket error");

    let ack = match ack_msg {
        Message::Binary(bytes) => TunnelMessage::from_bytes(&bytes).unwrap(),
        Message::Text(text) => TunnelMessage::from_bytes(text.as_bytes()).unwrap(),
        other => panic!("Expected Binary or Text, got: {:?}", other),
    };

    match ack {
        TunnelMessage::HeartbeatAck { seq } => assert_eq!(seq, 1),
        other => panic!("Expected HeartbeatAck, got: {:?}", other),
    }
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_tunnel_rejects_invalid_signature() {
    let backend = PekohubBackend::start().await;
    let (did, _signing_key) = generate_runtime_identity();

    let (ws_stream, _) = connect_async(&backend.ws_url)
        .await
        .expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    let hello = TunnelMessage::RuntimeHello {
        runtime_id: did,
        nonce: "nonce".to_string(),
        signature: BASE64.encode([0u8; 64]),
    };
    write
        .send(Message::Binary(hello.to_bytes().unwrap()))
        .await
        .unwrap();

    let msg = timeout(Duration::from_secs(5), read.next())
        .await
        .expect("Timeout")
        .expect("WebSocket closed")
        .expect("WebSocket error");

    let decoded = match msg {
        Message::Binary(bytes) => TunnelMessage::from_bytes(&bytes).unwrap(),
        Message::Text(text) => TunnelMessage::from_bytes(text.as_bytes()).unwrap(),
        other => panic!("Expected message, got: {:?}", other),
    };

    match decoded {
        TunnelMessage::Disconnect { reason } => {
            assert!(
                reason.to_lowercase().contains("auth")
                    || reason.to_lowercase().contains("signature")
                    || reason.to_lowercase().contains("invalid"),
                "Expected auth-related disconnect reason, got: {reason}"
            );
        }
        other => panic!("Expected Disconnect, got: {:?}", other),
    }
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_tunnel_instance_announce_and_api_visibility() {
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Create user via test-only endpoint
    let user_resp = client
        .post(format!("{}/test/create-user", backend.url))
        .json(&serde_json::json!({
            "external_id": "tunnel-test-user",
            "provider": "github",
            "namespace": "tunneltestuser",
            "display_name": "Tunnel Test User",
            "email": "tunnel@test.com"
        }))
        .send()
        .await
        .expect("Failed to create test user");
    assert!(user_resp.status().is_success(), "Test user creation failed");
    let user_body: serde_json::Value = user_resp.json().await.unwrap();
    let user_id = user_body["id"].as_i64().expect("No user id") as i32;

    // Create runtime record for owner resolution
    let runtime_resp = client
        .post(format!("{}/test/create-runtime", backend.url))
        .json(&serde_json::json!({
            "runtime_did": did,
            "owner_id": user_id,
            "display_name": "Test Runtime"
        }))
        .send()
        .await
        .expect("Failed to create runtime");
    assert!(
        runtime_resp.status().is_success(),
        "Runtime creation failed"
    );

    // Generate JWT for authenticated requests
    let jwt_token = generate_jwt(user_id as i64, "tunneltestuser");
    let auth_header = format!("Bearer {jwt_token}");

    let (mut write, read) = authenticate_tunnel(&backend.ws_url, &did, &signing_key).await;
    let _read = read; // keep alive

    // Announce an instance
    let instance_id = uuid::Uuid::new_v5(
        &uuid::uuid!("a1b2c3d4-e5f6-47a8-b9c0-d1e2f3a4b5c6"),
        format!("{did}:test-agent").as_bytes(),
    )
    .to_string();

    let announce = TunnelMessage::InstanceAnnounce {
        payload: InstanceAnnouncePayload {
            id: instance_id.clone(),
            instance_type: InstanceType::Agent,
            name: "test-agent".to_string(),
            agent_did: None,
            bundle_ref: None,
            principal_did: None,
            runtime_display_name: Some("Test Runtime".to_string()),
            status: InstanceStatus::Online,
            exposure: InstanceExposure::Public,
            allowed_principals: None,
            capabilities: Some(vec!["chat".to_string()]),
            metadata: None,
            transport_preference: None,
            runtime_direct_endpoint: None,
        },
    };
    write
        .send(Message::Binary(announce.to_bytes().unwrap()))
        .await
        .unwrap();

    // Give PekoHub time to process
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Query instances API with auth
    let list_resp = client
        .get(format!("{}/v1/instances", backend.url))
        .header("Authorization", &auth_header)
        .query(&[("runtime_id", &did)])
        .send()
        .await
        .expect("Failed to list instances");

    assert_eq!(list_resp.status(), 200, "List instances failed");
    let list_body: serde_json::Value = list_resp.json().await.unwrap();
    let instances = list_body["data"].as_array().expect("Expected data array");
    assert!(
        instances.iter().any(|i| i["id"] == instance_id),
        "Announced instance should appear in API. Got: {:?}",
        instances
    );

    // Send heartbeat
    let heartbeat = TunnelMessage::InstanceHeartbeat {
        payload: InstanceHeartbeatPayload {
            id: instance_id.clone(),
            status: InstanceStatus::Online,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
    };
    write
        .send(Message::Binary(heartbeat.to_bytes().unwrap()))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify instance is still online
    let detail_resp = client
        .get(format!("{}/v1/instances/{instance_id}", backend.url))
        .header("Authorization", &auth_header)
        .send()
        .await
        .expect("Failed to get instance detail");

    if detail_resp.status() == 200 {
        let detail: serde_json::Value = detail_resp.json().await.unwrap();
        assert_eq!(detail["status"], "online");
    }

    // Drop connection and verify instances go offline
    drop(write);
    drop(_read);

    tokio::time::sleep(Duration::from_millis(500)).await;

    let offline_resp = client
        .get(format!("{}/v1/instances/{instance_id}", backend.url))
        .header("Authorization", &auth_header)
        .send()
        .await
        .expect("Failed to get instance after disconnect");

    if offline_resp.status() == 200 {
        let detail: serde_json::Value = offline_resp.json().await.unwrap();
        assert_eq!(
            detail["status"], "offline",
            "Instance should be offline after tunnel disconnect"
        );
    }
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_tunnel_proxied_request_response() {
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();
    seed_runtime_for_test(&backend.url, &did).await;

    let (mut write, mut read) = authenticate_tunnel(&backend.ws_url, &did, &signing_key).await;

    // Send a proxied request (wrong direction, but tests serialization roundtrip)
    let request_id = "test-req-1".to_string();
    let req = TunnelMessage::ProxiedRequest {
        request_id: request_id.clone(),
        agent: "echo-agent".to_string(),
        payload: serde_json::json!({
            "requestId": request_id,
            "instanceId": "echo-agent",
            "method": "chat",
            "body": {"message": "hello"},
            "headers": {}
        })
        .to_string()
        .into_bytes(),
    };

    write
        .send(Message::Binary(req.to_bytes().unwrap()))
        .await
        .unwrap();

    // PekoHub will log a warning about unexpected direction but won't disconnect.
    // Verify the connection is still alive by sending a heartbeat.
    tokio::time::sleep(Duration::from_millis(200)).await;

    write
        .send(Message::Binary(
            TunnelMessage::Heartbeat { seq: 1 }.to_bytes().unwrap(),
        ))
        .await
        .unwrap();

    let ack_msg = timeout(Duration::from_secs(3), read.next())
        .await
        .expect("Timeout — connection may have been closed")
        .expect("WebSocket closed")
        .expect("WebSocket error");

    let ack = match ack_msg {
        Message::Binary(b) => TunnelMessage::from_bytes(&b).unwrap(),
        Message::Text(t) => TunnelMessage::from_bytes(t.as_bytes()).unwrap(),
        other => panic!("Unexpected: {:?}", other),
    };

    match ack {
        TunnelMessage::HeartbeatAck { seq } => assert_eq!(seq, 1),
        other => panic!("Expected HeartbeatAck, got: {:?}", other),
    }
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_tunnel_streaming_chunks_survive() {
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();
    seed_runtime_for_test(&backend.url, &did).await;

    let (mut write, mut read) = authenticate_tunnel(&backend.ws_url, &did, &signing_key).await;

    // Send streaming chunks (runtime → PekoHub direction)
    // PekoHub logs a warning about unexpected direction but keeps connection alive.
    for i in 0..3 {
        let chunk = TunnelMessage::StreamChunk {
            request_id: "test-stream".to_string(),
            seq: i,
            payload: format!("chunk-{i}").into_bytes(),
        };
        write
            .send(Message::Binary(chunk.to_bytes().unwrap()))
            .await
            .unwrap();
    }
    let end = TunnelMessage::StreamEnd {
        request_id: "test-stream".to_string(),
    };
    write
        .send(Message::Binary(end.to_bytes().unwrap()))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify connection still alive via heartbeat
    write
        .send(Message::Binary(
            TunnelMessage::Heartbeat { seq: 42 }.to_bytes().unwrap(),
        ))
        .await
        .unwrap();

    let ack_msg = timeout(Duration::from_secs(3), read.next())
        .await
        .expect("Timeout — connection may have been closed")
        .expect("WebSocket closed")
        .expect("WebSocket error");

    let ack = match ack_msg {
        Message::Binary(b) => TunnelMessage::from_bytes(&b).unwrap(),
        Message::Text(t) => TunnelMessage::from_bytes(t.as_bytes()).unwrap(),
        other => panic!("Unexpected: {:?}", other),
    };

    match ack {
        TunnelMessage::HeartbeatAck { seq } => assert_eq!(seq, 42),
        other => panic!("Expected HeartbeatAck, got: {:?}", other),
    }
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_instance_announce_publishes_transport_fields() {
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();
    seed_runtime_for_test(&backend.url, &did).await;

    let (mut write, read) = authenticate_tunnel(&backend.ws_url, &did, &signing_key).await;
    let _read = read; // keep the tunnel alive

    let principal_did = "did:peko:principal:tunnel-transport-001";
    let instance_id = uuid::Uuid::new_v5(
        &uuid::uuid!("a1b2c3d4-e5f6-47a8-b9c0-d1e2f3a4b5c6"),
        format!("{did}:transport-agent").as_bytes(),
    )
    .to_string();

    let announce = TunnelMessage::InstanceAnnounce {
        payload: InstanceAnnouncePayload {
            id: instance_id,
            instance_type: InstanceType::Principal,
            name: "transport-agent".to_string(),
            agent_did: None,
            bundle_ref: None,
            principal_did: Some(principal_did.to_string()),
            runtime_display_name: Some("Transport Test Runtime".to_string()),
            status: InstanceStatus::Online,
            exposure: InstanceExposure::Public,
            allowed_principals: None,
            capabilities: Some(vec!["chat".to_string()]),
            metadata: None,
            transport_preference: Some(TransportPreference::Direct),
            runtime_direct_endpoint: Some("wss://example.com:11436".to_string()),
        },
    };
    write
        .send(Message::Binary(announce.to_bytes().unwrap()))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    let dir_resp = client
        .get(format!(
            "{}/v1/principals/by-did/{}",
            backend.url,
            urlencoding::encode(principal_did)
        ))
        .send()
        .await
        .expect("directory request failed");

    assert_eq!(
        dir_resp.status(),
        200,
        "public principal directory lookup should succeed"
    );
    let body: serde_json::Value = dir_resp.json().await.unwrap();
    assert_eq!(body["transportPreference"], "direct");
    assert_eq!(body["directEndpoint"], "wss://example.com:11436");
}
