//! Tunnel End-to-End Integration Test (Layer 3)
//!
//! Full E2E test: runtime daemon → tunnel → PekoHub → HTTP proxy → chat → LLM → SSE stream
//!
//! Originally `tests/tunnel_e2e.rs`. Moved inline as part of F9.4 so the
//! `peko::test_utils` / `peko::daemon::state::AppState` exposure can narrow
//! once this test reaches `crate::daemon::state::AppState` instead of the
//! external `peko::test_utils` re-export. The test is gated by
//! `--features test-utils` (declared at the mod site in `src/daemon/mod.rs`)
//! so it only builds and runs when the user opts in.
//!
//! This test requires:
//!   - Node.js 22+ with tsx installed  (local mode)
//!   - OR a running PekoHub test container (container mode via PEKOHUB_URL)
//!   - For real LLM: MINIMAX_API_KEY environment variable
//!   - For mock LLM: MOCK_LLM_URL environment variable (CI mode)
//!
//! The test:
//!   1. Starts PekoHub backend on a random ephemeral port (or connects to container)
//!   2. Creates a temporary workspace with a Principal config
//!   3. Builds a real AppState (which loads the Principal via PrincipalManager)
//!   4. Writes tunnel credentials pointing to PekoHub
//!   5. Starts the tunnel via AppState::start_tunnel()
//!   6. Creates a user + runtime record in PekoHub
//!   7. Sends POST /v1/instances/:id/chat via HTTP
//!   8. Consumes SSE and verifies response
//!
//! ## Principal-era translation
//!
//! After the "Principal as the single actor" migration, `TunnelDispatcher::announce_instances`
//! iterates `PrincipalManager::list_all()` — there are no agent instances to announce anymore
//! (see `src/tunnel/dispatcher.rs:296-345`). The runtime's canonical chat surface is
//! `peko send <principal>`, and the inbound PekoHub-proxied chat is routed to
//! `PrincipalManager::receive`. This test therefore bootstraps a Principal on disk
//! at `<workspace>/config/principals/<name>/principal.toml` so `AppState::with_data_dir`
//! picks it up during the `read_dir` loop in `src/daemon/state.rs:425-440`, and the
//! announcer finds it via `principal_manager.list_all()`.
//!
//! Permission grants live on `PrincipalConfig.permissions` (replacing the legacy
//! `[[permissions]]` block at the agent config level). The user's `chat` permission
//! is what lets the proxied request through the dispatcher's `check_request_allowed`
//! defense-in-depth ACL.
//!
//! Run locally with real LLM:
//!   cd peko-runtime
//!   MINIMAX_API_KEY=sk-xxx cargo test --lib --features test-utils tunnel_e2e -- --ignored
//!
//! Run in container with mock LLM:
//!   PEKOHUB_URL=http://pekohub-test:3000 MOCK_LLM_URL=http://mock-llm:8080 \
//!     cargo test --lib --features test-utils tunnel_e2e -- --ignored

use std::time::Duration;

use crate::common::vault::Vault;
use crate::daemon::state::{AppState, DaemonConfigSnapshot};
use crate::tunnel::PekoHubCredential;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

// Helpers used to live in `tests/common/{crypto,auth,harness}.rs`. They are
// reused from there via `#[path = ...]` so this test file doesn't have to
// duplicate their bodies; other integration tests still use the same
// `tests/common/` versions.
#[path = "../../../tests/common/crypto.rs"]
mod crypto_helper;
use crypto_helper::generate_runtime_identity;

#[path = "../../../tests/common/auth.rs"]
mod auth_helper;
use auth_helper::generate_jwt;

#[allow(clippy::manual_assert)]
#[path = "../../../tests/common/harness.rs"]
mod harness_helper;
use harness_helper::PekohubBackend;

// ---------------------------------------------------------------------------
// Workspace setup
// ---------------------------------------------------------------------------

/// Create a temporary workspace with a Principal config on disk so
/// `AppState::with_data_dir` picks it up via the `read_dir` loop in
/// `src/daemon/state.rs:425-440` and `PrincipalManager::list_all()`
/// surfaces it to the tunnel announce loop.
///
/// On-disk layout (post-Principal migration):
///   `<workspace>/config/principals/<name>/principal.toml`
///
/// The Principal carries the same `chat` permission grant that the legacy
/// agent config used — without it the dispatcher's private-instance
/// ACL (`compute_allowed_user_ids` → `allowed_users`) is empty and the
/// proxied chat request is rejected with `Forbidden`.
async fn create_test_workspace(
    workspace_dir: &std::path::Path,
    principal_name: &str,
    chat_user_id: &str,
) -> anyhow::Result<()> {
    let config_dir = workspace_dir.join("config");
    let data_dir = workspace_dir.join("data");
    let cache_dir = workspace_dir.join("cache");

    tokio::fs::create_dir_all(&config_dir).await?;
    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&cache_dir).await?;

    // Determine the model: use mock LLM if MOCK_LLM_URL is set, otherwise
    // minimax. Model-first splits endpoint config out of the actor's
    // identity: the actual base_url + api_key live in the model catalog
    // at `<config_dir>/models.toml`, and the principal config pins the
    // configured model via `preferred_model_id` — there is no runtime
    // default, so an unpinned principal fails resolution with
    // "no model configured".
    //
    // This mirrors `tests/common/agent.rs::seed_mock_provider_in_catalog`
    // + `create_mock_principal_with_tools` used by the cli_subagent /
    // cli_extensions_l3 integration suites.
    let pinned_model_id = if let Ok(mock_llm_url) = std::env::var("MOCK_LLM_URL") {
        if mock_llm_url.is_empty() {
            return Err(anyhow::anyhow!("MOCK_LLM_URL is set but empty"));
        }
        // Seed the catalog with a `mock-llm` entry so the daemon's
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
        // Seed the catalog with a `minimax` entry pointing at the
        // production Minimax endpoint. The API key is loaded from the
        // OS keychain (or, under PEKO_TEST_RESOLVER_BOOTSTRAP=1, the
        // MINIMAX_API_KEY env var).
        seed_minimax_catalog_entry(workspace_dir, &api_key)?;
        "minimax"
    };

    // Create the Principal's directory + principal.toml. The Principal's
    // identity is generated lazily by `PrincipalManager::load` if the
    // `did` field is absent, so we don't need to provision an ed25519
    // key here — the manager takes care of it.
    let principals_dir = config_dir.join("principals");
    let principal_path = principals_dir.join(principal_name);
    tokio::fs::create_dir_all(&principal_path).await?;

    // TOML key-order trap: top-level scalar keys after a `[section]` block
    // are interpreted as belonging to the most recently opened sub-table,
    // not the root. So `exposure = "private"` placed after `[identity]`
    // or `[capabilities]` is silently absorbed by the wrong table and
    // the field falls back to its `#[default]` (Unexposed) — PekoHub
    // then rejects every chat with 503 before the user-permission check.
    // Place all root-level keys (`exposure`, `description`) BEFORE any
    // `[section]` header.
    let principal_toml = format!(
        r#"name = "{principal_name}"
description = "E2E test principal"

# PekoHub's `canChat` short-circuits to 503 Service Unavailable when
# `instance.exposure === "unexposed"`. `InstanceExposure` defaults to
# Unexposed (`src/tunnel/protocol.rs:27-28`), so we must pin it to
# Private here — otherwise the announce goes out as `unexposed` and
# PekoHub rejects every chat attempt before the user/grant check.
exposure = "private"

# Model-first: the principal must pin a configured model from
# `<config_dir>/models.toml` — the resolver has no runtime default
# and fails unpinned calls with "no model configured".
preferred_model_id = "{pinned_model_id}"

[identity]
display_name = "E2E Test Principal"

[capabilities]
grants = []

# Grant the test user Chat permission so the runtime's private-
# instance ACL (peko-runtime/src/tunnel/dispatcher.rs::
# compute_allowed_user_ids) lets the request through. Without this,
# `allowed_users` is empty and the chat is rejected with "Forbidden".
[[permissions]]
subject = {{ kind = "user", id = "{chat_user_id}" }}
permission = "chat"
granted_at = "2026-01-01T00:00:00Z"
granted_by = {{ kind = "user", id = "system" }}
"#
    );

    tokio::fs::write(principal_path.join("principal.toml"), principal_toml).await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// v3 provider-catalog seeders (PR #44: removed inline [provider] block)
// ---------------------------------------------------------------------------

/// Seed a `mock-llm` catalog entry under `<workspace_dir>/config/`
/// so the daemon's `LlmResolver` finds it on first lookup. The path
/// matches `AppState::with_data_dir(workspace_path, ..., config =
/// { config_dir: workspace_path.join("config"), ... })` — the catalog
/// resolver reads `<config_dir>/models.toml`.
fn seed_mock_provider_catalog(
    workspace_dir: &std::path::Path,
    mock_llm_url: &str,
    api_key: &str,
) -> anyhow::Result<()> {
    use crate::providers::catalog::{ApiFormat, ModelCatalogFile, ModelConfig};
    use std::collections::BTreeMap;

    let config_dir = workspace_dir.join("config");
    std::fs::create_dir_all(&config_dir)?;
    let catalog_path = config_dir.join("models.toml");
    let base_url = mock_llm_url.trim_end_matches('/').to_string();
    let now = chrono::Utc::now();
    let entry = ModelConfig {
        id: "mock-llm".to_string(),
        display_name: "mock-llm".to_string(),
        template_id: None,
        api_format: ApiFormat::OpenaiCompletions,
        base_url,
        model_id: "default".to_string(),
        context_window: None,
        max_output_tokens: None,
        headers: BTreeMap::new(),
        credential_id: None,
        requires_key: true,
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let mut entries = BTreeMap::new();
    entries.insert("mock-llm".to_string(), entry);
    let file = ModelCatalogFile {
        version: "4.0".to_string(),
        entries,
    };
    let toml = toml::to_string_pretty(&file).expect("serialize catalog");
    std::fs::write(&catalog_path, toml)?;
    // The API key is exposed via env in CI; for local runs the OS
    // keychain holds it. See the env-var fallback in
    // `LlmResolver::resolve_api_key`.
    let _ = api_key;
    Ok(())
}

/// Seed a `minimax` catalog entry under `<workspace_dir>/config/`
/// pointing at the production Minimax endpoint.
fn seed_minimax_catalog_entry(
    workspace_dir: &std::path::Path,
    api_key: &str,
) -> anyhow::Result<()> {
    use crate::providers::catalog::{ApiFormat, ModelCatalogFile, ModelConfig};
    use std::collections::BTreeMap;

    let config_dir = workspace_dir.join("config");
    std::fs::create_dir_all(&config_dir)?;
    let catalog_path = config_dir.join("models.toml");
    let now = chrono::Utc::now();
    let entry = ModelConfig {
        id: "minimax".to_string(),
        display_name: "Minimax".to_string(),
        template_id: None,
        api_format: ApiFormat::AnthropicMessages,
        base_url: "https://api.minimaxi.com/anthropic".to_string(),
        model_id: "MiniMax-M3".to_string(),
        context_window: None,
        max_output_tokens: None,
        headers: BTreeMap::new(),
        credential_id: None,
        requires_key: true,
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let mut entries = BTreeMap::new();
    entries.insert("minimax".to_string(), entry);
    let file = ModelCatalogFile {
        version: "4.0".to_string(),
        entries,
    };
    let toml = toml::to_string_pretty(&file).expect("serialize catalog");
    std::fs::write(&catalog_path, toml)?;
    let _ = api_key;
    Ok(())
}

// ---------------------------------------------------------------------------
// E2E Test
// ---------------------------------------------------------------------------

// Production daemon runs on a multi-threaded Tokio runtime; the
// `SystemPromptBuilder` uses `tokio::task::block_in_place` to drive
// ExtensionCore hooks synchronously, which requires a multi-threaded
// runtime. `#[tokio::test]` defaults to `current_thread`, which panics
// with "can call blocking only when running on the multi-threaded
// runtime" — pin this test to the production flavor.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires PekoHub backend and LLM (MINIMAX_API_KEY or MOCK_LLM_URL)"]
async fn test_e2e_tunnel_chat_with_llm() {
    // v3 headless bootstrap: this test builds an in-process AppState that
    // uses the OS keychain by default. In CI there is no keychain, so we
    // ALWAYS enable the env-var API-key fallback (for both MOCK and real
    // LLM modes). Without it, `LlmResolver::build_provider` returns Ok(None)
    // and the agent runs without a provider — see `daemon/state.rs:387`
    // for the bootstrap read path.
    std::env::set_var("PEKO_TEST_RESOLVER_BOOTSTRAP", "1");
    if std::env::var_os("MOCK_LLM_URL").is_some() {
        std::env::set_var("MOCK_LLM_API_KEY", "mock-llm-test-key");
    }

    // 1. Start PekoHub backend
    let backend = PekohubBackend::start().await;
    let (did, signing_key) = generate_runtime_identity();

    let client = reqwest::Client::builder()
        // Real-LLM chat calls (minimax) routinely take 5-15s end-to-end
        // (provider round-trip + SSE start). 10s was tight enough that
        // any provider hiccup caused the chat request to time out before
        // the first SSE chunk arrived (flaked in CI run #28307811834).
        // The agent's own LLM timeout is 5 minutes, so this is well
        // under that ceiling while leaving room for slow providers.
        .timeout(Duration::from_mins(1))
        .no_proxy()
        .build()
        .unwrap();

    // 2. Create user FIRST so we can put their id into the agent
    //    config's permissions grant — the runtime's
    //    `compute_allowed_user_ids` reads from `config.permissions`,
    //    and without a matching grant the private-instance ACL
    //    rejects the chat with "Forbidden".
    //
    //    external_id must be unique per run: the PekoHub test backend
    //    persists user records across runs (no test-time reset), and a
    //    hardcoded id collides with leftover state from prior runs.
    //    Suffix with a process-unique nonce (random u64) — the DID itself
    //    isn't safe to use directly since it contains `:` which some
    //    PekoHub columns reject.
    let run_tag = format!("{:016x}", rand::random::<u64>());
    let user_resp = client
        .post(format!("{}/test/create-user", backend.url))
        .json(&serde_json::json!({
            "external_id": format!("e2e-test-user-{run_tag}"),
            "provider": "github",
            "namespace": format!("e2etestuser{run_tag}"),
            "display_name": "E2E Test User",
            "email": "e2e@test.com"
        }))
        .send()
        .await
        .expect("Failed to create test user");
    assert!(
        user_resp.status().is_success(),
        "Test user creation failed (status={}): {}",
        user_resp.status(),
        user_resp.text().await.unwrap_or_default(),
    );
    let user_body: serde_json::Value = user_resp.json().await.unwrap();
    let user_id = user_body["id"].as_i64().expect("No user id") as i32;
    let chat_user_id = user_id.to_string();

    // 3. Create temporary workspace with Principal config
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let workspace_path = temp_dir.path();
    let principal_name = "e2e-test-principal";

    create_test_workspace(workspace_path, principal_name, &chat_user_id)
        .await
        .expect("Failed to create test workspace");

    // 4. Prepare the credential vault before building AppState so that both
    //    AppState::with_data_dir() and start_tunnel() load the same vault with
    //    the same passphrase. In CI PEKO_MASTER_PASSPHRASE is set; locally we
    //    fall back to the test default.
    let vault_passphrase = std::env::var("PEKO_MASTER_PASSPHRASE")
        .unwrap_or_else(|_| "peko-test-vault-passphrase".to_string());
    std::env::set_var("PEKO_MASTER_PASSPHRASE", &vault_passphrase);
    let private_key_b64 = BASE64.encode(signing_key.to_bytes());
    let vault_path = workspace_path.join("config").join("vault.enc");
    tokio::fs::create_dir_all(vault_path.parent().unwrap())
        .await
        .unwrap();
    let vault = Vault::with_passphrase(
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
        launch_mode: crate::daemon::LaunchMode::Headless,
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
    let jwt_token = generate_jwt(user_id as i64, &format!("e2etestuser{run_tag}"));
    let auth_header = format!("Bearer {jwt_token}");

    // 7. Write tunnel credentials next to the AppState config directory so
    // that start_tunnel() can find them. The private key lives in the vault
    // at the AppState's config directory.
    let config_dir = workspace_path.join("config");
    let cred_path = PekoHubCredential::path_for_config_dir(&config_dir);
    tokio::fs::create_dir_all(cred_path.parent().unwrap())
        .await
        .unwrap();

    let cred = PekoHubCredential {
        url: backend.ws_url.clone(),
        runtime_id: did.clone(),
        tls: None,
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
        "Principal should have been announced. Got: {:?}",
        instances
    );

    let instance_id = instances[0]["id"].as_str().unwrap().to_string();

    // 8. Send chat request via HTTP and consume SSE
    // The runtime defaults to Private exposure; pekohub now forwards
    // the authenticated user's id via x-pekohub-user-id so the runtime's
    // defense-in-depth ACL allows the chat through.

    // Under the mock LLM, force a deterministic MULTI-WORD response so the
    // streaming assertion below is meaningful. The mock streams its answer
    // word-by-word, so a multi-word phrase arrives as multiple SSE chunks
    // iff genuine token streaming is working end-to-end. (The default mock
    // response in CI is the single word "SUCCESS", which would stream as
    // one chunk regardless, and so could not distinguish streaming from a
    // single buffered chunk.)
    //
    // We drive this through `MOCK_LLM_SCRIPT` (re-read per request by the
    // mock) rather than `DEFAULT_RESPONSE` (bound once at mock startup, so
    // /_test/configure can't change it). The script maps a substring of
    // our prompt to the multi-word reply, leaving every other prompt on
    // the unchanged default.
    let mock_multi_word = "Peko tunnel works";
    if let Some(mock_url) = std::env::var_os("MOCK_LLM_URL") {
        let mock_url = mock_url.to_string_lossy().to_string();
        let script = serde_json::json!({ mock_multi_word: mock_multi_word }).to_string();
        let cfg = client
            .post(format!(
                "{}/_test/configure",
                mock_url.trim_end_matches('/')
            ))
            .json(&serde_json::json!({ "MOCK_LLM_SCRIPT": script }))
            .send()
            .await
            .expect("Failed to configure mock LLM");
        assert!(
            cfg.status().is_success(),
            "Mock LLM /_test/configure failed (status={})",
            cfg.status()
        );
    }

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
        if let Some(rest) = line.strip_prefix("data:") {
            let data = rest.trim();
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

    // Regression guard for the double-encoding bug: each streamed chunk
    // must be RAW assistant text, not a JSON object. If the runtime ever
    // re-wraps chunks as `{"chunk":..,"done":..}` again, that inner JSON
    // would leak into `full_text` here.
    assert!(
        !full_text.contains("\"done\"") && !full_text.contains("\"chunk\""),
        "Streamed chunks should be raw text, not JSON-wrapped. Got: {full_text}"
    );

    // Streaming coverage: under the mock LLM we configured a multi-word
    // response above, which the mock streams word-by-word. A genuinely
    // streamed answer therefore arrives as multiple SSE chunks. If the
    // runtime regressed to buffering the whole answer into a single chunk
    // (the old `streaming: false` behaviour, or the `!streamed_any`
    // fallback firing because no deltas were forwarded), we'd see exactly
    // one chunk here. Only asserted under the mock, whose chunk count is
    // deterministic; a real provider may chunk differently.
    if std::env::var_os("MOCK_LLM_URL").is_some() {
        assert!(
            chunks.len() >= 2,
            "Expected multiple streamed chunks (real token streaming), got {}: {:?}",
            chunks.len(),
            chunks
        );
    }

    // Test completes successfully if we reach here
}
