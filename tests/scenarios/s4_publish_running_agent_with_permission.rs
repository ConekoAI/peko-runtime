//! End-to-end user-journey scenario D4 (Phase D slice per
//! `docs/integration/TESTING.md` §7).
//!
//! Coverage — flow 6 from the Phase D plan: author publishes a running
//! agent behind the PekoHub tunnel, and the per-instance ACL on the
//! relay (`canChat` in `pekohub/backend/src/services/instances.ts`)
//! gates who can chat.
//!
//! | Rust test                                                  | Flow step                                                  |
//! |------------------------------------------------------------|------------------------------------------------------------|
//! | `permit_owner_can_chat`                                   | Flow 6a: owner hits `/v1/instances/:id/chat` → 200          |
//! | `permit_granted_user_chats_ungranted_forbidden`           | Flow 6b: granted user → 200, ungranted user → 403           |
//! | `no_auth_returns_401`                                     | Flow 6c: no `Authorization` header → 401                   |
//!
//! ## Scope
//!
//! **Mock-LLM tier.** PekoHub is the long-lived fixture container
//! (or local node+tsx process) — `make docker-up` brings it up.
//! The pekohub DB is reset at the top of every test with
//! `reset_pekohub` so cross-test user/runtime collisions are
//! eliminated.
//!
//! All tests early-return if `PEKOHUB_URL` or `MOCK_LLM_URL` is
//! unset, so a bare `cargo test` still passes.
//!
//! ## The structural facts this file relies on
//!
//! 1. **The runtime auto-starts the tunnel inside `peko daemon start
//!    --foreground` if `~/.peko/pekohub.toml` exists.** The
//!    `PekoCli::cmd()` builder sets `HOME = <tempdir>` (see
//!    [`tests/common/cli.rs:109-115`](../common/cli.rs#L109-L115)),
//!    and `PekoHubCredential::default_path()` returns
//!    `dirs::home_dir().join(".peko/pekohub.toml")` (see
//!    [`src/tunnel/credential.rs:62-66`](../../src/tunnel/credential.rs#L62-L66)).
//!    So writing the credential to `<cli.peko_dir()>/pekohub.toml`
//!    and spawning the daemon triggers the tunnel connect
//!    automatically. This is the same shape
//!    `tests/tunnel_e2e.rs:219-241` uses (in-process via
//!    `AppState::start_tunnel()`), but here we exercise the full
//!    `peko daemon start --foreground` code path to match the
//!    production startup sequence.
//! 2. **Tunnel → PekoHub announce.** On tunnel connect the runtime
//!    sends an `instance_announce` for every local agent with the
//!    `allowed_users` resolved from `agent.config.permissions`
//!    (see
//!    [`src/tunnel/dispatcher.rs:160-205`](../../src/tunnel/dispatcher.rs#L160-L205)
//!    and
//!    [`compute_allowed_user_ids`](../src/tunnel/dispatcher.rs:133-157)).
//!    PekoHub's `handleInstanceAnnounce` (at
//!    [`pekohub/backend/src/services/tunnel-manager.ts:385-421`](../../../pekohub/backend/src/services/tunnel-manager.ts#L385-L421))
//!    resolves the runtime DID to an owner via `resolveRuntimeOwner`;
//!    **the runtime record must already exist in pekohub's `runtimes`
//!    table** (created here via the `/test/create-runtime` fixture
//!    endpoint), otherwise the announce is silently dropped.
//! 3. **The per-instance ACL is enforced server-side at
//!    `pekohub/backend/src/services/instances.ts:339-345`**
//!    (`canChat`) and the chat route at
//!    `pekohub/backend/src/routes/api/instances.ts:545-607` — owner
//!    or any user in `allowedUsers` is allowed; everyone else gets
//!    403. Missing auth on a private instance returns 401.
//! 4. **The runtime's instance_id is stable per (runtime_did, agent)
//!    pair** — see `TunnelDispatcher::instance_id` at
//!    [`src/tunnel/dispatcher.rs:123-131`](../../src/tunnel/dispatcher.rs#L123-L131),
//!    which uses a UUID v5 namespace. We don't precompute it; we
//!    discover the instance via `GET /v1/instances?runtime_id=<did>`
//!    and look it up by runtime_id (one instance per runtime, since
//!    the test only creates one agent).
//! 5. **`peko agent permit` propagates to PekoHub within ~1s.** As of
//!    the fix for [issue #16](https://github.com/ConekoAI/peko-runtime/issues/16),
//!    the `AgentGrantPermission` and `AgentRevokePermission` IPC
//!    handlers call
//!    [`TunnelDispatcher::refresh_instance_allowed_users`](../src/tunnel/dispatcher.rs)
//!    after the local config write, which sends an `exposure_update`
//!    to PekoHub with `allowed_user_ids` re-derived from the new
//!    `AgentConfig.permissions`. PekoHub's `canChat` ACL (and the
//!    runtime's defense-in-depth cache) refreshes from that. The
//!    D4 test still pre-seeds the config before daemon start, but
//!    the live `permit`/`revoke` path is now covered by
//!    `tests/scenarios/s5_*.rs` (regression for #16).
//!
//! ## What the test asserts
//!
//! The relay-side ACL outcome — 200 for owner, 200 for permitted, 403
//! for unpermitted, 401 for unauthenticated. The LLM is incidental;
//! the mock LLM just needs to echo a non-empty response so we can
//! distinguish a 200-with-content from a tunnel-routing failure.

#[path = "../common/mod.rs"]
mod common;
use common::{
    create_test_user, generate_jwt, generate_runtime_identity, reset_pekohub, DaemonGuard, PekoCli,
    PekohubBackend,
};
use serial_test::serial;
use std::path::PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `PEKOHUB_URL` and `MOCK_LLM_URL` env. Returns Some(urls) only
/// when both are set and non-empty. Tests early-return on None so a
/// bare `cargo test` on a checkout without the docker-compose stack
/// still passes.
fn hub_and_llm_urls() -> Option<(String, String)> {
    let hub = std::env::var("PEKOHUB_URL").ok()?;
    if hub.is_empty() {
        return None;
    }
    let llm = std::env::var("MOCK_LLM_URL").ok()?;
    if llm.is_empty() {
        return None;
    }
    Some((hub, llm))
}

/// Mint a PekoHub API key for the user identified by `jwt`.
async fn mint_api_key(
    client: &reqwest::Client,
    backend_url: &str,
    jwt: &str,
    name: &str,
) -> String {
    let resp = client
        .post(format!("{backend_url}/v1/auth/api-keys"))
        .bearer_auth(jwt)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .expect("api-key POST transport failed");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "api-key mint failed: status={status}, body={body}"
    );
    let v: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("api-key response not JSON: {e}; body={body}"));
    v["key"]
        .as_str()
        .unwrap_or_else(|| panic!("api-key response missing `key`: {body}"))
        .to_string()
}

/// Write the agent config with a pre-seeded `[[permissions]]` grant.
/// On the FIRST `instance_announce` the runtime reads this and
/// pushes `allowed_users` to pekohub (see
/// [`src/tunnel/dispatcher.rs:160-205`](../../src/tunnel/dispatcher.rs#L160-L205)).
///
/// `owner_did` is the runtime's own DID — the runtime uses it as the
/// `host_runtime_id` and `owner_id` fields (which the runtime also
/// reads at announce time when looking up its own identity in the
/// tunnel).
///
/// `permitted_user_id` is the pekohub user_id (an integer string)
/// that should be granted Chat permission. May be `None` for tests
/// that only care about the owner path.
fn write_agent_with_perm(
    cli: &PekoCli,
    agent_name: &str,
    mock_llm_url: &str,
    owner_did: &str,
    permitted_user_id: Option<&str>,
) -> PathBuf {
    let agent_dir = cli.peko_dir().join("agents").join(agent_name);
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");

    let _base_url = mock_llm_url.trim_end_matches('/');

    // The `[[permissions]]` block is at the top level of the agent
    // config; the runtime's `AgentConfig::permissions` deserializer
    // reads it directly. After ADR-039, `subject` and `granted_by`
    // are `Principal` (kind/id structs). See
    // [`src/auth/principal.rs`](../src/auth/principal.rs) for the
    // schema.
    let perm_block = match permitted_user_id {
        Some(uid) => format!(
            r#"
# Pre-seeded grant for the permitted user — picked up by the
# runtime's first `instance_announce` and pushed to pekohub as
# `allowedUsers`. Format mirrors
# [`tests/tunnel_e2e.rs:114-125`](../tunnel_e2e.rs#L114-L125).
[[permissions]]
subject = {{ kind = "user", id = "{uid}" }}
permission = "chat"
granted_at = "2026-01-01T00:00:00Z"
granted_by = {{ kind = "user", id = "system" }}
"#
        ),
        None => String::new(),
    };

    let config_toml = format!(
        r#"version = "3.0"
name = "{agent_name}"
description = "D4 publish-with-permission agent"
auto_accept_trusted = false

preferred_provider_id = "mock-llm"
preferred_model_id = "default"
default_timeout_seconds = 60
host_runtime_id = "{owner_did}"
owner_id = "{owner_did}"

[extensions]
enabled = []

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}
{perm_block}"#
    );

    let config_path = agent_dir.join("config.toml");
    std::fs::write(&config_path, &config_toml).expect("write agent config.toml");
    std::fs::write(agent_dir.join("SYSTEM.md"), "").expect("write SYSTEM.md");
    config_path
}

/// Write `pekohub.toml` at `<cli.peko_dir()>/pekohub.toml`. The
/// daemon's `peko daemon start --foreground` reads this via
/// `PekoHubCredential::default_path()` (which resolves to
/// `<HOME>/.peko/pekohub.toml`); the daemon subprocess inherits
/// `HOME = <cli.home()>` from `PekoCli::cmd()`.
fn write_pekohub_credential(
    cli: &PekoCli,
    ws_url: &str,
    did: &str,
    signing_key: &ed25519_dalek::SigningKey,
) -> PathBuf {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use secrecy::SecretString;

    let private_key_b64 = BASE64.encode(signing_key.to_bytes());

    // Store the private key in the encrypted vault. The vault was already
    // created by PekoCli with its own passphrase, so load it explicitly
    // rather than creating a new one with a different passphrase.
    let vault_path = cli.peko_dir().join("vault.enc");
    let vault = pekobot::common::vault::Vault::load_with_passphrase(
        &vault_path,
        &SecretString::new(cli.vault_passphrase().into()),
    )
    .expect("load test vault for tunnel credential");
    vault
        .set_tunnel_private_key(did, &private_key_b64)
        .expect("store tunnel private key in vault");

    let cred = pekobot::tunnel::PekoHubCredential {
        url: ws_url.to_string(),
        runtime_id: did.to_string(),
    };
    let path = cli.peko_dir().join("runtime").join("pekohub.toml");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create runtime dir");
    cred.save_to_file(&path).expect("save pekohub.toml");
    path
}

/// Register a runtime record with pekohub. PekoHub's
/// `handleInstanceAnnounce` calls `resolveRuntimeOwner(runtime_id)`
/// and **silently drops the announce** if no record exists (see
/// `pekohub/backend/src/services/tunnel-manager.ts:389-398`).
/// The runtime record binds a runtime DID to a pekohub user_id (the
/// owner).
async fn register_runtime_with_pekohub(
    client: &reqwest::Client,
    backend_url: &str,
    did: &str,
    owner_user_id: i64,
) {
    let resp = client
        .post(format!("{backend_url}/test/create-runtime"))
        .json(&serde_json::json!({
            "runtime_did": did,
            "owner_id": owner_user_id,
            "display_name": format!("d4-runtime-{did_short}", did_short = &did[..24.min(did.len())]),
        }))
        .send()
        .await
        .expect("test/create-runtime POST transport failed");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "test/create-runtime failed: status={status}, body={body}"
    );
}

/// Poll `GET /v1/instances?runtime_id=<did>` for up to 30s, waiting
/// for the runtime's `instance_announce` to land in pekohub's DB.
/// Returns the FIRST instance id (we only have one agent in the
/// test, so there's only one).
async fn wait_for_announced_instance(
    client: &reqwest::Client,
    backend_url: &str,
    owner_jwt: &str,
    did: &str,
    timeout: Duration,
) -> String {
    let deadline = std::time::Instant::now() + timeout;
    // The first loop iteration unconditionally overwrites this
    // sentinel; the `panic!` at the deadline reads it. The
    // `#[allow]` quiets the "value assigned is never read" warning
    // (the compiler doesn't see the panic read since it never
    // returns).
    #[allow(unused_assignments)]
    let mut last_body = String::from("<never received>");
    loop {
        let resp = client
            .get(format!("{backend_url}/v1/instances"))
            .bearer_auth(owner_jwt)
            .query(&[("runtime_id", did)])
            .send()
            .await
            .expect("list instances transport failed");
        assert!(
            resp.status().is_success(),
            "list instances non-2xx: status={} body={:?}",
            resp.status(),
            resp.text().await.unwrap_or_default(),
        );
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let arr = body["data"]
            .as_array()
            .expect("instances response missing `data` array");
        if !arr.is_empty() {
            let instance_id = arr[0]["id"]
                .as_str()
                .expect("instance[0].id not a string")
                .to_string();
            return instance_id;
        }
        last_body = serde_json::to_string(&body).unwrap_or_default();
        if std::time::Instant::now() >= deadline {
            panic!(
                "runtime did not announce any instances in {timeout:?}\n\
                 --- last /v1/instances body ---\n{last_body}\n--- end ---"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Issue a chat request against `POST /v1/instances/:id/chat` with
/// the given Authorization header value (pass `None` to omit auth).
/// Returns the response status and a body text string (SSE chunks
/// joined with `\n` if the response is text/event-stream, or the raw
/// JSON body otherwise). We don't try to fully parse SSE — the test
/// just needs to distinguish "200 with content" from "200 empty" /
/// "401" / "403".
async fn post_chat(
    client: &reqwest::Client,
    backend_url: &str,
    instance_id: &str,
    auth: Option<&str>,
) -> (u16, String) {
    let mut req = client
        .post(format!("{backend_url}/v1/instances/{instance_id}/chat"))
        .json(&serde_json::json!({
            "message": "say SUCCESS"
        }));
    if let Some(token) = auth {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.expect("chat POST transport failed");
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    (status, body)
}

/// Mint a JWT signed with the fixture's secret (`peko auth`'s
/// `Authorization: Bearer <jwt>` path). Used so the pekohub
/// `authenticate` plugin can look up the user by `decoded.sub` (see
/// `pekohub/backend/src/plugins/auth.ts:122`).
fn jwt_for_user(user_id: i64, namespace: &str) -> String {
    generate_jwt(user_id, namespace)
}

// ---------------------------------------------------------------------------
// Test 1 (Flow 6a) — owner can chat
// ---------------------------------------------------------------------------

/// Flow 6a (positive): the owner of the runtime, sending a chat
/// request to their own announced instance, gets a 200 with a
/// non-empty body. The owner is allowed by the `canChat`
/// `instance.ownerId === userId` short-circuit (see
/// `pekohub/backend/src/services/instances.ts:343`), which is why
/// we do NOT need to pre-seed the agent config with a `[[permissions]]`
/// grant for the owner — the owner is always allowed.
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn permit_owner_can_chat() {
    let Some((_hub_url, mock_url)) = hub_and_llm_urls() else {
        eprintln!("PEKOHUB_URL or MOCK_LLM_URL not set; skipping");
        return;
    };

    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()
        .unwrap();

    // 1. Create owner user in pekohub.
    let (owner_id, owner_ns) =
        create_test_user(&client, &backend.url, "s4_owner_a").await;
    let owner_jwt = jwt_for_user(owner_id, &owner_ns);
    let owner_key = mint_api_key(&client, &backend.url, &owner_jwt, "s4-owner-key").await;

    // 2. Generate runtime identity (DID + signing key).
    let (did, signing_key) = generate_runtime_identity();

    // 3. Register the runtime with pekohub, owned by `owner_id`.
    register_runtime_with_pekohub(&client, &backend.url, &did, owner_id).await;

    // 4. Set up the per-CLI HOME: write agent config + pekohub.toml.
    let cli = PekoCli::new();
    let agent_name = "s4_owner_agent";
    write_agent_with_perm(&cli, agent_name, &mock_url, &did, None);
    write_pekohub_credential(&cli, &backend.ws_url, &did, &signing_key);

    // 5. Start the daemon. It reads `pekohub.toml` from $HOME and
    //    auto-starts the tunnel, which on connect sends
    //    `instance_announce` for every local agent.
    let _daemon = DaemonGuard::spawn(&cli);

    // 6. Wait for the announced instance to land in pekohub's DB.
    let instance_id =
        wait_for_announced_instance(&client, &backend.url, &owner_jwt, &did, Duration::from_secs(30))
            .await;

    // 7. Owner (with JWT) chats → 200 + non-empty body.
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&owner_jwt)).await;
    assert_eq!(status, 200, "owner chat should be 200: body={body}");
    assert!(
        !body.trim().is_empty(),
        "owner chat body should be non-empty: {body}"
    );

    // The `peko login --api-key` path (which mints the API key and
    // stores it locally) is exercised in `s2_extension_registry_roundtrip`
    // and `s3_agent_registry_roundtrip`. The relay-side auth plugin
    // accepts both JWTs (`Authorization: Bearer <jwt>`) and API keys
    // (`Authorization: Bearer ph_…`) interchangeably — see
    // `pekohub/backend/src/plugins/auth.ts:73-114`. The JWT path
    // above is sufficient evidence that the per-instance ACL allows
    // the owner; the API-key code path is a pekohub auth concern
    // orthogonal to the D4 ACL contract.
    let _ = owner_key;
}

// ---------------------------------------------------------------------------
// Test 2 (Flow 6b) — granted user chats, ungranted user gets 403
// ---------------------------------------------------------------------------

/// Flow 6b (positive + negative): a user pre-seeded in
/// `agent.config.permissions` is allowed to chat (200), and a user
/// not in `allowedUsers` is rejected (403) by pekohub's `canChat`
/// ACL at `pekohub/backend/src/services/instances.ts:339-345`.
///
/// We pre-seed the agent config (rather than calling
/// `peko agent permit`) because the runtime's `grant_agent_permission`
/// path writes to disk but does not push an `exposure_update` over
/// the tunnel (see the docstring at the top of this file, point 5).
/// The first `instance_announce` is the only point at which the
/// runtime pushes `allowedUsers` to pekohub.
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn permit_granted_user_chats_ungranted_forbidden() {
    let Some((_hub_url, mock_url)) = hub_and_llm_urls() else {
        eprintln!("PEKOHUB_URL or MOCK_LLM_URL not set; skipping");
        return;
    };

    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()
        .unwrap();

    // 1. Create three users: owner + granted + ungranted.
    let (owner_id, owner_ns) =
        create_test_user(&client, &backend.url, "s4_owner_b").await;
    let owner_jwt = jwt_for_user(owner_id, &owner_ns);

    let (granted_id, granted_ns) =
        create_test_user(&client, &backend.url, "s4_granted").await;
    let granted_jwt = jwt_for_user(granted_id, &granted_ns);

    let (ungranted_id, ungranted_ns) =
        create_test_user(&client, &backend.url, "s4_ungranted").await;
    let ungranted_jwt = jwt_for_user(ungranted_id, &ungranted_ns);

    // 2. Generate runtime identity.
    let (did, signing_key) = generate_runtime_identity();

    // 3. Register the runtime with pekohub.
    register_runtime_with_pekohub(&client, &backend.url, &did, owner_id).await;

    // 4. Set up the per-CLI HOME: agent config pre-seeds the granted
    //    user in `[[permissions]]`; the ungranted user is NOT in
    //    the config.
    let cli = PekoCli::new();
    let agent_name = "s4_acl_agent";
    write_agent_with_perm(
        &cli,
        agent_name,
        &mock_url,
        &did,
        Some(&granted_id.to_string()),
    );
    write_pekohub_credential(&cli, &backend.ws_url, &did, &signing_key);

    // 5. Start daemon → tunnel → instance_announce carries
    //    `allowed_users = [<granted_id>]` to pekohub.
    let _daemon = DaemonGuard::spawn(&cli);

    // 6. Wait for the announced instance.
    let instance_id =
        wait_for_announced_instance(&client, &backend.url, &owner_jwt, &did, Duration::from_secs(30))
            .await;

    // 7. Granted user → 200 (in allowedUsers).
    let (status, body) =
        post_chat(&client, &backend.url, &instance_id, Some(&granted_jwt)).await;
    assert_eq!(
        status, 200,
        "granted user should be allowed (in allowedUsers): body={body}"
    );
    assert!(
        !body.trim().is_empty(),
        "granted user chat body should be non-empty: {body}"
    );

    // 8. Ungranted user → 403 (not owner, not in allowedUsers).
    let (status, body) =
        post_chat(&client, &backend.url, &instance_id, Some(&ungranted_jwt)).await;
    assert_eq!(
        status, 403,
        "ungranted user should be forbidden: body={body}"
    );
    // Pekohub's chat route returns `{ error: "Forbidden" }` on 403
    // (see `pekohub/backend/src/routes/api/instances.ts:573`).
    assert!(
        body.to_lowercase().contains("forbidden"),
        "403 body should contain 'forbidden': {body}",
    );
}

// ---------------------------------------------------------------------------
// Test 3 (Flow 6c) — no auth header → 401
// ---------------------------------------------------------------------------

/// Flow 6c (negative): a chat request with no `Authorization`
/// header to a private instance returns 401. The pekohub chat route
/// at `pekohub/backend/src/routes/api/instances.ts:553-567` calls
/// `fastify.authenticate(request)` inside a try-catch on private
/// exposure and returns 401 on the catch.
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn no_auth_returns_401() {
    let Some((_hub_url, mock_url)) = hub_and_llm_urls() else {
        eprintln!("PEKOHUB_URL or MOCK_LLM_URL not set; skipping");
        return;
    };

    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()
        .unwrap();

    let (owner_id, owner_ns) =
        create_test_user(&client, &backend.url, "s4_owner_c").await;
    let owner_jwt = jwt_for_user(owner_id, &owner_ns);

    let (did, signing_key) = generate_runtime_identity();
    register_runtime_with_pekohub(&client, &backend.url, &did, owner_id).await;

    let cli = PekoCli::new();
    let agent_name = "s4_noauth_agent";
    write_agent_with_perm(&cli, agent_name, &mock_url, &did, None);
    write_pekohub_credential(&cli, &backend.ws_url, &did, &signing_key);

    let _daemon = DaemonGuard::spawn(&cli);
    let instance_id =
        wait_for_announced_instance(&client, &backend.url, &owner_jwt, &did, Duration::from_secs(30))
            .await;

    // No auth header → 401.
    let (status, body) = post_chat(&client, &backend.url, &instance_id, None).await;
    assert_eq!(
        status, 401,
        "no-auth chat should be 401: body={body}"
    );
    // The pekohub chat route returns `{ error: "Authentication required" }`
    // on 401 (see `pekohub/backend/src/routes/api/instances.ts:566`).
    assert!(
        body.to_lowercase().contains("unauthor") || body.to_lowercase().contains("auth"),
        "401 body should mention auth: {body}",
    );

    // Sanity: an invalid Bearer token also gets 401.
    let (status, body) = post_chat(
        &client,
        &backend.url,
        &instance_id,
        Some("not-a-valid-jwt-or-api-key"),
    )
    .await;
    assert_eq!(
        status, 401,
        "garbage token should be 401: body={body}"
    );
}
