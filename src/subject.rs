//! `Subject` — the canonical actor type (ADR-041).
//!
//! Before ADR-041, the runtime used the `Principal` enum (ADR-039) to model
//! "who is this?". ADR-041 elevates `Principal` to a top-level container
//! entity, so the actor enum is renamed to `Subject`.
//!
//! A `Subject` is any actor that can initiate an action or appear in an
//! ownership/grant record: a user, an AI principal, a team, or the public.
//!
//! Display format: `"user:{id}" | "principal:{id}" | "team:{id}" | "public"`.
//! FromStr is the inverse. Round-trips are byte-stable.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A runtime actor: a user, a principal, a team, or the public.
///
/// `User` and `Principal` are valid session peers (they have an id you can
/// key a session on). `Team` and `Public` are *not* — `Team` resolves
/// to a set of members at check time, and `Public` has no identity.
/// See `Subject::is_session_peer`.
///
/// Wire format: `{ "kind": "user" | "principal" | "team" | "public", "id": "..." }`
/// via `#[serde(tag = "kind", content = "id")]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Subject {
    /// A pekohub user or local DID.
    User(String),
    /// An AI principal, identified by name or DID.
    Principal(String),
    /// A team id; resolves to a set of member subjects at check time.
    Team(String),
    /// Unauthenticated public access.
    Public,
}

impl Default for Subject {
    /// Default is `Subject::User("")` (the legacy "no owner" sentinel).
    /// This is required so `#[serde(default)]` on the `owner` field works.
    fn default() -> Self {
        Subject::User(String::new())
    }
}

/// Stable string tag for a `Subject` (used in session keys and logging).
///
/// Distinct from `Subject::kind()` so the in-memory kind enum isn't
/// leaked into a public API surface we can't change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubjectKind {
    User,
    Principal,
    Team,
    Public,
}

impl fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Principal => f.write_str("principal"),
            Self::Team => f.write_str("team"),
            Self::Public => f.write_str("public"),
        }
    }
}

impl Subject {
    /// Get the kind tag for this subject.
    #[must_use]
    pub fn kind(&self) -> SubjectKind {
        match self {
            Self::User(_) => SubjectKind::User,
            Self::Principal(_) => SubjectKind::Principal,
            Self::Team(_) => SubjectKind::Team,
            Self::Public => SubjectKind::Public,
        }
    }

    /// Opaque, comparable subject identifier (the "id" component, or
    /// `"public"` for the unauthenticated case). String equality on
    /// this is the contract for owner/grant matching.
    #[must_use]
    pub fn subject_id(&self) -> &str {
        match self {
            Self::User(id) | Self::Principal(id) | Self::Team(id) => id,
            Self::Public => "public",
        }
    }

    /// True if this subject can be used as a session peer.
    ///
    /// Only `User` and `Principal` carry a per-session identity. `Team`
    /// and `Public` are routing buckets, not peer identities.
    #[must_use]
    pub fn is_session_peer(&self) -> bool {
        matches!(self, Self::User(_) | Self::Principal(_))
    }

    /// Project a tunnel-bridge user string (the value returned by
    /// `resolve_bridge_caller`) into a `Subject` (issue #26).
    ///
    /// The bridge is the PekoHub-proxied request path. Its caller is
    /// always a pekohub *user* — but the `"anonymous"` fallback is
    /// semantically unauthenticated, not a user named "anonymous", so
    /// it maps to `Subject::Public`.
    ///
    /// **Prefix normalization (issue #68):** PekoHub sends the caller
    /// id as a bare numeric string (`"39"`) but its own wire format
    /// for `Subject` includes the `user:` tag. The on-disk grant
    /// (parsed via `subject_from_string_with_default_user`) strips
    /// `user:` to store `Subject::User("39")` — the bare form. Without
    /// normalization here, the inbound bridge path would produce
    /// `Subject::User("user:39")` and the permission check would never
    /// match the stored grant. We strip any leading `user:` so
    /// `from_bridge_user("39")` and `from_bridge_user("user:39")` both
    /// collapse to the same `Subject::User("39")` the grant stores.
    /// (A second leading `user:` is not stripped — `strip_prefix`
    /// removes only one occurrence, which is sufficient for the
    /// PekoHub wire formats we observe in practice.)
    ///
    /// Centralized here so the prefix-strip and the anonymous
    /// special-case live next to the type's other constructors
    /// instead of being inlined at every call site.
    #[must_use]
    pub fn from_bridge_user(sub: &str) -> Subject {
        if sub == "anonymous" {
            Self::Public
        } else {
            let bare = sub.strip_prefix("user:").unwrap_or(sub);
            Self::User(bare.to_string())
        }
    }

    /// Canonical wire-side identifier for a principal (issue #28, ADR-041).
    ///
    /// Resolves a Principal config (or any source that gives us a
    /// candidate DID and a local name) to the `Subject::Principal` value
    /// that should flow through the tunnel, the P2P wire, and
    /// `PermissionGrant` lookups:
    ///
    /// - **DID wins** when present and non-empty — this is the
    ///   stable, runtime-independent identifier that lets cross-runtime
    ///   references stay unambiguous.
    /// - **Name is the fallback** when `did` is missing or empty.
    /// - **Empty DID is treated as missing** for defense in depth.
    #[must_use]
    pub fn principal_wire_id(did: Option<&str>, name: &str) -> String {
        match did {
            Some(d) if !d.is_empty() => d.to_string(),
            _ => name.to_string(),
        }
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(id) => write!(f, "user:{id}"),
            Self::Principal(id) => write!(f, "principal:{id}"),
            Self::Team(id) => write!(f, "team:{id}"),
            Self::Public => f.write_str("public"),
        }
    }
}

/// Error returned when a string cannot be parsed into a `Subject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectParseError(pub String);

impl fmt::Display for SubjectParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid subject: {}", self.0)
    }
}

impl std::error::Error for SubjectParseError {}

impl FromStr for Subject {
    type Err = SubjectParseError;

    /// Parse a `Subject` from its `Display` format:
    /// `"kind:id"` (e.g. `"user:alice"`, `"principal:helper"`, `"team:eng"`)
    /// or `"public"`. Empty id is rejected.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "public" {
            return Ok(Self::Public);
        }
        let (kind, id) = s
            .split_once(':')
            .ok_or_else(|| SubjectParseError(format!("expected 'kind:id', got '{s}'")))?;
        if id.is_empty() {
            return Err(SubjectParseError(format!("empty id for kind '{kind}'")));
        }
        match kind {
            "user" => Ok(Self::User(id.to_string())),
            "principal" => Ok(Self::Principal(id.to_string())),
            "team" => Ok(Self::Team(id.to_string())),
            other => Err(SubjectParseError(format!("unknown kind '{other}'"))),
        }
    }
}

/// Parse a CLI ownership string into a `Subject`.
///
/// This is a CLI-level convenience parser: it tries `Subject::from_str`
/// first and falls back to `Subject::User(s)` for bare strings (the
/// common case for ownership CLI args). An empty string
/// resolves to `Subject::User("")` (the "no owner" sentinel).
///
/// **Asymmetric prefix handling (intentional):**
/// - `"user:alice"` → `Subject::User("alice")` (the `user:` prefix is
///   stripped)
/// - `"principal:helper"` / `"team:eng"` / `"public"` → resolved via
///   `Subject::from_str` (the full string is the kind:id pair)
/// - bare `"alice"` → `Subject::User("alice")` (fallback when the
///   string has no `:` separator)
///
/// On-disk configs should set `owner = { kind, id }` directly; this helper
/// is only for CLI arguments that arrive as plain strings.
#[must_use]
pub fn subject_from_string_with_default_user(s: &str) -> Subject {
    if s.is_empty() {
        return Subject::User(String::new());
    }
    if let Ok(p) = Subject::from_str(s) {
        return p;
    }
    Subject::User(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_round_trip() {
        for p in [
            Subject::User("alice".into()),
            Subject::Principal("helper".into()),
            Subject::Team("engineering".into()),
            Subject::Public,
        ] {
            let s = p.to_string();
            let parsed = Subject::from_str(&s).expect("round-trip");
            assert_eq!(parsed, p, "round-trip mismatch for {s}");
        }
    }

    #[test]
    fn test_display_format() {
        assert_eq!(Subject::User("alice".into()).to_string(), "user:alice");
        assert_eq!(
            Subject::Principal("helper".into()).to_string(),
            "principal:helper"
        );
        assert_eq!(Subject::Team("eng".into()).to_string(), "team:eng");
        assert_eq!(Subject::Public.to_string(), "public");
    }

    #[test]
    fn test_from_str_variants() {
        assert_eq!(
            Subject::from_str("user:alice").unwrap(),
            Subject::User("alice".into())
        );
        assert_eq!(
            Subject::from_str("principal:helper").unwrap(),
            Subject::Principal("helper".into())
        );
        assert_eq!(
            Subject::from_str("team:eng").unwrap(),
            Subject::Team("eng".into())
        );
        assert_eq!(Subject::from_str("public").unwrap(), Subject::Public);
    }

    #[test]
    fn test_from_str_errors() {
        assert!(Subject::from_str("").is_err());
        assert!(Subject::from_str("alice").is_err()); // no kind:id
        assert!(Subject::from_str("user:").is_err()); // empty id
        assert!(Subject::from_str("principal:").is_err());
        assert!(Subject::from_str("team:").is_err());
        assert!(Subject::from_str("admin:root").is_err()); // unknown kind
    }

    #[test]
    fn test_kind() {
        assert_eq!(Subject::User("a".into()).kind(), SubjectKind::User);
        assert_eq!(Subject::Principal("a".into()).kind(), SubjectKind::Principal);
        assert_eq!(Subject::Team("a".into()).kind(), SubjectKind::Team);
        assert_eq!(Subject::Public.kind(), SubjectKind::Public);
    }

    #[test]
    fn test_subject_id_and_equality() {
        assert_eq!(Subject::User("alice".into()).subject_id(), "alice");
        assert_eq!(Subject::Principal("alice".into()).subject_id(), "alice");
        assert_eq!(Subject::Team("eng".into()).subject_id(), "eng");
        assert_eq!(Subject::Public.subject_id(), "public");

        // Same kind + same id -> equal
        assert_eq!(Subject::User("a".into()), Subject::User("a".into()));
        // Different kind, same id -> not equal (cross-kind guard)
        assert_ne!(Subject::User("a".into()), Subject::Principal("a".into()));
        assert_ne!(Subject::Team("a".into()), Subject::Principal("a".into()));
    }

    #[test]
    fn test_is_session_peer() {
        assert!(Subject::User("a".into()).is_session_peer());
        assert!(Subject::Principal("a".into()).is_session_peer());
        assert!(!Subject::Team("a".into()).is_session_peer());
        assert!(!Subject::Public.is_session_peer());
    }

    #[test]
    fn test_kind_display() {
        // The canonical replacement for the dropped `peer_type()`
        // method: `kind().to_string()` produces the same lowercase
        // string for every variant. Pin the contract here so any
        // future change to `SubjectKind`'s Display impl surfaces.
        assert_eq!(Subject::User("alice".into()).kind().to_string(), "user");
        assert_eq!(
            Subject::Principal("helper".into()).kind().to_string(),
            "principal"
        );
        assert_eq!(Subject::Team("eng".into()).kind().to_string(), "team");
        assert_eq!(Subject::Public.kind().to_string(), "public");
    }

    /// Issue #26 + #68: the tunnel dispatcher stamps audit events with
    /// a `Subject` projection of the bridge caller string. The
    /// `"anonymous"` fallback is semantically unauthenticated ->
    /// `Subject::Public`; everything else is a pekohub user.
    ///
    /// **Prefix normalization (issue #68):** PekoHub sends the caller
    /// id as a bare numeric string (`"39"`) but its own wire format
    /// for `Subject` includes the `user:` tag. The on-disk grant
    /// (parsed via `subject_from_string_with_default_user`) strips
    /// `user:` to store `Subject::User("39")` — the bare form. We
    /// collapse any leading `user:` here so the bridge and CLI paths
    /// produce the same `Subject::User("39")` that the permission
    /// check matches against.
    #[test]
    fn test_from_bridge_user() {
        // Bare numeric user id from PekoHub — no prefix to strip.
        assert_eq!(
            Subject::from_bridge_user("39"),
            Subject::User("39".to_string())
        );

        // PekoHub's own wire format (`user:39`) collapses to the same
        // bare form. This is the asymmetry fix for #68.
        assert_eq!(
            Subject::from_bridge_user("user:39"),
            Subject::User("39".to_string())
        );

        // Double-prefix collapses one layer — `strip_prefix` only
        // removes a single leading occurrence, so the result still
        // carries one `user:` tag. PekoHub never sends a double-
        // prefixed value in practice; this just pins the literal
        // behavior so a future change to a recursive stripper is
        // caught.
        assert_eq!(
            Subject::from_bridge_user("user:user:39"),
            Subject::User("user:39".to_string())
        );

        // JWT-validated path returns a pekohub sub like `user-42`;
        // no `user:` prefix to strip, but the kind tag is not added
        // here — callers that need a wire-tagged Subject use
        // `Subject::from_str`.
        assert_eq!(
            Subject::from_bridge_user("user-42"),
            Subject::User("user-42".to_string())
        );

        // "anonymous" fallback -> unauthenticated, not a user.
        assert_eq!(Subject::from_bridge_user("anonymous"), Subject::Public);

        // Empty string projects to `Subject::User("")`, distinguishable
        // from `Subject::Public`. Caller-side validation is responsible
        // for not passing empty strings.
        assert_eq!(
            Subject::from_bridge_user(""),
            Subject::User("".to_string())
        );
    }

    #[test]
    fn test_toml_inline_table_parses_via_derive() {
        // Sanity check: the `#[serde(tag = "kind", content = "id")]`
        // derive parses a TOML inline table directly.
        #[derive(serde::Deserialize, Debug)]
        struct Wrap {
            owner: Subject,
        }
        let toml_str = r#"owner = { kind = "principal", id = "helper" }"#;
        let w: Wrap = toml::from_str(toml_str).expect("inline table parses");
        assert_eq!(w.owner, Subject::Principal("helper".into()));
    }

    /// Issue #28: `principal_wire_id` is the single source of truth for
    /// resolving a principal's DID-or-name into a wire identifier.
    #[test]
    fn test_principal_wire_id_prefers_did() {
        // DID wins over name when present and non-empty.
        assert_eq!(
            Subject::principal_wire_id(Some("did:peko:local:abc123"), "helper"),
            "did:peko:local:abc123"
        );
    }

    #[test]
    fn test_principal_wire_id_falls_back_to_name() {
        // Missing DID -> name.
        assert_eq!(Subject::principal_wire_id(None, "helper"), "helper");
    }

    #[test]
    fn test_principal_wire_id_treats_empty_did_as_missing() {
        // Empty DID is treated as missing for defense in depth.
        assert_eq!(Subject::principal_wire_id(Some(""), "helper"), "helper");
    }
}
