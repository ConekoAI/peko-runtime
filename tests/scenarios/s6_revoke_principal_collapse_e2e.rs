//! End-to-end scenario for [issue #25] — collapse IPC
//! `(subject_id, subject_type)` into `subject: Principal`.
//!
//! # Scope
//!
//! Regression for [ConekoAI/peko-runtime#25][issue-25]: the IPC
//! `AgentRevokePermission` / `TeamRevokePermission` packets used to
//! carry only `subject_id: String`, with no `subject_type`. The
//! server-side handler hardcoded `principal_from_string_with_default_user`,
//! which always returns `Principal::User(...)`, so any Agent / Team /
//! Public grant became unrevocable through the IPC layer — a silent
//! no-op pinned by three regression tests in
//! `tests/principal_back_compat.rs`.
//!
//! The fix introduces `RequestPacket::resolved_subject()`, which
//! collapses the canonical `subject: Principal` field with the legacy
//! `(subject_id, subject_type)` pair. This test exercises the full
//! wire path (CLI/daemon IPC → service layer → on-disk config) to
//! confirm that **revoking an Agent / Team / Public grant via IPC
//! actually removes the grant**.
//!
//! # What this test asserts
//!
//! 1. **Agent grant/revoke round-trip** — `Principal::Agent("peer-agent")`
//!    granted, then revoked via IPC; on-disk `agent.toml` shows the
//!    grant is gone.
//! 2. **Team grant/revoke round-trip** — same with `Principal::Team("eng")`.
//! 3. **Legacy wire shape still works** — a CLI sending the legacy
//!    `(subject_id, subject_type)` pair still has its grant removed
//!    on revoke (pins the back-compat window).
//! 4. **Missing subject errors cleanly** — a packet with neither
//!    `subject` nor `subject_id` returns a `ResponsePacket::Error`
//!    that mentions "missing subject", rather than silently
//!    succeeding.
//!
//! # Why this test does not need PekoHub
//!
//! The bug is on the local IPC path; the on-disk `permissions` array
//! is the source of truth, and PekoHub's `canChat` ACL re-derives
//! from that on the next `announce_instances`. PekoHub-side grant
//! endpoint changes are owned by issue #27.
//!
//! [issue-25]: https://github.com/ConekoAI/peko-runtime/issues/25

// `SubjectType` is `#[deprecated]` as of #25 (kept for one release
// of back-compat). The legacy-wire-shape test below constructs packets
// with `subject_type: Some(SubjectType::User)` — silence the warnings
// inside this test module.
#![allow(deprecated)]

#[path = "../common/mod.rs"]
mod common;
use common::{DaemonGuard, PekoCli};
use serial_test::serial;
use std::time::Duration;

use pekobot::auth::ownership::{Permission, SubjectType};
use pekobot::auth::principal::Principal;
use pekobot::ipc::packet::{RequestPacket, ResponsePacket};
use pekobot::ipc::DaemonClient;

// ---------------------------------------------------------------------------
// Fixture wiring
// ---------------------------------------------------------------------------

/// Pre-write the agent config under the test's isolated `<HOME>/.peko`.
/// `owner = { kind = "user", id = "local" }` matches what
/// `peko agent create` produces and what `s5_live_permit_propagation`
/// uses (the CLI's local-socket caller passes the owner check in
/// `agent_service::grant_agent_permission`). Empty `permissions` so
/// the test starts from a known baseline.
fn write_agent_config(cli: &PekoCli, agent_name: &str) {
    let agent_dir = cli.peko_dir().join("agents").join(agent_name);
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    let toml = r#"version = "3.0"
name = "s6-agent"
description = "issue #25 collapse-IPC e2e"
auto_accept_trusted = false

preferred_provider_id = "mock-llm"
preferred_model_id = "default"
default_timeout_seconds = 60
host_runtime_id = ""
owner = { kind = "user", id = "local" }

[extensions]
enabled = []

[channels]
cli = true
"#;
    std::fs::write(agent_dir.join("config.toml"), toml).expect("write agent config.toml");
}

/// Pre-write a team under the test's isolated `<HOME>/.peko`. Mirrors
/// the structure `peko team create` produces.
fn write_team(cli: &PekoCli, team_name: &str) {
    let team_dir = cli.peko_dir().join("teams").join(team_name);
    std::fs::create_dir_all(&team_dir).expect("create team dir");
    let toml = format!(
        r#"name = "{team_name}"
description = "issue #25 collapse-IPC e2e team"
created_at = "2026-01-01T00:00:00Z"
host_runtime_id = ""
owner = {{ kind = "user", id = "local" }}
"#
    );
    std::fs::write(team_dir.join("team.toml"), toml).expect("write team.toml");
}

/// Read the agent config from disk and return its permissions.
fn read_agent_permissions(cli: &PekoCli, agent_name: &str) -> Vec<Permission> {
    let path = cli.peko_dir().join("agents").join(agent_name).join("config.toml");
    let raw = std::fs::read_to_string(&path).expect("read agent config.toml");
    let cfg: pekobot::types::agent::AgentConfig =
        toml::from_str(&raw).expect("parse agent config.toml");
    cfg.permissions.iter().map(|g| g.permission.clone()).collect()
}

/// Read the team metadata from disk and return its permissions.
fn read_team_permissions(cli: &PekoCli, team_name: &str) -> Vec<Permission> {
    let path = cli.peko_dir().join("teams").join(team_name).join("team.toml");
    let raw = std::fs::read_to_string(&path).expect("read team.toml");
    let meta: pekobot::common::types::team::TeamMetadata =
        toml::from_str(&raw).expect("parse team.toml");
    meta.permissions.iter().map(|g| g.permission.clone()).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The bug repro (issue #25): a non-User grant must be revocable via
/// IPC. Pre-#25, the revoke handler hardcoded `Principal::User(...)`
/// for the subject, so the equality check `g.subject == *subject`
/// silently failed and the grant persisted.
#[tokio::test]
#[serial]
async fn s6_revoke_agent_subject_round_trips_through_ipc() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let agent = "s6-agent";
    write_agent_config(&cli, agent);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    // Grant: send a new-shape packet with a non-User subject.
    let grant = RequestPacket::AgentGrantPermission {
        request_id: 1,
        agent: agent.into(),
        subject: Some(Principal::Agent("peer-agent".into())),
        subject_id: None,
        subject_type: None,
        permission: Permission::Chat,
    };
    let resp = client
        .request_response(grant)
        .await
        .expect("grant succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );

    // Confirm the grant landed on disk.
    let perms_after_grant = read_agent_permissions(&cli, agent);
    assert_eq!(
        perms_after_grant,
        vec![Permission::Chat],
        "grant should be persisted"
    );

    // Revoke: same non-User subject, new shape. Pre-#25 this is a no-op.
    let revoke = RequestPacket::AgentRevokePermission {
        request_id: 2,
        agent: agent.into(),
        subject: Some(Principal::Agent("peer-agent".into())),
        subject_id: None,
        subject_type: None,
        permission: Permission::Chat,
    };
    let resp = client
        .request_response(revoke)
        .await
        .expect("revoke succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );

    // The fix: the on-disk grant is now gone.
    let perms_after_revoke = read_agent_permissions(&cli, agent);
    assert!(
        perms_after_revoke.is_empty(),
        "Agent-issued grant should have been revoked; on-disk permissions: {perms_after_revoke:?}"
    );
}

#[tokio::test]
#[serial]
async fn s6_revoke_team_subject_round_trips_through_ipc() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let team = "s6-team";
    write_team(&cli, team);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    let grant = RequestPacket::TeamGrantPermission {
        request_id: 1,
        team: team.into(),
        subject: Some(Principal::Team("eng".into())),
        subject_id: None,
        subject_type: None,
        permission: Permission::Chat,
    };
    let resp = client
        .request_response(grant)
        .await
        .expect("grant succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );

    let perms_after_grant = read_team_permissions(&cli, team);
    assert_eq!(perms_after_grant, vec![Permission::Chat]);

    let revoke = RequestPacket::TeamRevokePermission {
        request_id: 2,
        team: team.into(),
        subject: Some(Principal::Team("eng".into())),
        subject_id: None,
        subject_type: None,
        permission: Permission::Chat,
    };
    let resp = client
        .request_response(revoke)
        .await
        .expect("revoke succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );

    let perms_after_revoke = read_team_permissions(&cli, team);
    assert!(
        perms_after_revoke.is_empty(),
        "Team-issued grant should have been revoked; on-disk permissions: {perms_after_revoke:?}"
    );
}

/// Back-compat: a CLI that still emits the legacy `(subject_id,
/// subject_type)` pair must keep working. This pins the deprecation
/// window for #25 — when both fields are dropped, this test must be
/// updated (or the legacy path deleted) in lockstep.
#[tokio::test]
#[serial]
async fn s6_legacy_cli_wire_shape_still_accepted() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let agent = "s6-agent";
    write_agent_config(&cli, agent);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    let grant = RequestPacket::AgentGrantPermission {
        request_id: 1,
        agent: agent.into(),
        subject: None,
        subject_id: Some("legacy-user".into()),
        subject_type: Some(SubjectType::User),
        permission: Permission::ViewSettings,
    };
    let resp = client
        .request_response(grant)
        .await
        .expect("legacy grant succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );

    let perms_after_grant = read_agent_permissions(&cli, agent);
    assert_eq!(perms_after_grant, vec![Permission::ViewSettings]);

    // Revoke with the same legacy shape.
    let revoke = RequestPacket::AgentRevokePermission {
        request_id: 2,
        agent: agent.into(),
        subject: None,
        subject_id: Some("legacy-user".into()),
        subject_type: Some(SubjectType::User),
        permission: Permission::ViewSettings,
    };
    let resp = client
        .request_response(revoke)
        .await
        .expect("legacy revoke succeeds");
    assert!(
        matches!(resp, ResponsePacket::Done { success: true, .. }),
        "expected Done with success=true, got: {resp:?}"
    );

    let perms_after_revoke = read_agent_permissions(&cli, agent);
    assert!(
        perms_after_revoke.is_empty(),
        "legacy-wire grant should have been revoked; on-disk permissions: {perms_after_revoke:?}"
    );
}

/// A packet with neither `subject` nor `subject_id` must surface an
/// explicit error rather than silently succeeding or being treated as
/// a `Principal::User("")`. Pre-#25 a Revoke packet with neither
/// field would never reach the service layer; the new behavior is
/// strictly better — the operator sees exactly what they sent wrong.
#[tokio::test]
#[serial]
async fn s6_missing_subject_returns_error() {
    let cli = PekoCli::new();
    cli.install_ipc_endpoint_env();
    let agent = "s6-agent";
    write_agent_config(&cli, agent);

    let _guard = DaemonGuard::spawn(&cli);
    let client = DaemonClient::connect().await.expect("connect daemon");

    let grant = RequestPacket::AgentGrantPermission {
        request_id: 1,
        agent: agent.into(),
        subject: None,
        subject_id: None,
        subject_type: None,
        permission: Permission::Chat,
    };
    // `request_response` returns Err on a ResponsePacket::Error.
    let err = client
        .request_response(grant)
        .await
        .expect_err("missing subject must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("missing subject"),
        "error should mention 'missing subject', got: {msg}"
    );

    // Same for revoke.
    let revoke = RequestPacket::AgentRevokePermission {
        request_id: 2,
        agent: agent.into(),
        subject: None,
        subject_id: None,
        subject_type: None,
        permission: Permission::Chat,
    };
    let err = client
        .request_response(revoke)
        .await
        .expect_err("missing subject must error on revoke too");
    assert!(format!("{err:#}").contains("missing subject"));
}

// Sanity: the daemon startup itself can take a moment on Windows.
// `DaemonGuard::spawn` already polls `peko daemon status` for up to
// 30s, so this is here only as documentation.
#[allow(dead_code)]
const _STARTUP_BUFFER: Duration = Duration::from_secs(0);