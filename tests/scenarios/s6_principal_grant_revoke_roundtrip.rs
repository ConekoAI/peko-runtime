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
//! # What this test asserts
//!
//! 1. **Agent grant/revoke round-trip** — `Subject::Principal("peer-agent")`
//!    granted to an agent, then revoked via IPC; on-disk `config.toml`
//!    shows the grant is gone.
//! 2. **Team grant/revoke round-trip** — same with
//!    `Subject::Team("eng")` on a team.
//! 3. **Public grant/revoke round-trip** — `Subject::Public` granted
//!    to an agent, then revoked.
//! 4. **Idempotent revoke** — revoking a principal that was never
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

/// Pre-write the agent config under the test's isolated `<HOME>/.peko`.
/// `owner = { kind = "user", id = "local" }` matches what
/// `peko agent create` produces and satisfies the local-socket caller
/// owner check in `agent_service::grant_agent_permission`.
fn write_agent_config(cli: &PekoCli, agent_name: &str) {
    let agent_dir = cli.peko_dir().join("agents").join(agent_name);
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    let toml = format!(
        r#"version = "3.0"
name = "{agent_name}"
description = "s6 inline-Subject grant/revoke e2e"
auto_accept_trusted = false

preferred_provider_id = "mock-llm"
preferred_model_id = "default"
default_timeout_seconds = 60
host_runtime_id = ""
owner = {{ kind = "user", id = "local" }}

[extensions]
enabled = []

[channels]
cli = true
"#
    );
    std::fs::write(agent_dir.join("config.toml"), toml).expect("write agent config.toml");
}

/// Pre-write a team under the test's isolated `<HOME>/.peko`. Mirrors
/// the structure `peko team create` produces.
fn write_team(cli: &PekoCli, team_name: &str) {
    let team_dir = cli.peko_dir().join("teams").join(team_name);
    std::fs::create_dir_all(&team_dir).expect("create team dir");
    let toml = format!(
        r#"name = "{team_name}"
description = "s6 inline-Subject grant/revoke e2e team"
created_at = "2026-01-01T00:00:00Z"
host_runtime_id = ""
owner = {{ kind = "user", id = "local" }}
"#
    );
    std::fs::write(team_dir.join("team.toml"), toml).expect("write team.toml");
}

/// Read the agent config from disk and return the granted permissions.
fn read_agent_permissions(cli: &PekoCli, agent_name: &str) -> Vec<Permission> {
    let path = cli
        .peko_dir()
        .join("agents")
        .join(agent_name)
        .join("config.toml");
    let raw = std::fs::read_to_string(&path).expect("read agent config.toml");
    let cfg: peko::agents::agent_config::AgentConfig =
        toml::from_str(&raw).expect("parse agent config.toml");
    cfg.permissions
        .iter()
        .map(|g| g.permission.clone())
        .collect()
}

/// Read the team metadata from disk and return the granted permissions.
fn read_team_permissions(cli: &PekoCli, team_name: &str) -> Vec<Permission> {
    let path = cli
        .peko_dir()
        .join("teams")
        .join(team_name)
        .join("team.toml");
    let raw = std::fs::read_to_string(&path).expect("read team.toml");
    let meta: peko::common::types::team::TeamMetadata =
        toml::from_str(&raw).expect("parse team.toml");
    meta.permissions
        .iter()
        .map(|g| g.permission.clone())
        .collect()
}

/// Issue a grant packet and assert it succeeds.
async fn grant_agent(client: &DaemonClient, agent: &str, subject: Subject, perm: Permission) {
    let packet = RequestPacket::AgentGrantPermission {
        request_id: 1,
        agent: agent.into(),
        subject,
        permission: perm,
    };
    let resp = client
        .request_response(packet)
        .await
        .expect("agent grant succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );
}

/// Issue a revoke packet and assert it succeeds.
async fn revoke_agent(client: &DaemonClient, agent: &str, subject: Subject, perm: Permission) {
    let packet = RequestPacket::AgentRevokePermission {
        request_id: 2,
        agent: agent.into(),
        subject,
        permission: perm,
    };
    let resp = client
        .request_response(packet)
        .await
        .expect("agent revoke succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );
}

/// Issue a team grant packet and assert it succeeds.
async fn grant_team(client: &DaemonClient, team: &str, subject: Subject, perm: Permission) {
    let packet = RequestPacket::TeamGrantPermission {
        request_id: 1,
        team: team.into(),
        subject,
        permission: perm,
    };
    let resp = client
        .request_response(packet)
        .await
        .expect("team grant succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );
}

/// Issue a team revoke packet and assert it succeeds.
async fn revoke_team(client: &DaemonClient, team: &str, subject: Subject, perm: Permission) {
    let packet = RequestPacket::TeamRevokePermission {
        request_id: 2,
        team: team.into(),
        subject,
        permission: perm,
    };
    let resp = client
        .request_response(packet)
        .await
        .expect("team revoke succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Agent grant/revoke round-trip with a non-User `Subject::Principal`
/// subject. Pre-#25 the revoke handler hardcoded `Subject::User(...)`,
/// so this would have been a silent no-op.
#[tokio::test]
#[serial]
async fn s6_agent_subject_round_trips_through_ipc() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let agent = "s6-agent";
    write_agent_config(&cli, agent);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    let subject = Subject::Principal("peer-agent".into());

    grant_agent(&client, agent, subject.clone(), Permission::Chat).await;
    let perms_after_grant = read_agent_permissions(&cli, agent);
    assert_eq!(
        perms_after_grant,
        vec![Permission::Chat],
        "grant should be persisted"
    );

    revoke_agent(&client, agent, subject, Permission::Chat).await;
    let perms_after_revoke = read_agent_permissions(&cli, agent);
    assert!(
        perms_after_revoke.is_empty(),
        "Agent-issued grant should have been revoked; on-disk permissions: {perms_after_revoke:?}"
    );
}

/// Team grant/revoke round-trip with a `Subject::Team` subject.
#[tokio::test]
#[serial]
async fn s6_team_subject_round_trips_through_ipc() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let team = "s6-team";
    write_team(&cli, team);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    let subject = Subject::Team("eng".into());

    grant_team(&client, team, subject.clone(), Permission::Chat).await;
    let perms_after_grant = read_team_permissions(&cli, team);
    assert_eq!(
        perms_after_grant,
        vec![Permission::Chat],
        "team grant should be persisted"
    );

    revoke_team(&client, team, subject, Permission::Chat).await;
    let perms_after_revoke = read_team_permissions(&cli, team);
    assert!(
        perms_after_revoke.is_empty(),
        "Team-issued grant should have been revoked; on-disk permissions: {perms_after_revoke:?}"
    );
}

/// Public grant/revoke round-trip with `Subject::Public` on an agent.
#[tokio::test]
#[serial]
async fn s6_public_subject_round_trips_through_ipc() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let agent = "s6-agent-public";
    write_agent_config(&cli, agent);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    grant_agent(&client, agent, Subject::Public, Permission::ViewSettings).await;
    let perms_after_grant = read_agent_permissions(&cli, agent);
    assert_eq!(
        perms_after_grant,
        vec![Permission::ViewSettings],
        "Public grant should be persisted"
    );

    revoke_agent(&client, agent, Subject::Public, Permission::ViewSettings).await;
    let perms_after_revoke = read_agent_permissions(&cli, agent);
    assert!(
        perms_after_revoke.is_empty(),
        "Public grant should have been revoked; on-disk permissions: {perms_after_revoke:?}"
    );
}

/// Revoking a principal that was never granted should succeed and leave
/// the permissions list empty (idempotent revoke).
#[tokio::test]
#[serial]
async fn s6_revoke_missing_grant_is_idempotent() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let agent = "s6-agent-idempotent";
    write_agent_config(&cli, agent);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    revoke_agent(
        &client,
        agent,
        Subject::Principal("never-granted".into()),
        Permission::Chat,
    )
    .await;

    let perms = read_agent_permissions(&cli, agent);
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
