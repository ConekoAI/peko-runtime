//! End-to-end scenario for capability grant / revoke / list round trips.
//!
//! # Scope
//!
//! `CapabilityGrant` / `CapabilityRevoke` / `CapabilityList` are the IPC
//! packets behind `peko capability grant|revoke|list`. They mutate the
//! principal's `[capabilities] grants` array in `principal.toml` via
//! `PrincipalManager::update_config` — the same single-write-lock authority
//! path every other capability touch uses.
//!
//! This scenario covers three concerns:
//!
//! 1. **IPC round trip** — `DaemonClient::capability_grant` /
//!    `capability_revoke` / `capability_list` actually persist and
//!    surface the change. Distinct from the existing `s6_*` tests, which
//!    cover the *permission* (subject ACL) wire shape, not the
//!    capability grants.
//! 2. **CLI round trip** — shelling out to the real `peko capability`
//!    binary produces the same on-disk result; no in-process shortcuts.
//! 3. **No-op revoke surface** — `CapabilityRevoked.removed` is `false`
//!    when the cap was never granted (literal absent AND no wildcard
//!    covered it), pinning the new field added by the IPC consistency
//!    remediation.
//!
//! Unlike `s6_principal_grant_revoke_roundtrip.rs`, none of these tests
//! post-create patch `principal.toml` to swap the owner: the
//! `CapabilityGrant`/`CapabilityRevoke` handlers do not consult the
//! principal's `owner` field, only its existence.

#[path = "../common/mod.rs"]
mod common;
use common::{agent, DaemonGuard, PekoCli};
use serial_test::serial;
use std::time::Duration;

use peko_core::ipc::packet::ResponsePacket;
use peko_core::ipc::DaemonClient;

// ---------------------------------------------------------------------------
// Fixture wiring
// ---------------------------------------------------------------------------

/// Create a Principal under the test's isolated `<HOME>/.peko` by invoking
/// the real `peko principal create` command. Seeding the `mock-llm`
/// catalog entry is required because `principal create` validates the
/// `--model` argument against the catalog.
fn create_principal(cli: &PekoCli, name: &str) {
    let mock_url = std::env::var_os("MOCK_LLM_URL")
        .map(|u| u.to_string_lossy().into_owned())
        .unwrap_or_else(|| "http://127.0.0.1:9/v1".to_string());
    agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);
    let output = cli
        .cmd()
        .args(["principal", "create", name, "--model", "mock-llm"])
        .output()
        .expect("run `peko principal create`");
    assert!(
        output.status.success(),
        "`peko principal create {name}` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Read the granted capability strings from disk.
fn read_capabilities_from_disk(cli: &PekoCli, name: &str) -> Vec<String> {
    let path = cli
        .peko_dir()
        .join("principals")
        .join(name)
        .join("principal.toml");
    let raw = std::fs::read_to_string(&path).expect("read principal.toml");
    let cfg: peko_core::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    cfg.capabilities.to_strings()
}

/// Issue a `CapabilityGrant` and assert the daemon reports it.
async fn grant_capability(client: &DaemonClient, principal: &str, capability: &str) {
    let resp = client
        .capability_grant(principal, capability)
        .await
        .expect("capability_grant succeeds");
    assert!(
        matches!(resp, ResponsePacket::CapabilityGranted { .. }),
        "expected CapabilityGranted, got: {resp:?}"
    );
}

/// Issue a `CapabilityRevoke` and return the daemon's `removed` flag.
async fn revoke_capability(client: &DaemonClient, principal: &str, capability: &str) -> bool {
    let resp = client
        .capability_revoke(principal, capability)
        .await
        .expect("capability_revoke succeeds");
    match resp {
        ResponsePacket::CapabilityRevoked { removed, .. } => removed,
        other => panic!("expected CapabilityRevoked, got: {other:?}"),
    }
}

/// Issue a `CapabilityList` and return the structured `granted` slice.
async fn list_granted(client: &DaemonClient, principal: &str) -> Vec<String> {
    let resp = client
        .capability_list(principal)
        .await
        .expect("capability_list succeeds");
    match resp {
        ResponsePacket::CapabilityList { granted, .. } => granted,
        other => panic!("expected CapabilityList, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Drive the full grant → list → revoke → list cycle through the IPC
/// `DaemonClient`. Asserts the change is visible in both the structured
/// response and the on-disk `principal.toml`.
#[tokio::test]
#[serial]
async fn s7_capability_grant_list_revoke_round_trips_via_ipc() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let name = "s7-capability-ipc";
    create_principal(&cli, name);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    // starter_bundle() includes `tool:Read`; assert it survives the round
    // trip and that our added cap (`tool:TodoWrite`) lands in `granted`.
    let granted_before = list_granted(&client, name).await;
    assert!(
        granted_before.iter().any(|c| c == "tool:Read"),
        "starter bundle should include tool:Read; granted={granted_before:?}"
    );

    grant_capability(&client, name, "tool:TodoWrite").await;
    let granted_after_grant = list_granted(&client, name).await;
    assert!(
        granted_after_grant.iter().any(|c| c == "tool:TodoWrite"),
        "post-grant list should include tool:TodoWrite; granted={granted_after_grant:?}"
    );
    assert!(
        granted_after_grant.iter().any(|c| c == "tool:Read"),
        "starter bundle must survive an unrelated grant; granted={granted_after_grant:?}"
    );
    let on_disk_after_grant = read_capabilities_from_disk(&cli, name);
    assert!(
        on_disk_after_grant.iter().any(|c| c == "tool:TodoWrite"),
        "post-grant principal.toml should include tool:TodoWrite; on_disk={on_disk_after_grant:?}"
    );

    let removed = revoke_capability(&client, name, "tool:TodoWrite").await;
    let granted_after_revoke = list_granted(&client, name).await;
    let on_disk_after_revoke = read_capabilities_from_disk(&cli, name);
    assert!(
        removed,
        "revoke should report removed=true for a literal grant"
    );

    assert!(
        !granted_after_revoke.iter().any(|c| c == "tool:TodoWrite"),
        "post-revoke list must drop tool:TodoWrite; granted={granted_after_revoke:?}"
    );
    assert!(
        !on_disk_after_revoke.iter().any(|c| c == "tool:TodoWrite"),
        "post-revoke principal.toml must drop tool:TodoWrite; on_disk={on_disk_after_revoke:?}"
    );
}

/// Drive the cycle by shelling out to the real `peko capability`
/// binary, then verify the on-disk result. This proves the CLI surface
/// agrees with the IPC surface without any in-process shortcuts.
#[tokio::test]
#[serial]
async fn s7_capability_cli_grant_revoke_round_trips_via_subprocess() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let name = "s7-capability-cli";
    create_principal(&cli, name);

    let _guard = DaemonGuard::spawn(&cli);

    let grant_out = cli
        .cmd()
        .args(["capability", "grant", "--principal", name, "tool:Grep"])
        .output()
        .expect("run `peko capability grant`");
    assert!(
        grant_out.status.success(),
        "`peko capability grant` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&grant_out.stdout),
        String::from_utf8_lossy(&grant_out.stderr),
    );
    let on_disk_after_grant = read_capabilities_from_disk(&cli, name);
    assert!(
        on_disk_after_grant.iter().any(|c| c == "tool:Grep"),
        "CLI grant should persist tool:Grep; on_disk={on_disk_after_grant:?}"
    );

    let revoke_out = cli
        .cmd()
        .args(["capability", "revoke", "--principal", name, "tool:Grep"])
        .output()
        .expect("run `peko capability revoke`");
    assert!(
        revoke_out.status.success(),
        "`peko capability revoke` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&revoke_out.stdout),
        String::from_utf8_lossy(&revoke_out.stderr),
    );
    let on_disk_after_revoke = read_capabilities_from_disk(&cli, name);
    assert!(
        !on_disk_after_revoke.iter().any(|c| c == "tool:Grep"),
        "CLI revoke must drop tool:Grep; on_disk={on_disk_after_revoke:?}"
    );
    assert!(
        on_disk_after_revoke.iter().any(|c| c == "tool:Read"),
        "starter bundle must survive a CLI revoke of an unrelated cap; on_disk={on_disk_after_revoke:?}"
    );
}

/// Pin the new `CapabilityRevoked.removed` semantics: revoking a cap
/// that was never granted AND has no wildcard covering it must report
/// `removed: false`. Pins the IPC consistency fix that distinguishes
/// "✅ revoked" from "✅ nothing to revoke".
#[tokio::test]
#[serial]
async fn s7_capability_revoke_with_no_match_reports_not_removed() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let name = "s7-capability-noop";
    create_principal(&cli, name);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    // `tool:NotGrantedAnywhere` is not in the starter bundle and there is
    // no wildcard grant that could cover it.
    let removed = revoke_capability(&client, name, "tool:NotGrantedAnywhere").await;
    assert!(
        !removed,
        "revoking an absent literal with no wildcard cover must report removed=false; got {removed}"
    );
}

// Sanity: the daemon startup itself can take a moment on Windows.
// `DaemonGuard::spawn` already polls `peko daemon status` for up to
// 30s, so this is here only as documentation.
#[allow(dead_code)]
const _STARTUP_BUFFER: Duration = Duration::from_secs(0);
