//! End-to-end user-journey scenario for [issue #16] — live
//! `peko principal permit` / `peko principal revoke` propagation.
//!
//! # Scope
//!
//! Regression for [ConekoAI/peko-runtime#16][issue-16]: the agent permit
//! CLI used to write the grant to `~/.peko/agents/<name>/config.toml`
//! but never re-announced the instance to PekoHub, so PekoHub's
//! `canChat` ACL stayed stale until the daemon restarted. The fix
//! (see `src/ipc/server.rs` `PrincipalGrantPermission` /
//! `PrincipalRevokePermission` handlers, and
//! `src/tunnel/dispatcher.rs::refresh_instance_allowed_principals`) makes
//! the IPC handler call the dispatcher's `refresh_instance_allowed_principals`
//! after the local config write, which re-announces the instance with
//! the freshly-derived `allowed_user_ids` to PekoHub. PekoHub treats
//! `instance_announce` as an upsert, updating `allowedUsers` in its
//! instance record; PekoHub's `canChat` (`backend/src/services/instances.ts:339-345`)
//! then sees the new `allowedUsers`. The runtime's defense-in-depth
//! `instance_state.allowed_users` cache is updated in the same
//! round-trip by `announce_single_instance`.
//!
//! (Runtime-originated `exposure_update` messages are hub-to-runtime
//! only in the current PekoHub tunnel protocol, so we use
//! `instance_announce` for the runtime-to-hub direction.)
//!
//! # What this test asserts
//!
//! - `peko principal permit <principal> <subject> chat` issued against
//!   a running daemon (no restart) causes PekoHub to allow that user's
//!   chat within ~1s.
//! - `peko principal revoke <principal> <subject> chat` causes PekoHub
//!   to deny that user's chat within ~1s.
//! - The same user can be re-permitted and lose access again, with the
//!   daemon running continuously the whole time.
//!
//! # Principal-era surface
//!
//! After the "Principal as the single actor" migration, the standalone
//! agent ACL CLI (`peko agent permit/revoke`, `peko agent show`) is
//! gone; the Principal is the actor that owns permissions. The
//! equivalent live-propagation contract is exercised via
//! `peko principal permit <principal> <subject> <permission>` — the
//! handler chain (`PrincipalGrantPermission` IPC →
//! `PrincipalManager::update_config` →
//! `TunnelDispatcher::refresh_instance_allowed_principals` →
//! `compute_allowed_user_ids` → fresh `instance_announce`) is the
//! direct successor of the old agent path.
//!
//! PekoHub-side `allowedUsers` derivation lives in
//! `src/tunnel/dispatcher.rs::compute_allowed_user_ids`: only
//! `Permission::Chat` grants with `SubjectKind::User` subjects
//! contribute, and the `user:` prefix is stripped.
//!
//! # Mock-LLM tier
//!
//! Same setup as the s4 scenario: `PekohubBackend` brings up a local
//! PekoHub + mock LLM, `PekoCli` gives the test an isolated
//! `HOME`/`PEKO_HOME`, and `DaemonGuard` spawns the daemon with that
//! environment. The test early-returns on missing `PEKOHUB_URL` /
//! `MOCK_LLM_URL` so a bare `cargo test` still passes.
//!
//! [issue-16]: https://github.com/ConekoAI/peko-runtime/issues/16

#[path = "../common/mod.rs"]
mod common;
use common::{
    create_test_user, generate_jwt, generate_runtime_identity, reset_pekohub, DaemonGuard, PekoCli,
    PekohubBackend,
};
use serial_test::serial;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Fixture wiring (mirrors s4's helpers — kept local to avoid a fragile
// `pub(crate)` surface in tests/common).
// ---------------------------------------------------------------------------

/// Read `PEKOHUB_URL` and `MOCK_LLM_URL` env. Returns `Some((hub, llm))`
/// only when both are set and non-empty.
fn hub_and_llm_urls() -> Option<(String, String)> {
    let hub = std::env::var("PEKOHUB_URL").ok()?;
    if hub.is_empty() {
        return None;
    };
    let llm = std::env::var("MOCK_LLM_URL").ok()?;
    if llm.is_empty() {
        return None;
    };
    Some((hub, llm))
}

/// Write the Principal config for the s5 test.
///
/// The Principal's `owner` is set to `"user:local"` so the local-socket
/// caller passes the `ManageSettings` owner-check in the
/// `PrincipalGrantPermission` handler. (`peko principal create` defaults
/// to `owner = "user:default"`, which matches the `peko send` CLI
/// caller, but not the local-socket caller that the daemon-presented
/// CLI in this scenario uses — see also `s6_principal_grant_revoke_roundtrip`'s
/// `create_principal` helper for the same rewrite.)
///
/// The runtime's `instance_announce` derives `allowedUsers` fresh on
/// every announce from `[[permissions]]`, so leaving it empty here
/// gives PekoHub an `allowedUsers = []` on the first announce.
fn write_principal(cli: &PekoCli, principal_name: &str, mock_llm_url: &str) {
    // `peko principal create` writes the workspace, identity, default
    // `agents/primary.md` prompt, and `principal.toml` — the daemon's
    // `load_principal` requires this scaffold. It runs before
    // `DaemonGuard::spawn`, so seed the `mock-llm` catalog entry here
    // too (idempotent — the guard re-seeds the same values on spawn)
    // and pin the principal to it: create requires `--model` and
    // validates it against the catalog.
    common::agent::seed_mock_provider_in_catalog(cli.home(), mock_llm_url);
    let output = cli
        .cmd()
        .args(["principal", "create", principal_name, "--model", "mock-llm"])
        .output()
        .expect("run `peko principal create`");
    assert!(
        output.status.success(),
        "`peko principal create {principal_name}` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Rewrite the owner to `user:local` so the local-socket caller
    // passes the `ManageSettings` owner-check in the grant handler.
    let principal_toml = cli
        .peko_dir()
        .join("principals")
        .join(principal_name)
        .join("principal.toml");
    let raw = std::fs::read_to_string(&principal_toml).expect("read principal.toml");
    let mut cfg: peko::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    cfg.owner = peko_auth::Subject::User("local".into());
    std::fs::write(
        &principal_toml,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");
}

/// Write the pekohub credential at `<peko_home>/runtime/pekohub.toml` so
/// the daemon's `peko daemon start --foreground` auto-starts the tunnel.
fn write_pekohub_credential(
    cli: &PekoCli,
    ws_url: &str,
    did: &str,
    signing_key: &ed25519_dalek::SigningKey,
) {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use secrecy::SecretString;

    let private_key_b64 = BASE64.encode(signing_key.to_bytes());

    // Store the private key in the encrypted vault. The vault was already
    // created by PekoCli with its own passphrase, so load it explicitly
    // rather than creating a new one with a different passphrase.
    let vault_path = cli.peko_dir().join("vault.enc");
    let vault = peko::common::vault::Vault::load_with_passphrase(
        &vault_path,
        &SecretString::new(cli.vault_passphrase().into()),
    )
    .expect("load test vault for tunnel credential");
    vault
        .set_tunnel_private_key(did, &private_key_b64)
        .expect("store tunnel private key in vault");

    let cred = peko::tunnel::PekoHubCredential {
        url: ws_url.to_string(),
        runtime_id: did.to_string(),
        tls: None,
    };
    let path = cli.peko_dir().join("runtime").join("pekohub.toml");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create runtime dir");
    cred.save_to_file(&path).expect("save pekohub.toml");
}

/// Pre-register the runtime with PekoHub so the announce isn't
/// dropped (see the comment in s4 for the full story).
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
            "display_name": format!("s5-runtime-{}", &did[..24.min(did.len())]),
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

/// Wait up to `timeout` for at least one announced instance to appear
/// in PekoHub for the given runtime DID.
async fn wait_for_announced_instance(
    client: &reqwest::Client,
    backend_url: &str,
    owner_jwt: &str,
    did: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
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
        if let Some(arr) = body["data"].as_array() {
            if !arr.is_empty() {
                return arr[0]["id"]
                    .as_str()
                    .expect("instance[0].id not a string")
                    .to_string();
            }
        }
        last_body = serde_json::to_string(&body).unwrap_or_default();
        if Instant::now() >= deadline {
            panic!(
                "runtime did not announce any instances in {timeout:?}\n\
                 --- last /v1/instances body ---\n{last_body}\n--- end ---"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Issue a chat POST and return `(status, body)`.
async fn post_chat(
    client: &reqwest::Client,
    backend_url: &str,
    instance_id: &str,
    auth: Option<&str>,
) -> (u16, String) {
    let mut req = client
        .post(format!("{backend_url}/v1/instances/{instance_id}/chat"))
        .json(&serde_json::json!({ "message": "say SUCCESS" }));
    if let Some(token) = auth {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.expect("chat POST transport failed");
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    (status, body)
}

/// Run `peko principal permit/revoke` in the test's isolated `HOME`/
/// `PEKO_HOME`. Asserts the exit code is 0 (i.e. the IPC handler
/// accepted the request, including the side-effect call to
/// `refresh_instance_allowed_principals`).
///
/// After the Principal-as-single-actor migration, the agent ACL CLI is
/// gone — the equivalent live-propagation contract is
/// `peko principal permit|revoke <principal> <subject> <permission>`
/// (positional). The CLI handler parses `subject` via
/// `subject_from_string_with_default_user` (so a bare numeric user id
/// becomes `Subject::User("local")` after the prefix default), and
/// `permission` via `parse_permission` (so `"chat"` becomes
/// `Permission::Chat`). Both forms are accepted positionally.
fn run_peko_principal_permit(cli: &PekoCli, principal: &str, subject_id: &str, verb: &str) {
    assert!(
        matches!(verb, "permit" | "revoke"),
        "verb must be permit|revoke"
    );
    let output = cli
        .cmd()
        .args(["principal", verb, principal, subject_id, "chat"])
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `peko principal {verb}`: {e}"));
    assert!(
        output.status.success(),
        "`peko principal {verb} {principal} {subject_id} chat` failed: \
         exit={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

// ---------------------------------------------------------------------------
// Test — live permit/revoke propagation (regression for issue #16)
// ---------------------------------------------------------------------------

/// Asserts that:
/// 1. With no `[[permissions]]` on the Principal, a non-owner user is
///    denied (PekoHub `canChat` `allowedUsers` is empty).
/// 2. `peko principal permit <principal> <user> chat` makes that user
///    allowed within ~1s — the IPC handler must push a fresh
///    `instance_announce` to PekoHub.
/// 3. `peko principal revoke <principal> <user> chat` makes that user
///    denied again within ~1s.
/// 4. Re-permitting allows the user again within ~1s.
///
/// Throughout, the daemon is never restarted; only the tunnel
/// `instance_announce` round-trip carries the new ACL.
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn permit_revoke_propagates_to_pekohub_within_1s() {
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

    // 1. Create two users. `owner` is the PekoHub-side instance owner
    //    (set via `test/create-runtime`); `grantee` is the user we
    //    permit/revoke via the CLI. PekoHub's `canChat` short-circuits
    //    on `instance.ownerId === userId`, so we test `grantee`
    //    exclusively — its access is governed solely by `allowedUsers`.
    let (owner_id, owner_ns) = create_test_user(&client, &backend.url, "s5_owner").await;
    let owner_jwt = generate_jwt(owner_id, &owner_ns);

    let (grantee_id, grantee_ns) = create_test_user(&client, &backend.url, "s5_grantee").await;
    let grantee_jwt = generate_jwt(grantee_id, &grantee_ns);

    // 2. Generate runtime identity; register with PekoHub as `owner`.
    let (did, signing_key) = generate_runtime_identity();
    register_runtime_with_pekohub(&client, &backend.url, &did, owner_id).await;

    // 3. Lay down Principal config + pekohub credential in an isolated HOME.
    let cli = PekoCli::new();
    let principal_name = "s5_live_permit_principal";
    write_principal(&cli, principal_name, &mock_url);
    write_pekohub_credential(&cli, &backend.ws_url, &did, &signing_key);

    // 4. Start daemon → tunnel → initial `instance_announce` with
    //    `allowed_users = []` (no `[[permissions]]` in `principal.toml`).
    let _daemon = DaemonGuard::spawn(&cli);
    let instance_id = wait_for_announced_instance(
        &client,
        &backend.url,
        &owner_jwt,
        &did,
        Duration::from_secs(30),
    )
    .await;

    // 5. Baseline: grantee is NOT in allowedUsers → 403.
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&grantee_jwt)).await;
    assert_eq!(
        status, 403,
        "grantee should be forbidden before any permit: body={body}"
    );

    // 6. `peko principal permit` — this MUST propagate to PekoHub within 1s.
    run_peko_principal_permit(&cli, principal_name, &grantee_id.to_string(), "permit");
    // Give the tunnel a moment to deliver the fresh `instance_announce`
    // and for PekoHub to apply it. The issue acceptance criteria
    // require "within 1s"; we allow 2s for wall-clock slack in CI.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&grantee_jwt)).await;
    assert_eq!(
        status, 200,
        "grantee should be allowed after `peko principal permit` (issue #16 propagation): body={body}"
    );
    assert!(
        !body.trim().is_empty(),
        "grantee chat body should be non-empty: {body}"
    );

    // 7. `peko principal revoke` — the security-side acceptance
    //    criterion. The previously-allowed user must lose access
    //    within 1s, NOT keep chatting until the daemon restarts.
    run_peko_principal_permit(&cli, principal_name, &grantee_id.to_string(), "revoke");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&grantee_jwt)).await;
    assert_eq!(
        status, 403,
        "grantee should be forbidden after `peko principal revoke` (issue #16 propagation): body={body}"
    );

    // 8. Re-permit — proves the round-trip is symmetric and
    //    repeatable, and that the tunnel + PekoHub stay in sync
    //    across multiple cycles.
    run_peko_principal_permit(&cli, principal_name, &grantee_id.to_string(), "permit");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&grantee_jwt)).await;
    assert_eq!(
        status, 200,
        "grantee should be allowed after second `peko principal permit`: body={body}"
    );

    // Sanity: pekohub's `canChat` `allowedUsers` reflects the live
    // grant by reading the public instance record via owner JWT.
    let resp = client
        .get(format!("{}/v1/instances/{}", backend.url, instance_id))
        .bearer_auth(&owner_jwt)
        .send()
        .await
        .expect("instance GET transport failed");
    assert!(resp.status().is_success(), "instance GET non-2xx");
    let instance: serde_json::Value = resp.json().await.unwrap_or_default();
    let allowed = instance["allowedPrincipals"]
        .as_array()
        .expect("instance.allowedPrincipals not an array");
    let allowed_ids: Vec<String> = allowed
        .iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect();
    assert!(
        allowed_ids.iter().any(|u| u == &grantee_id.to_string()),
        "PekoHub instance.allowedPrincipals should contain grantee_id after second permit; got {allowed_ids:?}"
    );

    let _ = grantee_ns;
}
