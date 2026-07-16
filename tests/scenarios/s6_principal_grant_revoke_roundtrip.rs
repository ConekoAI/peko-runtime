//! End-to-end scenario for grant/revoke round-trips with inline
//! `Subject` subjects (ADR-039, post issue #30).
//!
//! # Scope
//!
//! This is the replacement for the old
//! `tests/scenarios/s6_revoke_principal_collapse_e2e.rs`, which tested
//! the legacy `(subject_id, subject_type)` wire shape from issue #25.
//! That legacy shape was dropped in issue #30; grant/revoke packets now
//! carry a single `subject: Subject`. This scenario exercises the
//! same persistence path (IPC → service layer → on-disk config) with
//! the new inline shape.
//!
//! After the Principal-as-single-actor migration the standalone agent
//! permission packets were removed; the Principal is now the actor that
//! owns permissions. This scenario exercises
//! `PrincipalGrantPermission`/`PrincipalRevokePermission`.
//!
//! # What this test asserts
//!
//! 1. **Principal grant/revoke round-trip** — `Subject::Principal("peer")`
//!    granted to a Principal, then revoked via IPC; on-disk
//!    `principal.toml` shows the grant is gone.
//! 2. **Public grant/revoke round-trip** — `Subject::Public` granted
//!    to a Principal, then revoked.
//! 3. **Idempotent revoke** — revoking a subject that was never
//!    granted returns success and leaves the permissions list empty.
//!
//! # Why this test does not need PekoHub
//!
//! The assertions are on the on-disk `permissions` array, which is the
//! source of truth for the local config. PekoHub-side ACL derivation is
//! covered by `s5_live_permit_propagation.rs`.

#[path = "../common/mod.rs"]
mod common;
use common::{DaemonGuard, PekoCli};
use serial_test::serial;
use std::time::Duration;

use peko::auth::ownership::Permission;
use peko::auth::Subject;
use peko::ipc::packet::{RequestPacket, ResponsePacket};
use peko::ipc::DaemonClient;

// ---------------------------------------------------------------------------
// Fixture wiring
// ---------------------------------------------------------------------------

/// Create a Principal under the test's isolated `<HOME>/.peko` by invoking
/// the real `peko principal create` command. This produces the workspace,
/// identity, default `agents/primary.md` prompt, and `principal.toml` that
/// the daemon's `load_principal` requires — `owner = { kind = "user", id =
/// "local" }`, which satisfies the local-socket caller owner check in the
/// `PrincipalGrantPermission` handler.
fn create_principal(cli: &PekoCli, name: &str) {
    // Runs before `DaemonGuard::spawn`, so seed the `mock-llm` catalog
    // entry here too (idempotent — the guard re-seeds the same values
    // from MOCK_LLM_URL on spawn) and pin the principal to it: create
    // requires `--model` and validates it against the catalog. These
    // grant/revoke tests never dial the LLM, so a placeholder URL is
    // fine when MOCK_LLM_URL isn't set.
    let mock_url = std::env::var_os("MOCK_LLM_URL")
        .map(|u| u.to_string_lossy().into_owned())
        .unwrap_or_else(|| "http://127.0.0.1:9/v1".to_string());
    common::agent::seed_mock_provider_in_catalog(cli.home(), &mock_url);
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

    // `peko principal create` defaults the owner to `user:default`, but the
    // local-socket caller in this test is `user:local`. Rewrite the owner so
    // the `ManageSettings` owner-check in the grant/revoke handlers passes —
    // mirroring the `owner = { kind = "user", id = "local" }` the old agent
    // fixture used.
    let path = cli
        .peko_dir()
        .join("principals")
        .join(name)
        .join("principal.toml");
    let raw = std::fs::read_to_string(&path).expect("read principal.toml");
    let mut cfg: peko::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    cfg.owner = Subject::User("local".into());
    std::fs::write(
        &path,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");
}

/// Read the principal config from disk and return the granted permissions.
fn read_principal_permissions(cli: &PekoCli, name: &str) -> Vec<Permission> {
    let path = cli
        .peko_dir()
        .join("principals")
        .join(name)
        .join("principal.toml");
    let raw = std::fs::read_to_string(&path).expect("read principal.toml");
    let cfg: peko::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    cfg.permissions
        .iter()
        .map(|g| g.permission.clone())
        .collect()
}

/// Issue a principal grant packet and assert it succeeds.
async fn grant_principal(client: &DaemonClient, name: &str, subject: Subject, perm: Permission) {
    let packet = RequestPacket::PrincipalGrantPermission {
        request_id: 1,
        name: name.into(),
        subject,
        permission: perm,
    };
    let resp = client
        .request_response(packet)
        .await
        .expect("principal grant succeeds");
    assert!(
        matches!(resp, ResponsePacket::PrincipalPermissionGranted { .. }),
        "expected PrincipalPermissionGranted, got: {resp:?}"
    );
}

/// Issue a principal revoke packet and assert it succeeds.
async fn revoke_principal(client: &DaemonClient, name: &str, subject: Subject, perm: Permission) {
    let packet = RequestPacket::PrincipalRevokePermission {
        request_id: 2,
        name: name.into(),
        subject,
        permission: perm,
    };
    let resp = client
        .request_response(packet)
        .await
        .expect("principal revoke succeeds");
    assert!(
        matches!(resp, ResponsePacket::PrincipalPermissionRevoked { .. }),
        "expected PrincipalPermissionRevoked, got: {resp:?}"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Principal grant/revoke round-trip with a non-User `Subject::Principal`
/// subject. Pre-#25 the revoke handler hardcoded `Subject::User(...)`,
/// so this would have been a silent no-op.
#[tokio::test]
#[serial]
async fn s6_principal_subject_round_trips_through_ipc() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let name = "s6-principal";
    create_principal(&cli, name);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    let subject = Subject::Principal("peer".into());

    grant_principal(&client, name, subject.clone(), Permission::Chat).await;
    let perms_after_grant = read_principal_permissions(&cli, name);
    assert_eq!(
        perms_after_grant,
        vec![Permission::Chat],
        "grant should be persisted"
    );

    revoke_principal(&client, name, subject, Permission::Chat).await;
    let perms_after_revoke = read_principal_permissions(&cli, name);
    assert!(
        perms_after_revoke.is_empty(),
        "Principal-issued grant should have been revoked; on-disk permissions: {perms_after_revoke:?}"
    );
}

/// Public grant/revoke round-trip with `Subject::Public` on a Principal.
#[tokio::test]
#[serial]
async fn s6_public_subject_round_trips_through_ipc() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let name = "s6-principal-public";
    create_principal(&cli, name);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    grant_principal(&client, name, Subject::Public, Permission::ViewSettings).await;
    let perms_after_grant = read_principal_permissions(&cli, name);
    assert_eq!(
        perms_after_grant,
        vec![Permission::ViewSettings],
        "Public grant should be persisted"
    );

    revoke_principal(&client, name, Subject::Public, Permission::ViewSettings).await;
    let perms_after_revoke = read_principal_permissions(&cli, name);
    assert!(
        perms_after_revoke.is_empty(),
        "Public grant should have been revoked; on-disk permissions: {perms_after_revoke:?}"
    );
}

/// Revoking a subject that was never granted should succeed and leave
/// the permissions list empty (idempotent revoke).
#[tokio::test]
#[serial]
async fn s6_revoke_missing_grant_is_idempotent() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let name = "s6-principal-idempotent";
    create_principal(&cli, name);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    revoke_principal(
        &client,
        name,
        Subject::Principal("never-granted".into()),
        Permission::Chat,
    )
    .await;

    let perms = read_principal_permissions(&cli, name);
    assert!(
        perms.is_empty(),
        "revoking a non-existent grant should leave permissions empty; got {perms:?}"
    );
}

// Sanity: the daemon startup itself can take a moment on Windows.
// `DaemonGuard::spawn` already polls `peko daemon status` for up to
// 30s, so this is here only as documentation.
#[allow(dead_code)]
const _STARTUP_BUFFER: Duration = Duration::from_secs(0);
