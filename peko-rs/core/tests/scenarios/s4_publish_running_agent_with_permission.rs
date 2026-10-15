//! Private-principal webchat through an explicitly registered runtime actor.
//!
//! The fixture starts a tunnel without registering local-only principals, then
//! calls `peko register` and waits for the instance identified by its Hub receipt.
//! Hub ownership governs registration administration; recipient runtime chat
//! grants govern access. An explicitly permitted account receives streamed output,
//! while an unauthenticated request receives 401 before it reaches the runtime.
//!
//! Requires PekoHub and the mock LLM (or the existing real-LLM test configuration).

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

/// Write a Principal config under `<peko_home>/principals/<name>/`
/// with a pre-seeded `[[permissions]]` grant, then seed the
/// `mock-llm` provider-catalog entry. On the FIRST `instance_announce`
/// the runtime iterates `PrincipalManager::list_all()`, reads this
/// principal's `config.permissions`, and pushes `allowed_users` to
/// pekohub (see
/// [`src/tunnel/dispatcher.rs:297-323`](../../src/tunnel/dispatcher.rs#L297-L323)).
///
/// Mirrors the pattern in `tests/scenarios/s6_principal_grant_revoke_roundtrip.rs` —
/// `peko create` scaffolds the workspace + identity, then we
/// patch `principal.toml` for the per-test exposure + grant.
///
/// `owner_did` is the runtime's own DID — pekohub's `resolveRuntimeOwner`
/// uses it to bind the principal's instance to the owner created via
/// the `/test/create-runtime` fixture endpoint. PekoHub silently drops
/// `instance_announce` if the runtime record is missing.
///
/// `permitted_user_id` is the pekohub user_id (an integer string)
/// that should be granted Chat permission. May be `None` for tests
/// that only care about the owner path.
fn write_principal_with_perm(
    cli: &PekoCli,
    principal_name: &str,
    mock_llm_url: &str,
    _owner_did: &str,
    permitted_user_id: Option<&str>,
) {
    // A ready fixture avoids a bootstrap sidecar before tunnel credentials
    // exist. The guarded daemon below owns the complete runtime lifecycle.
    common::create_mock_principal(cli, principal_name, mock_llm_url);

    let principal_toml = cli
        .peko_dir()
        .join("principals")
        .join(principal_name)
        .join("principal.toml");
    let raw = std::fs::read_to_string(&principal_toml).expect("read principal.toml");
    let mut cfg: peko_core::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");

    cfg.exposure = peko_auth::Exposure::Private;
    cfg.owner = peko_auth::ActorRef::User("local".into());

    if let Some(uid) = permitted_user_id {
        cfg.permissions.push(peko_auth::PermissionGrant {
            subject: peko_auth::ActorRef::User(uid.into()),
            permission: peko_auth::Permission::Chat,
            granted_at: "2026-01-01T00:00:00Z".to_string(),
            granted_by: peko_auth::ActorRef::User("system".into()),
        });
    }

    std::fs::write(
        &principal_toml,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");
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
    let vault = peko_core::common::vault::Vault::load_with_passphrase(
        &vault_path,
        &SecretString::new(cli.vault_passphrase().into()),
    )
    .expect("load test vault for tunnel credential");
    // The runtime's host identity must match the identity authenticated by its
    // tunnel: principal registration proofs bind both signing identities.
    let key_id = format!("{did}#keys-1");
    vault
        .set_identity_private_key(&key_id, "ed25519-raw-base64", &private_key_b64)
        .expect("store fixture runtime signing key");
    let identity = peko_identity::runtime::RuntimeIdentity {
        runtime_did: did.into(),
        key_id,
        created_at: chrono::Utc::now(),
    };
    let runtime_dir = cli.peko_dir().join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("create fixture runtime identity directory");
    std::fs::write(
        runtime_dir.join("identity.toml"),
        toml::to_string_pretty(&identity).unwrap(),
    )
    .expect("write fixture runtime identity");

    let cred = peko_core::tunnel::PekoHubCredential {
        url: ws_url.to_string(),
        runtime_id: did.to_string(),
        tls: None,
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
    owner_user_id: &str,
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
/// Start Hub access explicitly; local-only principals never auto-register.
fn register_principal(cli: &PekoCli, name: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let output = cli
            .cmd()
            .args(["register", name])
            .output()
            .expect("register principal");
        if output.status.success() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Principal registration failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}

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
        // Transport errors (e.g. `hyper::Error(IncompleteMessage)` — the
        // server closed an idle keep-alive connection while the request
        // was in flight) are RETRYable: since ADR-054 the blocking
        // `principal create` ahead of this poll idles the client for
        // 60s+, making the stale-keep-alive race deterministic. Panicking
        // on the first hiccup turned a poll into a flake; treat transport
        // failures like "not yet announced" and keep polling until the
        // deadline.
        let resp = match client
            .get(format!("{backend_url}/v1/instances"))
            .bearer_auth(owner_jwt)
            .query(&[("runtime_id", did)])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    panic!("list instances transport failed (deadline hit): {e}");
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
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
fn jwt_for_user(user_id: &str, namespace: &str) -> String {
    generate_jwt(user_id, namespace)
}

// ---------------------------------------------------------------------------
// Test 1 (Flow 6a) — owner can chat
// ---------------------------------------------------------------------------

/// An account with an explicit runtime chat grant receives streamed output.
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
    let (owner_id, owner_ns) = create_test_user(&client, &backend.url, "s4_owner_a").await;
    let owner_jwt = jwt_for_user(&owner_id, &owner_ns);
    let owner_key = mint_api_key(&client, &backend.url, &owner_jwt, "s4-owner-key").await;

    // 2. Generate runtime identity (DID + signing key).
    let (did, signing_key) = generate_runtime_identity();

    // 3. Register the runtime with pekohub, owned by `owner_id`.
    register_runtime_with_pekohub(&client, &backend.url, &did, &owner_id).await;

    // 4. Set up the per-CLI HOME: write principal config + pekohub.toml.
    let cli = PekoCli::new();
    let principal_name = "s4_owner_principal";
    write_principal_with_perm(&cli, principal_name, &mock_url, &did, Some(&owner_id));
    write_pekohub_credential(&cli, &backend.ws_url, &did, &signing_key);

    // 5. Start the tunnel, then explicitly register this principal.
    let _daemon = DaemonGuard::spawn(&cli);
    register_principal(&cli, principal_name);

    // 6. Wait for the announced instance to land in pekohub's DB.
    let instance_id = wait_for_announced_instance(
        &client,
        &backend.url,
        &owner_jwt,
        &did,
        Duration::from_secs(30),
    )
    .await;

    // 7. Owner (with JWT) chats → 200 + non-empty body.
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&owner_jwt)).await;
    assert_eq!(status, 200, "owner chat should be 200: body={body}");
    assert!(
        !body.trim().is_empty(),
        "owner chat body should be non-empty: {body}"
    );

    // The `peko login --api-key` path (which mints the API key and
    // stores it locally) is exercised in `s2_extension_registry_roundtrip`.
    // The relay-side auth plugin
    // accepts both JWTs (`Authorization: Bearer <jwt>`) and API keys
    // (`Authorization: Bearer ph_…`) interchangeably — see
    // `pekohub/backend/src/plugins/auth.ts:73-114`. The JWT path
    // above is sufficient evidence that the per-instance ACL allows
    // the owner; the API-key code path is a pekohub auth concern
    // orthogonal to the D4 ACL contract.
    let _ = owner_key;
}

// ---------------------------------------------------------------------------
// Test 2 (Flow 6c) — no auth header → 401
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

    let (owner_id, owner_ns) = create_test_user(&client, &backend.url, "s4_owner_c").await;
    let owner_jwt = jwt_for_user(&owner_id, &owner_ns);

    let (did, signing_key) = generate_runtime_identity();
    register_runtime_with_pekohub(&client, &backend.url, &did, &owner_id).await;

    let cli = PekoCli::new();
    let principal_name = "s4_noauth_principal";
    write_principal_with_perm(&cli, principal_name, &mock_url, &did, None);
    write_pekohub_credential(&cli, &backend.ws_url, &did, &signing_key);

    let _daemon = DaemonGuard::spawn(&cli);
    register_principal(&cli, principal_name);
    let instance_id = wait_for_announced_instance(
        &client,
        &backend.url,
        &owner_jwt,
        &did,
        Duration::from_secs(30),
    )
    .await;

    // No auth header → 401.
    let (status, body) = post_chat(&client, &backend.url, &instance_id, None).await;
    assert_eq!(status, 401, "no-auth chat should be 401: body={body}");
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
    assert_eq!(status, 401, "garbage token should be 401: body={body}");
}
