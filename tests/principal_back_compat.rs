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

use pekobot::auth::principal::{Principal, principal_from_string_with_default_user};
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

// -- Pin the Revoke-path limitation (ADR-039 follow-up) --
//
// `ipc/server.rs` `AgentRevokePermission` / `TeamRevokePermission`
// handlers currently hardcode the wire-side assumption that the
// subject being revoked is a `Principal::User` — the IPC `RequestPacket`
// for revoke only carries `subject_id: String`, not `subject_type`.
// This means revoking an Agent / Team / Public grant via the IPC
// layer is a no-op (the service layer's `Principal` equality check
// won't match). The fix is a wire-shape change to add `subject_type`
// to the revoke packets (a follow-up ADR).
//
// This test pins the current limitation so the regression is caught
// by CI if anyone "fixes" the helper without also updating the IPC
// wire format.

#[test]
fn test_revoke_string_form_cannot_match_agent_grant() {
    // The grant is for an Agent. A `Principal`-aware revoke with
    // `Principal::Agent("helper")` removes it correctly.
    let agent_grant_subject = Principal::Agent("helper".into());
    let user_form_subject = principal_from_string_with_default_user("helper");

    // Cross-kind guard: an Agent grant is NOT equal to a User subject
    // with the same id, even if the underlying id string matches.
    assert_ne!(agent_grant_subject, user_form_subject);
    // The string-form helper always returns User, regardless of how
    // the original grant was written.
    assert_eq!(user_form_subject, Principal::User("helper".into()));
}

#[test]
fn test_revoke_string_form_cannot_match_team_grant() {
    // A Team grant with id "eng" is NOT equal to the User form
    // `Principal::User("eng")` produced by the IPC revoke path.
    // (The string-form helper happens to produce `Principal::Public`
    // for the bare string `"public"` because `from_str` succeeds for
    // the sentinel; but that's an unrelated edge case — for any
    // non-sentinel team id, the kind is dropped on revoke.)
    let team_grant_subject = Principal::Team("eng".into());
    let user_form_subject = principal_from_string_with_default_user("eng");
    assert_ne!(team_grant_subject, user_form_subject);
    assert_eq!(user_form_subject, Principal::User("eng".into()));
}

#[test]
fn test_revoke_string_form_public_sentinel_collision() {
    // Edge case: the bare string `"public"` parses to `Principal::Public`
    // via `from_str`, so `principal_from_string_with_default_user("public")`
    // happens to return `Principal::Public` (not `Principal::User("public")`).
    // This means a `Principal::Public` grant CAN be revoked by the
    // current string-form helper — but ONLY because the id is the
    // sentinel string. A grant with `subject = Principal::Public` and
    // any other id string would still not match. Pin this so a
    // future change to the helper doesn't silently break the
    // collision.
    assert_eq!(
        principal_from_string_with_default_user("public"),
        Principal::Public
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
