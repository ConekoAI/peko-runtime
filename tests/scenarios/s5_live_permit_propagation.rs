//! End-to-end user-journey scenario for [issue #16] — live
//! `peko agent permit` / `peko agent revoke` propagation.
//!
//! # Scope
//!
//! Regression for [ConekoAI/peko-runtime#16][issue-16]: the agent permit
//! CLI used to write the grant to `~/.peko/agents/<name>/config.toml`
//! but never pushed a fresh `exposure_update` to PekoHub, so PekoHub's
//! `canChat` ACL stayed stale until the daemon restarted. The fix
//! (see `src/ipc/server.rs` `AgentGrantPermission` /
//! `AgentRevokePermission` handlers, and
//! `src/tunnel/dispatcher.rs::refresh_instance_allowed_users`) makes
//! the IPC handler call the dispatcher's `refresh_instance_allowed_users`
//! after the local config write, which sends an `exposure_update` with
//! the freshly-derived `allowed_user_ids` to PekoHub. PekoHub's
//! `handleExposureUpdate` re-broadcasts the change via
//! `instance_announce`; PekoHub's `canChat` reads the new
//! `allowedUsers`. PekoHub's `handleExposureUpdate` also updates
//! PekoHub's instance record; PekoHub's `canChat` (`backend/src/services/instances.ts:339-345`)
//! then sees the new `allowedUsers`. The runtime's defense-in-depth
//! `instance_state.allowed_users` cache is updated in the same
//! round-trip via `dispatcher::handle_exposure_update`.
//!
//! # What this test asserts
//!
//! - `peko agent permit <agent> <user> chat` issued against a running
//!   daemon (no restart) causes PekoHub to allow that user's chat
//!   within ~1s.
//! - `peko agent revoke <agent> <user> chat` causes PekoHub to deny
//!   that user's chat within ~1s.
//! - The same user can be re-permitted and lose access again, with
//!   the daemon running continuously the whole time.
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
    }
    let llm = std::env::var("MOCK_LLM_URL").ok()?;
    if llm.is_empty() {
        return None;
    }
    Some((hub, llm))
}

/// Write the agent config for the s5 test.
///
/// `owner_id` is set to `"local"` so the CLI's local-socket caller
/// passes the owner check in `agent_service::grant_agent_permission`
/// (the daemon's `AgentCreate` handler at
/// `src/ipc/server.rs:1017` stamps the same default on agent
/// creation, so this matches what a real `peko agent create` would
/// produce). PekoHub, by contrast, resolves the instance owner via the
/// runtime DID registered through `test/create-runtime` — `owner_id`
/// in the agent config is the *permission-grant authority* (who can
/// run `peko agent permit`), not the PekoHub-side instance owner.
///
/// The runtime's `instance_announce` reads `host_runtime_id` to
/// surface the runtime DID, which PekoHub uses to bind the instance
/// to the pre-registered runtime record. `allowedUsers` is derived
/// fresh on every announce from `[[permissions]]`, so leaving it
/// empty here gives PekoHub an `allowedUsers = []` on the first
/// announce.
fn write_agent(cli: &PekoCli, agent_name: &str, mock_llm_url: &str, runtime_did: &str) {
    let agent_dir = cli.peko_dir().join("agents").join(agent_name);
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    let base_url = mock_llm_url.trim_end_matches('/');
    let config_toml = format!(
        r#"version = "3.0"
name = "{agent_name}"
description = "s5 live permit propagation agent"
auto_accept_trusted = false

preferred_provider_id = "mock-llm"
preferred_model_id = "default"
default_timeout_seconds = 60
host_runtime_id = "{runtime_did}"
owner_id = "local"

[extensions]
enabled = []

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}
"#
    );
    std::fs::write(agent_dir.join("config.toml"), &config_toml).expect("write agent config.toml");
    std::fs::write(agent_dir.join("SYSTEM.md"), "").expect("write SYSTEM.md");
}

/// Write the pekohub credential at `<peko_home>/pekohub.toml` so the
/// daemon's `peko daemon start --foreground` auto-starts the tunnel.
fn write_pekohub_credential(
    cli: &PekoCli,
    ws_url: &str,
    did: &str,
    signing_key: &ed25519_dalek::SigningKey,
) {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use secrecy::SecretString;

    let private_key_b64 = BASE64.encode(signing_key.to_bytes());

    // Store the private key in the encrypted vault.
    let vault_path = cli.peko_dir().join("vault.enc");
    let vault = pekobot::common::vault::Vault::with_passphrase(
        &vault_path,
        &SecretString::new("test-tunnel-passphrase".into()),
    )
    .expect("create vault for tunnel credential");
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

/// Run `peko agent permit/revoke` in the test's isolated `HOME` /
/// `PEKO_HOME`. Asserts the exit code is 0 (i.e. the IPC handler
/// accepted the request, including the side-effect call to
/// `refresh_instance_allowed_users`).
fn run_peko_agent_permit(cli: &PekoCli, agent: &str, subject_id: &str, verb: &str) {
    assert!(matches!(verb, "permit" | "revoke"), "verb must be permit|revoke");
    let mut cmd = cli.cmd();
    cmd.arg("agent")
        .arg(verb)
        .arg(agent)
        .arg("--subject")
        .arg(subject_id)
        .arg("--permission")
        .arg("chat");
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `peko agent {verb}`: {e}"));
    assert!(
        output.status.success(),
        "`peko agent {verb} {agent} --subject {subject_id} --permission chat` failed: \
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
/// 1. With no `[[permissions]]` on the agent, a non-owner user is
///    denied (PekoHub `canChat` `allowedUsers` is empty).
/// 2. `peko agent permit <agent> <user> chat` makes that user
///    allowed within ~1s — the IPC handler must push a fresh
///    `exposure_update` to PekoHub.
/// 3. `peko agent revoke <agent> <user> chat` makes that user
///    denied again within ~1s.
/// 4. Re-permitting allows the user again within ~1s.
///
/// Throughout, the daemon is never restarted; only the tunnel
/// `exposure_update` round-trip carries the new ACL.
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

    // 3. Lay down agent config + pekohub credential in an isolated HOME.
    let cli = PekoCli::new();
    let agent_name = "s5_live_permit_agent";
    write_agent(&cli, agent_name, &mock_url, &did);
    write_pekohub_credential(&cli, &backend.ws_url, &did, &signing_key);

    // 4. Start daemon → tunnel → initial `instance_announce` with
    //    `allowed_users = []` (no `[[permissions]]` in config).
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

    // 6. `peko agent permit` — this MUST propagate to PekoHub within 1s.
    run_peko_agent_permit(&cli, agent_name, &grantee_id.to_string(), "permit");
    // Give the tunnel a moment to deliver the `exposure_update` and
    // for PekoHub to apply it. The issue acceptance criteria
    // require "within 1s"; we allow 2s for wall-clock slack in CI.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&grantee_jwt)).await;
    assert_eq!(
        status, 200,
        "grantee should be allowed after `peko agent permit` (issue #16 propagation): body={body}"
    );
    assert!(
        !body.trim().is_empty(),
        "grantee chat body should be non-empty: {body}"
    );

    // 7. `peko agent revoke` — the security-side acceptance
    //    criterion. The previously-allowed user must lose access
    //    within 1s, NOT keep chatting until the daemon restarts.
    run_peko_agent_permit(&cli, agent_name, &grantee_id.to_string(), "revoke");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&grantee_jwt)).await;
    assert_eq!(
        status, 403,
        "grantee should be forbidden after `peko agent revoke` (issue #16 propagation): body={body}"
    );

    // 8. Re-permit — proves the round-trip is symmetric and
    //    repeatable, and that the tunnel + PekoHub stay in sync
    //    across multiple cycles.
    run_peko_agent_permit(&cli, agent_name, &grantee_id.to_string(), "permit");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, body) = post_chat(&client, &backend.url, &instance_id, Some(&grantee_jwt)).await;
    assert_eq!(
        status, 200,
        "grantee should be allowed after second `peko agent permit`: body={body}"
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
    let allowed = instance["data"]["allowedUsers"]
        .as_array()
        .expect("instance.allowedUsers not an array");
    let allowed_ids: Vec<String> = allowed
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    assert!(
        allowed_ids.iter().any(|u| u == &grantee_id.to_string()),
        "PekoHub instance.allowedUsers should contain grantee_id after second permit; got {allowed_ids:?}"
    );

    let _ = grantee_ns;
}
