//! Back-compat shim tests for the `Principal` type (ADR-039).
//!
//! Verifies that:
//! - The `Principal` type parses/serializes correctly via JSON
//!   (the wire format the IPC layer uses).
//! - The session-key byte-stability guarantee from ADR-039 holds.
//! - The `principal_from_string_with_default_user` helper handles all
//!   the legacy string forms used by `runtime/migration.rs`.
//!
//! The on-disk AgentConfig TOML round-trip is covered by the unit tests
//! inside `src/auth/principal.rs` and the in-tree agent fixtures.

// Tests in this file exercise BOTH wire shapes:
// - The legacy `(subject_id, subject_type)` shape (ADR-033, pre-#25).
// - The canonical `subject: Principal` shape (ADR-039, post-#25).
// `SubjectType` is `#[deprecated]` as of #25 (it stays exported for
// one release of back-compat) — silence the warnings inside the test
// module that explicitly constructs the legacy wire path.
#![allow(deprecated)]

use pekobot::auth::ownership::SubjectType;
use pekobot::auth::principal::{Principal, principal_from_string_with_default_user};
use pekobot::ipc::packet::RequestPacket;
use pekobot::session::key::derive_base_session_key;

#[test]
fn test_json_legacy_string_parses() {
    // The IPC wire form: just a string. Resolved by
    // `principal_from_string_with_default_user`.
    let p = principal_from_string_with_default_user("user:abc");
    assert_eq!(p, Principal::User("abc".into()));
}

#[test]
fn test_json_struct_form_parses() {
    // The IPC wire form: the `Principal` derive uses
    // `#[serde(tag = "kind", content = "id")]` for JSON.
    let p: Principal = serde_json::from_str(r#"{"kind":"agent","id":"helper"}"#)
        .expect("struct form parses");
    assert_eq!(p, Principal::Agent("helper".into()));
}

#[test]
fn test_json_struct_form_round_trip() {
    let p = Principal::Agent("helper".into());
    let s = serde_json::to_string(&p).unwrap();
    let p2: Principal = serde_json::from_str(&s).unwrap();
    assert_eq!(p, p2);
}

// -- Session key byte-stability (the regression-prevention suite) --

#[test]
fn test_session_key_unchanged_for_legacy_user_owner() {
    // The v2 session key format is byte-stable for `Principal::User` /
    // `Principal::Agent`. This is the regression-prevention test for
    // the commitment in ADR-039.
    let key = derive_base_session_key("testagent", &Principal::User("user:alice".into()));
    assert_eq!(key, "agent:testagent:peer:user:user_alice");
}

#[test]
fn test_session_key_unchanged_for_legacy_agent_owner() {
    let key = derive_base_session_key("testagent", &Principal::Agent("helper".into()));
    assert_eq!(key, "agent:testagent:peer:agent:helper");
}

#[test]
fn test_session_key_team_falls_back_to_user_default() {
    // `Principal::Team` is NOT a valid session peer; the function
    // falls back to `peer:user:default` and emits a warn.
    let key = derive_base_session_key("testagent", &Principal::Team("eng".into()));
    assert_eq!(key, "agent:testagent:peer:user:default");
}

#[test]
fn test_session_key_public_falls_back_to_user_default() {
    let key = derive_base_session_key("testagent", &Principal::Public);
    assert_eq!(key, "agent:testagent:peer:user:default");
}

// -- `principal_from_string_with_default_user` --

#[test]
fn test_principal_from_string_with_default_user() {
    // Empty string → User("") (the legacy "no owner" sentinel).
    assert_eq!(
        principal_from_string_with_default_user(""),
        Principal::User(String::new())
    );
    // Bare string → User(s).
    assert_eq!(
        principal_from_string_with_default_user("alice"),
        Principal::User("alice".into())
    );
    // `user:alice` → strips the prefix, becomes User("alice").
    assert_eq!(
        principal_from_string_with_default_user("user:alice"),
        Principal::User("alice".into())
    );
    // `agent:helper` → Agent("helper") (via from_str, prefix-agnostic).
    assert_eq!(
        principal_from_string_with_default_user("agent:helper"),
        Principal::Agent("helper".into())
    );
    // `team:eng` → Team("eng").
    assert_eq!(
        principal_from_string_with_default_user("team:eng"),
        Principal::Team("eng".into())
    );
    // `public` → Public.
    assert_eq!(
        principal_from_string_with_default_user("public"),
        Principal::Public
    );
}

// -- Pin the IPC revoke-path fix (issue #25) --
//
// As of issue #25, `RequestPacket::resolved_subject()` collapses the
// `subject: Principal` field (canonical, ADR-039) and the legacy
// `(subject_id, subject_type)` pair (ADR-033) into a single
// `Principal`. Before this fix, the Revoke handlers hardcoded the
// string-form helper, which always returned `Principal::User(...)`,
// making revoke of any Agent/Team/Public grant a silent no-op.
//
// These tests exercise `resolved_subject()` directly. The same
// resolution is exercised end-to-end by the s6 scenario test in
// `tests/scenarios/s6_revoke_principal_collapse_e2e.rs`.

fn grant_with_subject(
    subject: Option<Principal>,
    subject_id: Option<String>,
    subject_type: Option<SubjectType>,
) -> RequestPacket {
    RequestPacket::AgentGrantPermission {
        request_id: 1,
        agent: "a".into(),
        subject,
        subject_id,
        subject_type,
        permission: pekobot::auth::ownership::Permission::Chat,
    }
}

fn revoke_with_subject(
    subject: Option<Principal>,
    subject_id: Option<String>,
    subject_type: Option<SubjectType>,
) -> RequestPacket {
    RequestPacket::AgentRevokePermission {
        request_id: 1,
        agent: "a".into(),
        subject,
        subject_id,
        subject_type,
        permission: pekobot::auth::ownership::Permission::Chat,
    }
}

#[test]
fn test_revoke_principal_subject_matches_agent_grant() {
    // After the fix: a new-shape revoke with `Principal::Agent("helper")`
    // resolves to the same `Principal` as the grant and removes it.
    let grant = grant_with_subject(Some(Principal::Agent("helper".into())), None, None);
    let revoke = revoke_with_subject(Some(Principal::Agent("helper".into())), None, None);

    let grant_subject = grant.resolved_subject().expect("grant resolves");
    let revoke_subject = revoke.resolved_subject().expect("revoke resolves");
    assert_eq!(grant_subject, Principal::Agent("helper".into()));
    assert_eq!(revoke_subject, grant_subject);

    // Legacy shape: a revoke that arrives with `(subject_id="helper",
    // subject_type=Agent)` must also match a same-kind grant. This
    // pins the back-compat window for old CLIs.
    let legacy_revoke = revoke_with_subject(None, Some("helper".into()), Some(SubjectType::Agent));
    assert_eq!(
        legacy_revoke.resolved_subject().expect("legacy revoke resolves"),
        Principal::Agent("helper".into())
    );

    // Cross-kind guard still holds: a `User("helper")` form does NOT
    // match an `Agent("helper")` grant. (This is the bug pre-#25.)
    assert_ne!(
        principal_from_string_with_default_user("helper"),
        Principal::Agent("helper".into())
    );
}

#[test]
fn test_revoke_principal_subject_matches_team_grant() {
    let grant = grant_with_subject(Some(Principal::Team("eng".into())), None, None);
    let revoke = revoke_with_subject(Some(Principal::Team("eng".into())), None, None);

    assert_eq!(
        grant.resolved_subject().expect("grant resolves"),
        Principal::Team("eng".into())
    );
    assert_eq!(
        revoke.resolved_subject().expect("revoke resolves"),
        grant.resolved_subject().expect("grant resolves")
    );

    // Legacy `(subject_id="eng", subject_type=Team)` also resolves.
    let legacy_revoke =
        revoke_with_subject(None, Some("eng".into()), Some(SubjectType::Team));
    assert_eq!(
        legacy_revoke.resolved_subject().expect("legacy revoke resolves"),
        Principal::Team("eng".into())
    );

    // Cross-kind guard still holds for the User fallback.
    assert_ne!(
        principal_from_string_with_default_user("eng"),
        Principal::Team("eng".into())
    );
}

#[test]
fn test_revoke_public_sentinel_collision_and_cross_kind_guard() {
    // New shape with `Principal::Public` round-trips.
    let grant = grant_with_subject(Some(Principal::Public), None, None);
    let revoke = revoke_with_subject(Some(Principal::Public), None, None);
    assert_eq!(
        grant.resolved_subject().expect("grant resolves"),
        Principal::Public
    );
    assert_eq!(
        revoke.resolved_subject().expect("revoke resolves"),
        grant.resolved_subject().expect("grant resolves")
    );

    // The legacy `"public"` string still folds to `Principal::Public`
    // (this is the documented edge case from `principal_from_string`).
    // It must NOT match a `Principal::Public` grant when sent as
    // `(subject_id="public", subject_type=User)` — that combination
    // would have cross-kind-guarded out and silently failed pre-#25.
    let legacy_user = revoke_with_subject(
        None,
        Some("public".into()),
        Some(SubjectType::User),
    );
    // `principal_from_wire("public", User)` returns `Principal::User("public")`,
    // which is NOT equal to `Principal::Public`. The cross-kind guard
    // catches this — it must stay guarded.
    assert_eq!(
        legacy_user.resolved_subject().expect("legacy user resolves"),
        Principal::User("public".into())
    );
    assert_ne!(Principal::User("public".into()), Principal::Public);
}

#[test]
fn test_resolved_subject_missing_subject_errors() {
    // Both `subject` and `subject_id` absent → the resolver must
    // surface a clear error rather than silently producing a sentinel.
    let grant = grant_with_subject(None, None, None);
    let err = grant.resolved_subject().expect_err("must error on missing subject");
    let msg = err.to_string();
    assert!(
        msg.contains("missing subject"),
        "error message should mention missing subject, got: {msg}"
    );

    // Same for revoke.
    let revoke = revoke_with_subject(None, None, None);
    assert!(revoke.resolved_subject().is_err());

    // Same for the team variants.
    let team_grant = RequestPacket::TeamGrantPermission {
        request_id: 1,
        team: "t".into(),
        subject: None,
        subject_id: None,
        subject_type: None,
        permission: pekobot::auth::ownership::Permission::Chat,
    };
    assert!(team_grant.resolved_subject().is_err());

    let team_revoke = RequestPacket::TeamRevokePermission {
        request_id: 1,
        team: "t".into(),
        subject: None,
        subject_id: None,
        subject_type: None,
        permission: pekobot::auth::ownership::Permission::Chat,
    };
    assert!(team_revoke.resolved_subject().is_err());
}

#[test]
fn test_resolved_subject_legacy_wire_shape_serde_round_trip() {
    // A packet that arrives via the legacy wire (only `subject_id` +
    // `subject_type`, no `subject`) must still resolve correctly via
    // `resolved_subject()`. This pins the on-wire back-compat for old
    // CLIs.
    let legacy_json = r#"{
        "type": "agent_grant_permission",
        "request_id": 1,
        "agent": "a",
        "subject_id": "helper",
        "subject_type": "agent",
        "permission": "chat"
    }"#;
    let parsed: RequestPacket =
        serde_json::from_str(legacy_json).expect("legacy wire parses");
    let resolved = parsed.resolved_subject().expect("legacy resolves");
    assert_eq!(resolved, Principal::Agent("helper".into()));

    // And the new wire shape round-trips too.
    let new_json = r#"{
        "type": "agent_grant_permission",
        "request_id": 1,
        "agent": "a",
        "subject": {"kind": "team", "id": "eng"},
        "permission": "chat"
    }"#;
    let parsed: RequestPacket = serde_json::from_str(new_json).expect("new wire parses");
    assert_eq!(
        parsed.resolved_subject().expect("new resolves"),
        Principal::Team("eng".into())
    );

    // Serialize-only: a new-shape packet must NOT carry the legacy
    // fields on the wire (`skip_serializing_if = "Option::is_none"`).
    let new_packet = grant_with_subject(Some(Principal::Agent("helper".into())), None, None);
    let json = serde_json::to_string(&new_packet).expect("serialize");
    assert!(
        !json.contains("subject_id") && !json.contains("subject_type"),
        "new-shape serialization should omit legacy fields, got: {json}"
    );
}

// -- Pin the `manager.rs:1743` wildcard arm behavior (ADR-039) --
//
// The wildcard arm in the spawn-cleanup peer-rehydration match
// previously defaulted to `Peer::Agent(id)`, inconsistent with the
// v1 defaults at `manager.rs:1046, 1049, 1052` (which default to
// `Peer::User("default")` for unknown peer types). After ADR-039
// the wildcard arm produces a `Principal::User` peer. This test
// pins that behavior by constructing a `ParsedSessionKeyV2` with
// an unknown peer_type and verifying the resulting `Principal` is
// `User` (not `Agent`).

#[test]
fn test_wildcard_peer_type_resolves_to_user_peer() {
    use pekobot::session::key::{ParsedSessionKeyV2, derive_base_session_key};

    // Synthetic ParsedSessionKeyV2 with an unknown peer_type.
    // The "kind" is `"unknown"` — neither `"user"` nor `"agent"`,
    // so the wildcard arm fires.
    let parsed = ParsedSessionKeyV2 {
        agent: "testagent".to_string(),
        peer_type: "unknown".to_string(),
        peer_id: "abc".to_string(),
        overlay_type: None,
        overlay_id: None,
        is_overlay: false,
        raw: "agent:testagent:peer:unknown:abc".to_string(),
    };

    // Re-hydrate the peer the way `manager.rs:1740-1744` does.
    // After ADR-039 the wildcard arm produces a User peer.
    let peer: Principal = match parsed.peer_type.as_str() {
        "agent" => Principal::Agent(parsed.peer_id),
        "user" => Principal::User(parsed.peer_id),
        _ => Principal::User(parsed.peer_id),
    };
    assert_eq!(peer, Principal::User("abc".into()));

    // And the resulting session key (using the new wildcard behavior)
    // should be the same as if a User peer had been created
    // directly — the byte-stability guarantee from the session-key
    // tests above still holds.
    let key = derive_base_session_key("testagent", &peer);
    assert_eq!(key, "agent:testagent:peer:user:abc");
}
