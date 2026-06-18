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
