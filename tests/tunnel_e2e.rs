//! Tunnel End-to-End Integration Test (Layer 3)
//!
//! Full E2E test: runtime daemon → tunnel → PekoHub → HTTP proxy → chat → LLM → SSE stream
//!
//! This test requires:
//!   - Node.js 22+ with tsx installed  (local mode)
//!   - OR a running PekoHub test container (container mode via PEKOHUB_URL)
//!   - For real LLM: MINIMAX_API_KEY environment variable
//!   - For mock LLM: MOCK_LLM_URL environment variable (CI mode)
//!
//! The test:
//!   1. Starts PekoHub backend on a random ephemeral port (or connects to container)
//!   2. Creates a temporary workspace with an agent config
//!   3. Builds a real AppState (with real agent service)
//!   4. Writes tunnel credentials pointing to PekoHub
//!   5. Starts the tunnel via AppState::start_tunnel()
//!   6. Creates a user + runtime record in PekoHub
//!   7. Sends POST /v1/instances/:id/chat via HTTP
//!   8. Consumes SSE and verifies response
//!
//! Run locally with real LLM:
//!   cd peko-runtime
//!   MINIMAX_API_KEY=sk-xxx cargo test --test tunnel_e2e -- --ignored
//!
//! Run in container with mock LLM:
//!   PEKOHUB_URL=http://pekohub-test:3000 MOCK_LLM_URL=http://mock-llm:8080 \
//!     cargo test --test tunnel_e2e -- --ignored

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use pekobot::test_utils::{AppState, DaemonConfigSnapshot};

mod common;
use common::{generate_jwt, generate_runtime_identity, PekohubBackend};

// ---------------------------------------------------------------------------
// Workspace setup
// ---------------------------------------------------------------------------

/// Create a temporary workspace with an agent config
async fn create_test_workspace(
    workspace_dir: &std::path::Path,
    agent_name: &str,
    chat_user_id: &str,
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

    // Determine provider: use mock LLM if MOCK_LLM_URL is set, otherwise minimax.
    // v3 splits provider config out of the agent: the agent carries only
    // soft hints (`preferred_provider_id` / `preferred_model_id`), and the
    // actual base_url + api_key live in the v3 provider catalog at
    // `~/.peko/providers.toml`. We seed the catalog entry here, then write
    // a hint-only agent config.
    //
    // This mirrors `tests/common/agent.rs::seed_mock_provider_in_catalog`
    // + `write_v3_mock_agent` used by the cli_subagent integration suite.
    let preferred_provider_id = if let Ok(mock_llm_url) = std::env::var("MOCK_LLM_URL") {
        if mock_llm_url.is_empty() {
            return Err(anyhow::anyhow!("MOCK_LLM_URL is set but empty"));
        }
        // Seed the v3 catalog with a `mock-llm` entry so the daemon's
        // LlmResolver finds it on first lookup. The api_key
        // `mock-llm-test-key` matches what `PekoCli::cmd` exports as
        // `MOCK_LLM_API_KEY` under the `PEKO_TEST_RESOLVER_BOOTSTRAP=1`
        // headless fallback (CI mode).
        seed_mock_provider_catalog(workspace_dir, &mock_llm_url, "mock-llm-test-key")?;
        "mock-llm"
    } else {
        let api_key = std::env::var("MINIMAX_API_KEY").map_err(|_| {
            anyhow::anyhow!("MINIMAX_API_KEY or MOCK_LLM_URL environment variable not set")
        })?;
        // Seed the v3 catalog with a `minimax` entry pointing at the
        // production Minimax endpoint. The API key is loaded from the
        // OS keychain (or, under PEKO_TEST_RESOLVER_BOOTSTRAP=1, the
        // MINIMAX_API_KEY env var).
        seed_minimax_catalog_entry(workspace_dir, &api_key)?;
        "minimax"
    };

    let config_toml = format!(
        r#"version = "3.0"
name = "{agent_name}"
description = "E2E test agent"
auto_accept_trusted = false
default_timeout_seconds = 60

preferred_provider_id = "{preferred_provider_id}"
preferred_model_id = "default"

[extensions]
enabled = []

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}

# Grant the test user Chat permission so the runtime's private-
# instance ACL (peko-runtime/src/tunnel/dispatcher.rs::
# check_request_allowed) lets the request through. Without this,
# `allowed_users` is computed as empty and the chat is rejected
# with "Forbidden".
[[permissions]]
subject = {{ kind = "user", id = "{chat_user_id}" }}
permission = "chat"
granted_at = "2026-01-01T00:00:00Z"
granted_by = {{ kind = "user", id = "system" }}
"#
    );

    tokio::fs::write(agent_dir.join("config.toml"), config_toml).await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// v3 provider-catalog seeders (PR #44: removed inline [provider] block)
// ---------------------------------------------------------------------------

/// Seed a v3 `mock-llm` catalog entry under `<workspace_dir>/config/`
/// so the daemon's `LlmResolver` finds it on first lookup. The path
/// matches `AppState::with_data_dir(workspace_path, ..., config =
/// { config_dir: workspace_path.join("config"), ... })` — the catalog
/// resolver reads `<config_dir>/providers.toml`.
fn seed_mock_provider_catalog(
    workspace_dir: &std::path::Path,
    mock_llm_url: &str,
    api_key: &str,
) -> anyhow::Result<()> {
    use pekobot::providers::catalog::{
        ApiFormat, ModelInfo, ProviderCatalogEntry, ProviderCatalogFile,
    };
    use std::collections::BTreeMap;

    let config_dir = workspace_dir.join("config");
    std::fs::create_dir_all(&config_dir)?;
    let catalog_path = config_dir.join("providers.toml");
    let base_url = mock_llm_url.trim_end_matches('/').to_string();
    let now = chrono::Utc::now();
    let entry = ProviderCatalogEntry {
        id: "mock-llm".to_string(),
        display_name: "mock-llm".to_string(),
        template_id: None,
        api_format: ApiFormat::OpenaiCompletions,
        base_url,
        default_model_id: "default".to_string(),
        models: vec![ModelInfo {
            id: "default".to_string(),
            display_name: None,
            context_length: None,
            max_output_tokens: None,
            capabilities: vec![],
        }],
        headers: BTreeMap::new(),
        requires_key: true,
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let mut entries = BTreeMap::new();
    entries.insert("mock-llm".to_string(), entry);
    let file = ProviderCatalogFile {
        version: "3.0".to_string(),
        entries,
        default_provider_id: None,
        default_model_id: None,
    };
    let toml = toml::to_string_pretty(&file).expect("serialize catalog");
    std::fs::write(&catalog_path, toml)?;
    // The API key is exposed via env in CI; for local runs the OS
    // keychain holds it. See the env-var fallback in
    // `LlmResolver::resolve_api_key`.
    let _ = api_key;
    Ok(())
}

/// Seed a v3 `minimax` catalog entry under `<workspace_dir>/config/`
/// pointing at the production Minimax endpoint.
fn seed_minimax_catalog_entry(
    workspace_dir: &std::path::Path,
    api_key: &str,
) -> anyhow::Result<()> {
    use pekobot::providers::catalog::{
        ApiFormat, ModelInfo, ProviderCatalogEntry, ProviderCatalogFile,
    };
    use std::collections::BTreeMap;

    let config_dir = workspace_dir.join("config");
    std::fs::create_dir_all(&config_dir)?;
    let catalog_path = config_dir.join("providers.toml");
    let now = chrono::Utc::now();
    let entry = ProviderCatalogEntry {
        id: "minimax".to_string(),
        display_name: "Minimax".to_string(),
        template_id: None,
        api_format: ApiFormat::AnthropicMessages,
        base_url: "https://api.minimaxi.com/anthropic".to_string(),
        default_model_id: "MiniMax-M2.7".to_string(),
        models: vec![ModelInfo {
            id: "MiniMax-M2.7".to_string(),
            display_name: None,
            context_length: None,
            max_output_tokens: None,
            capabilities: vec![],
        }],
        headers: BTreeMap::new(),
        requires_key: true,
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let mut entries = BTreeMap::new();
    entries.insert("minimax".to_string(), entry);
    let file = ProviderCatalogFile {
        version: "3.0".to_string(),
        entries,
        default_provider_id: None,
        default_model_id: None,
    };
    let toml = toml::to_string_pretty(&file).expect("serialize catalog");
    std::fs::write(&catalog_path, toml)?;
    let _ = api_key;
    Ok(())
}

// ---------------------------------------------------------------------------
// E2E Test
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend and LLM (MINIMAX_API_KEY or MOCK_LLM_URL)"]
async fn test_e2e_tunnel_chat_with_llm() {
    // v3 headless bootstrap: this test builds an in-process AppState that
    // uses the OS keychain by default. In CI there is no keychain, so when
    // MOCK_LLM_URL is set we enable the env-var API-key fallback that
    // `PekoCli::cmd` normally exports for the CLI-based integration suites.
    if std::env::var_os("MOCK_LLM_URL").is_some() {
        std::env::set_var("PEKO_TEST_RESOLVER_BOOTSTRAP", "1");
        std::env::set_var("MOCK_LLM_API_KEY", "mock-llm-test-key");
    }

    // 1. Start PekoHub backend
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()
        .unwrap();

    // 2. Create user FIRST so we can put their id into the agent
    //    config's permissions grant — the runtime's
    //    `compute_allowed_user_ids` reads from `config.permissions`,
    //    and without a matching grant the private-instance ACL
    //    rejects the chat with "Forbidden".
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
    let chat_user_id = user_id.to_string();

    // 3. Create temporary workspace with agent config
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let workspace_path = temp_dir.path();
    let agent_name = "e2e-test-agent";

    create_test_workspace(workspace_path, agent_name, &chat_user_id)
        .await
        .expect("Failed to create test workspace");

    // 4. Prepare the credential vault before building AppState so that both
    //    AppState::with_data_dir() and start_tunnel() load the same vault with
    //    the same passphrase. In CI PEKO_MASTER_PASSPHRASE is set; locally we
    //    fall back to the test default.
    let vault_passphrase = std::env::var("PEKO_MASTER_PASSPHRASE")
        .unwrap_or_else(|_| "peko-test-vault-passphrase".to_string());
    let private_key_b64 = BASE64.encode(signing_key.to_bytes());
    let vault_path = workspace_path.join("config").join("vault.enc");
    tokio::fs::create_dir_all(vault_path.parent().unwrap())
        .await
        .unwrap();
    let vault = pekobot::common::vault::Vault::with_passphrase(
        &vault_path,
        &secrecy::SecretString::new(vault_passphrase.clone().into()),
    )
    .expect("create vault for tunnel credential");
    vault
        .set_tunnel_private_key(&did, &private_key_b64)
        .expect("store tunnel private key in vault");

    // 5. Build AppState with real services
    let config = DaemonConfigSnapshot {
        data_dir: workspace_path.join("data"),
        config_dir: workspace_path.join("config"),
        log_level: "warn".to_string(),
    };

    let app_state: AppState = AppState::with_data_dir(
        workspace_path,
        "127.0.0.1",
        0, // random port — we don't need the HTTP server for this test
        config,
        workspace_path.join("data"),
    )
    .await
    .expect("Failed to build AppState");

    // 6. Create runtime record (with owner_id = the user we created above)

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
    assert!(
        runtime_resp.status().is_success(),
        "Runtime creation failed"
    );

    // Generate JWT for authenticated requests
    let jwt_token = generate_jwt(user_id as i64, "e2etestuser");
    let auth_header = format!("Bearer {jwt_token}");

    // 7. Write tunnel credentials to the default location (~/.peko/runtime/pekohub.toml)
    // so that start_tunnel() can find them. The private key lives in the vault
    // at the AppState's config directory.
    let cred_path = pekobot::tunnel::PekoHubCredential::default_path();
    tokio::fs::create_dir_all(cred_path.parent().unwrap())
        .await
        .unwrap();

    let cred = pekobot::tunnel::PekoHubCredential {
        url: backend.ws_url.clone(),
        runtime_id: did.clone(),
    };
    cred.save_to_file(&cred_path)
        .expect("Failed to save credentials");

    // Clean up credential file after test
    let _cleanup = scopeguard::guard(cred_path.clone(), |p| {
        let _ = std::fs::remove_file(&p);
    });

    // 6. Start tunnel (issue #8: cap reconnect attempts at 5 for this test
    // so a misbehaving server surfaces fast)
    let tunnel_started: bool = app_state
        .start_tunnel(5)
        .await
        .expect("Failed to start tunnel");
    assert!(
        tunnel_started,
        "Tunnel should have started (credentials exist)"
    );

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
    // The runtime defaults to Private exposure; pekohub now forwards
    // the authenticated user's id via x-pekohub-user-id so the runtime's
    // defense-in-depth ACL allows the chat through.
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
    let body_text = chat_resp
        .text()
        .await
        .expect("Failed to read response body");
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

    // Verify we got a response
    assert!(
        !full_text.is_empty(),
        "Expected non-empty response from LLM. SSE body:\n{body_text}"
    );
    assert!(
        full_text.to_lowercase().contains("peko")
            || full_text.to_lowercase().contains("tunnel")
            || full_text.to_lowercase().contains("works")
            || full_text.to_lowercase().contains("success"),
        "Expected response to contain keywords. Got: {full_text}"
    );

    // Test completes successfully if we reach here
}
