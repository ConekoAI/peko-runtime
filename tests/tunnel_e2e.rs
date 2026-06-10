//! Tunnel End-to-End Integration Test (Layer 3)
//!
//! Full E2E test: runtime daemon → tunnel → PekoHub → HTTP proxy → chat → real LLM → SSE stream
//!
//! This test requires:
//!   - Node.js 22+ with tsx installed
//!   - The PekoHub backend source at `../pekohub/backend`
//!   - `MINIMAX_API_KEY` environment variable set
//!
//! The test:
//!   1. Starts PekoHub backend on a random ephemeral port
//!   2. Creates a temporary workspace with a minimax-powered agent config
//!   3. Builds a real AppState (with real agent service)
//!   4. Writes tunnel credentials pointing to PekoHub
//!   5. Starts the tunnel via AppState::start_tunnel()
//!   6. Creates a user + runtime record in PekoHub
//!   7. Sends POST /v1/instances/:id/chat via HTTP
//!   8. Consumes SSE and verifies real LLM response
//!
//! Run:
//!   cd peko-runtime
//!   MINIMAX_API_KEY=sk-xxx cargo test --test tunnel_e2e -- --ignored

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use pekobot::test_utils::{AppState, DaemonConfigSnapshot};

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
        &Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(PEKOHUB_JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Test harness: auto-start pekohub backend
// ---------------------------------------------------------------------------

struct PekohubBackend {
    #[allow(dead_code)]
    child: Child,
    url: String,
    ws_url: String,
}

impl PekohubBackend {
    async fn start() -> Self {
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

        Self { child, url, ws_url }
    }
}

impl Drop for PekohubBackend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

#[allow(dead_code)]
fn sign_nonce(signing_key: &SigningKey, nonce: &str) -> String {
    let signature = signing_key.sign(nonce.as_bytes());
    BASE64.encode(signature.to_bytes())
}

// ---------------------------------------------------------------------------
// Workspace setup
// ---------------------------------------------------------------------------

/// Create a temporary workspace with a minimax-powered agent config
async fn create_test_workspace(
    workspace_dir: &std::path::Path,
    agent_name: &str,
) -> anyhow::Result<()> {
    let config_dir = workspace_dir.join("config");
    let data_dir = workspace_dir.join("data");
    let cache_dir = workspace_dir.join("cache");

    tokio::fs::create_dir_all(&config_dir).await?;
    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&cache_dir).await?;

    // Create agents directory
    let agents_dir = config_dir.join("agents");
    tokio::fs::create_dir_all(&agents_dir).await?;

    // Create agent directory
    let agent_dir = agents_dir.join(agent_name);
    tokio::fs::create_dir_all(&agent_dir).await?;

    // Write agent config TOML using minimax provider
    let api_key = std::env::var("MINIMAX_API_KEY")
        .map_err(|_| anyhow::anyhow!("MINIMAX_API_KEY environment variable not set"))?;

    let config_toml = format!(
        r#"version = "1.0"
name = "{agent_name}"
description = "E2E test agent"
auto_accept_trusted = false
default_timeout_seconds = 60

[provider]
provider_type = "minimax"
api_key = "{api_key}"
default_model = "default"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "MiniMax-M2.7"
max_tokens = 1024
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

[extensions]
enabled = []

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}
"#
    );

    tokio::fs::write(agent_dir.join("config.toml"), config_toml).await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// E2E Test
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Node.js with tsx, PekoHub backend, and MINIMAX_API_KEY"]
async fn test_e2e_tunnel_chat_with_real_llm() {
    // Skip if no API key
    let api_key = match std::env::var("MINIMAX_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            eprintln!("Skipping E2E test: MINIMAX_API_KEY not set");
            return;
        }
    };
    if api_key.is_empty() {
        eprintln!("Skipping E2E test: MINIMAX_API_KEY is empty");
        return;
    }

    // 1. Start PekoHub backend
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();

    // 2. Create temporary workspace with agent config
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let workspace_path = temp_dir.path();
    let agent_name = "e2e-test-agent";

    create_test_workspace(workspace_path, agent_name)
        .await
        .expect("Failed to create test workspace");

    // 3. Build AppState with real services
    let config = DaemonConfigSnapshot {
        data_dir: workspace_path.join("data"),
        config_dir: workspace_path.join("config"),
        log_level: "warn".to_string(),
    };

    let app_state = AppState::with_data_dir(
        workspace_path,
        "127.0.0.1",
        0, // random port — we don't need the HTTP server for this test
        config,
        workspace_path.join("data"),
    )
    .await
    .expect("Failed to build AppState");

    // 4. Create user + runtime record in PekoHub BEFORE starting tunnel
    //    so that owner resolution works when instances are announced
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()
        .unwrap();

    let user_resp = client
        .post(format!("{}/test/create-user", backend.url))
        .json(&serde_json::json!({
            "external_id": "e2e-test-user",
            "provider": "github",
            "namespace": "e2etestuser",
            "display_name": "E2E Test User",
            "email": "e2e@test.com"
        }))
        .send()
        .await
        .expect("Failed to create test user");
    assert!(user_resp.status().is_success(), "Test user creation failed");
    let user_body: serde_json::Value = user_resp.json().await.unwrap();
    let user_id = user_body["id"].as_i64().expect("No user id") as i32;

    // Insert runtime record for owner resolution
    let runtime_resp = client
        .post(format!("{}/test/create-runtime", backend.url))
        .json(&serde_json::json!({
            "runtime_did": did,
            "owner_id": user_id,
            "display_name": "E2E Test Runtime"
        }))
        .send()
        .await
        .expect("Failed to create runtime");
    assert!(runtime_resp.status().is_success(), "Runtime creation failed");

    // Generate JWT for authenticated requests
    let jwt_token = generate_jwt(user_id as i64, "e2etestuser");
    let auth_header = format!("Bearer {jwt_token}");

    // 5. Write tunnel credentials to the default location (~/.peko/pekohub.toml)
    // so that start_tunnel() can find them
    let cred_path = pekobot::tunnel::PekoHubCredential::default_path();
    tokio::fs::create_dir_all(cred_path.parent().unwrap())
        .await
        .unwrap();

    let cred = pekobot::tunnel::PekoHubCredential {
        url: backend.ws_url.clone(),
        runtime_id: did.clone(),
        private_key: BASE64.encode(signing_key.to_bytes()),
    };
    cred.save_to_file(&cred_path).expect("Failed to save credentials");

    // Clean up credential file after test
    let _cleanup = scopeguard::guard(cred_path.clone(), |p| {
        let _ = std::fs::remove_file(&p);
    });

    // 6. Start tunnel
    let tunnel_started = app_state
        .start_tunnel()
        .await
        .expect("Failed to start tunnel");
    assert!(tunnel_started, "Tunnel should have started (credentials exist)");

    // Give tunnel time to connect and announce
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 7. Find the announced instance
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
        !instances.is_empty(),
        "Agent should have been announced. Got: {:?}",
        instances
    );

    let instance_id = instances[0]["id"].as_str().unwrap().to_string();

    // 8. Send chat request via HTTP and consume SSE
    let chat_resp = client
        .post(format!("{}/v1/instances/{instance_id}/chat", backend.url))
        .header("Authorization", &auth_header)
        .json(&serde_json::json!({
            "message": "Say exactly 'Peko tunnel works' and nothing else."
        }))
        .send()
        .await
        .expect("Failed to send chat request");

    assert_eq!(
        chat_resp.status(),
        200,
        "Chat request failed: {:?}",
        chat_resp.text().await.unwrap_or_default()
    );

    // Consume SSE stream
    let body_text = chat_resp.text().await.expect("Failed to read response body");
    let mut chunks: Vec<String> = Vec::new();
    let mut full_text = String::new();

    for line in body_text.lines() {
        if line.starts_with("data:") {
            let data = line[5..].trim();
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(chunk) = event.get("chunk").and_then(|c| c.as_str()) {
                    chunks.push(chunk.to_string());
                    full_text.push_str(chunk);
                }
                if event.get("done").and_then(|d| d.as_bool()) == Some(true) {
                    break;
                }
            }
        }
    }

    // Verify we got a real response
    assert!(
        !full_text.is_empty(),
        "Expected non-empty response from LLM. SSE body:\n{body_text}"
    );
    assert!(
        full_text.to_lowercase().contains("peko")
            || full_text.to_lowercase().contains("tunnel")
            || full_text.to_lowercase().contains("works"),
        "Expected response to contain keywords. Got: {full_text}"
    );

    // Test completes successfully if we reach here
}
