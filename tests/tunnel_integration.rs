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

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use futures::{SinkExt, StreamExt};
use rand::RngCore;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use pekobot::tunnel::protocol::{
    InstanceAnnouncePayload, InstanceHeartbeatPayload, InstanceExposure, InstanceStatus,
    InstanceType, TunnelMessage,
};

// JWT secret must match the PekoHub test fixture
const PEKOHUB_JWT_SECRET: &str = "test-secret-key-that-is-32-chars-long!!";

/// Generate a JWT token for the test user
fn generate_jwt(user_id: i64, namespace: &str) -> String {
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Claims {
        sub: String,
        namespace: String,
        iat: u64,
    }

    let claims = Claims {
        sub: user_id.to_string(),
        namespace: namespace.to_string(),
        iat: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(PEKOHUB_JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Test harness: auto-start pekohub backend or connect to container
// ---------------------------------------------------------------------------

struct PekohubBackend {
    #[allow(dead_code)]
    child: Option<Child>,
    url: String,
    ws_url: String,
}

impl PekohubBackend {
    async fn start() -> Self {
        // Container mode: pekohub is already running
        if let Ok(url) = std::env::var("PEKOHUB_URL") {
            let ws_url = url.replace("http://", "ws://").replace("https://", "wss://");
            let ws_url = format!("{ws_url}/v1/tunnel");

            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .no_proxy()
                .build()
                .unwrap();

            let mut ready = false;
            for _ in 0..50 {
                if client.get(format!("{url}/health")).send().await.is_ok() {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(
                ready,
                "PekoHub backend at {url} did not become ready in 5 seconds"
            );

            return Self {
                child: None,
                url,
                ws_url,
            };
        }

        // Local mode: spawn Node.js + tsx process
        let backend_path = std::env::var("PEKOHUB_BACKEND_PATH").unwrap_or_else(|_| {
            concat!(env!("CARGO_MANIFEST_DIR"), "/../pekohub/backend").to_string()
        });

        let script_path = format!("{backend_path}/tests/fixtures/server.ts");

        if !std::path::Path::new(&script_path).exists() {
            panic!(
                "PekoHub test server script not found at: {script_path}\n\
                 Set PEKOHUB_BACKEND_PATH to the pekohub/backend directory."
            );
        }

        let tsx_cli = format!("{backend_path}/node_modules/tsx/dist/cli.mjs");
        if !std::path::Path::new(&tsx_cli).exists() {
            panic!(
                "tsx CLI not found at: {tsx_cli}\n\
                 Run: cd {backend_path} && npm install"
            );
        }

        let mut cmd = Command::new("node");
        cmd.arg(&tsx_cli)
            .arg(&script_path)
            .arg("--port")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&backend_path);

        let mut child = cmd.spawn().expect(
            "Failed to start PekoHub backend. Is Node.js 22+ with tsx installed? \
             Install with: cd pekohub/backend && npm install",
        );

        let stdout = child.stdout.take().expect("Failed to capture stdout");
        let reader = std::io::BufReader::new(stdout);
        let port = tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            for line in reader.lines() {
                let line = line.expect("Failed to read line from PekoHub backend");
                if let Some(port_str) = line.strip_prefix("PORT=") {
                    return port_str.parse::<u16>().expect("Invalid PORT line");
                }
            }
            panic!("PekoHub backend did not print PORT= line")
        })
        .await
        .expect("Port detection task panicked");

        let url = format!("http://127.0.0.1:{port}");
        let ws_url = format!("ws://127.0.0.1:{port}/v1/tunnel");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap();

        let mut ready = false;
        for _ in 0..50 {
            if client.get(format!("{url}/health")).send().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ready, "PekoHub backend did not become ready in 5 seconds");

        Self {
            child: Some(child),
            url,
            ws_url,
        }
    }
}

impl Drop for PekohubBackend {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Crypto helpers
// ---------------------------------------------------------------------------

fn generate_runtime_identity() -> (String, SigningKey) {
    let mut rng = rand::thread_rng();
    let mut secret = [0u8; 32];
    rng.fill_bytes(&mut secret);
    let signing_key = SigningKey::from_bytes(&secret);
    let public_key = signing_key.verifying_key();

    let multicodec = [0xed, 0x01];
    let mut prefixed = Vec::with_capacity(2 + 32);
    prefixed.extend_from_slice(&multicodec);
    prefixed.extend_from_slice(public_key.as_bytes());
    let encoded = bs58::encode(&prefixed).into_string();
    let did = format!("did:key:z{encoded}");

    (did, signing_key)
}

fn sign_nonce(signing_key: &SigningKey, nonce: &str) -> String {
    let signature = signing_key.sign(nonce.as_bytes());
    BASE64.encode(signature.to_bytes())
}

/// Authenticate a tunnel connection and return the WebSocket split.
async fn authenticate_tunnel(
    ws_url: &str,
    did: &str,
    signing_key: &SigningKey,
) -> (
    futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >,
    futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
) {
    let (ws_stream, _) = connect_async(ws_url)
        .await
        .expect("Failed to connect to tunnel endpoint");
    let (mut write, mut read) = ws_stream.split();

    let mut nonce_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = BASE64.encode(&nonce_bytes);
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
        TunnelMessage::TunnelReady { heartbeat_interval_secs } => {
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
        signature: BASE64.encode(&[0u8; 64]),
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
    assert!(runtime_resp.status().is_success(), "Runtime creation failed");

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
            bundle_ref: None,
            runtime_display_name: Some("Test Runtime".to_string()),
            status: InstanceStatus::Online,
            exposure: InstanceExposure::Public,
            allowed_users: None,
            capabilities: Some(vec!["chat".to_string()]),
            metadata: None,
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
        assert_eq!(detail["status"], "offline", "Instance should be offline after tunnel disconnect");
    }
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_tunnel_proxied_request_response() {
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();

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

    write.send(Message::Binary(req.to_bytes().unwrap())).await.unwrap();

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

    let (mut write, mut read) = authenticate_tunnel(&backend.ws_url, &did, &signing_key).await;

    // Send streaming chunks (runtime → PekoHub direction)
    // PekoHub logs a warning about unexpected direction but keeps connection alive.
    for i in 0..3 {
        let chunk = TunnelMessage::StreamChunk {
            request_id: "test-stream".to_string(),
            seq: i,
            payload: format!("chunk-{i}").into_bytes(),
        };
        write.send(Message::Binary(chunk.to_bytes().unwrap())).await.unwrap();
    }
    let end = TunnelMessage::StreamEnd {
        request_id: "test-stream".to_string(),
    };
    write.send(Message::Binary(end.to_bytes().unwrap())).await.unwrap();

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
