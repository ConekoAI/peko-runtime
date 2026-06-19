//! `Principal` — the canonical actor type (ADR-039).
//!
//! Before this module, the runtime had three different ways to model
//! "who is this?" that disagreed on the universe of subjects:
//!
//! - `Peer::{User, Agent}` — no `Team` variant.
//! - `SubjectType::{User, Team, Public}` — no `Agent` variant.
//! - `AgentConfig::owner_id: String` — free-form, default `""`.
//!
//! `Principal` unifies them into a single value type. `Peer` is now a
//! type alias for `Principal`, and `SubjectType` is kept as the IPC
//! wire-side tag.
//!
//! Display format: `"user:{id}" | "agent:{id}" | "team:{id}" | "public"`.
//! FromStr is the inverse. Round-trips are byte-stable.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The actor in a session, permission check, or ownership record.
///
/// `User` and `Agent` are valid session peers (they have an id you can
/// key a session on). `Team` and `Public` are *not* — `Team` resolves
/// to a set of members at check time, and `Public` has no identity.
/// See `Principal::is_session_peer`.
///
/// Wire format: `{ "kind": "user" | "agent" | "team" | "public", "id": "..." }`
/// via `#[serde(tag = "kind", content = "id")]`. The legacy
/// `owner_id = "string"` form is handled by the two-field
/// `owner` + `owner_id` shim on `AgentConfig` / `TeamMetadata` (see
/// ADR-039).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Principal {
    /// A pekohub user or local DID.
    User(String),
    /// A peko agent instance, identified by name or DID.
    Agent(String),
    /// A team id; resolves to a set of `Principal::Agent` members.
    Team(String),
    /// Unauthenticated public access.
    Public,
}

impl Default for Principal {
    /// Default is `Principal::User("")` (the legacy "no owner" sentinel
    /// used in `runtime/migration.rs:170-171, 234-235`). This is
    /// required so `#[serde(default)]` on the `owner` field works.
    fn default() -> Self {
        Principal::User(String::new())
    }
}

/// Stable string tag for a `Principal` (used in session keys and logging).
///
/// Distinct from `Principal::kind()` so the in-memory kind enum isn't
/// leaked into a public API surface we can't change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubjectKind {
    User,
    Agent,
    Team,
    Public,
}

impl fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Agent => f.write_str("agent"),
            Self::Team => f.write_str("team"),
            Self::Public => f.write_str("public"),
        }
    }
}

impl Principal {
    /// Get the kind tag for this principal.
    #[must_use]
    pub fn kind(&self) -> SubjectKind {
        match self {
            Self::User(_) => SubjectKind::User,
            Self::Agent(_) => SubjectKind::Agent,
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
            Self::User(id) | Self::Agent(id) | Self::Team(id) => id,
            Self::Public => "public",
        }
    }

    /// True if this principal can be used as a session peer.
    ///
    /// Only `User` and `Agent` carry a per-session identity. `Team`
    /// and `Public` are routing buckets, not peer identities.
    #[must_use]
    pub fn is_session_peer(&self) -> bool {
        matches!(self, Self::User(_) | Self::Agent(_))
    }

    // -- Compatibility shim for the old `Peer` API --
    //
    // These methods mirror `Peer::{id, peer_type, is_user, is_agent}` so
    // the 25 call sites that used the old enum keep compiling through
    // the `pub type Peer = Principal;` alias. `peer_type` is kept for
    // backwards compatibility but the new `kind()` is preferred.

    /// Get the peer's ID string (mirrors `Peer::id`).
    #[must_use]
    pub fn id(&self) -> &str {
        self.subject_id()
    }

    /// Get the peer type as a string (mirrors `Peer::peer_type`).
    ///
    /// Returns `"user" | "agent" | "team" | "public"`. Existing callers
    /// that only inspect `"user"` or `"agent"` are unaffected.
    #[must_use]
    pub fn peer_type(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Agent(_) => "agent",
            Self::Team(_) => "team",
            Self::Public => "public",
        }
    }

    /// Check if this principal is a user (mirrors `Peer::is_user`).
    #[must_use]
    pub fn is_user(&self) -> bool {
        matches!(self, Self::User(_))
    }

    /// Check if this principal is an agent (mirrors `Peer::is_agent`).
    #[must_use]
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent(_))
    }

    /// Project a tunnel-bridge user string (the value returned by
    /// `resolve_bridge_caller`) into a `Principal` (issue #26).
    ///
    /// The bridge is the PekoHub-proxied request path. Its caller is
    /// always a pekohub *user* — but the `"anonymous"` fallback is
    /// semantically unauthenticated, not a user named "anonymous", so
    /// it maps to `Principal::Public`. Every other value gets the
    /// `user:{sub}` prefix that matches `CallerContext::subject()`'s
    /// projection for `Identity::User`.
    ///
    /// Centralized here so the `user:` prefix and the anonymous
    /// special-case live next to the type's other constructors
    /// instead of being inlined at every call site.
    #[must_use]
    pub fn from_bridge_user(sub: &str) -> Principal {
        if sub == "anonymous" {
            Self::Public
        } else {
            Self::User(format!("user:{sub}"))
        }
    }
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(id) => write!(f, "user:{id}"),
            Self::Agent(id) => write!(f, "agent:{id}"),
            Self::Team(id) => write!(f, "team:{id}"),
            Self::Public => write!(f, "public"),
        }
    }
}

/// Error returned when a string cannot be parsed into a `Principal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalParseError(pub String);

impl fmt::Display for PrincipalParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid principal: {}", self.0)
    }
}

impl std::error::Error for PrincipalParseError {}

impl FromStr for Principal {
    type Err = PrincipalParseError;

    /// Parse a `Principal` from its `Display` format:
    /// `"kind:id"` (e.g. `"user:alice"`, `"agent:helper"`, `"team:eng"`)
    /// or `"public"`. Empty id is rejected.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "public" {
            return Ok(Self::Public);
        }
        let (kind, id) = s
            .split_once(':')
            .ok_or_else(|| PrincipalParseError(format!("expected 'kind:id', got '{s}'")))?;
        if id.is_empty() {
            return Err(PrincipalParseError(format!("empty id for kind '{kind}'")));
        }
        match kind {
            "user" => Ok(Self::User(id.to_string())),
            "agent" => Ok(Self::Agent(id.to_string())),
            "team" => Ok(Self::Team(id.to_string())),
            other => Err(PrincipalParseError(format!("unknown kind '{other}'"))),
        }
    }
}

/// Build a `Principal` from a wire-format string with a fallback kind.
///
/// This is the defensive parser for legacy data where the kind is
/// implicit (e.g. a bare `owner_id = "user:abc"` → `Principal::User`).
/// Tries `Principal::from_str` first; on failure, treats the string as
/// an id of the supplied kind. An empty string always resolves to
/// `Principal::User("")` (the legacy "no owner" sentinel).
#[must_use]
pub fn principal_from_string(s: &str, default_kind: SubjectKind) -> Principal {
    if s.is_empty() {
        return Principal::User(String::new());
    }
    if let Ok(p) = Principal::from_str(s) {
        return p;
    }
    match default_kind {
        SubjectKind::User => Principal::User(s.to_string()),
        SubjectKind::Agent => Principal::Agent(s.to_string()),
        SubjectKind::Team => Principal::Team(s.to_string()),
        SubjectKind::Public => Principal::Public,
    }
}

/// Convenience: parse an `owner_id` string as a `Principal::User`
/// (the legacy default kind for ownership strings).
///
/// Tries `Principal::from_str` first; on failure, treats the string as
/// a `Principal::User` id. An empty string always resolves to
/// `Principal::User("")` (the legacy "no owner" sentinel).
///
/// **Asymmetric prefix handling (intentional, but worth flagging):**
/// - `"user:alice"` → `Principal::User("alice")` (the `user:` prefix is
///   stripped as a legacy normalization)
/// - `"agent:helper"` / `"team:eng"` / `"public"` → resolved via
///   `Principal::from_str` (prefix-agnostic — the full string is the
///   kind:id pair)
/// - bare `"alice"` → `Principal::User("alice")` (the legacy fallback
///   when the string has no `:` separator)
///
/// This means a typo like `owner_id = "use:alice"` silently becomes
/// `Principal::User("use:alice")` rather than being normalized. That
/// matches the pre-ADR-039 behavior (the legacy `subject_id` field was
/// always treated as a user identifier), so the asymmetry is
/// backward-compatible. New configs should set `owner = { kind, id }`
/// explicitly.
#[must_use]
pub fn principal_from_string_with_default_user(s: &str) -> Principal {
    principal_from_string(s, SubjectKind::User)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_round_trip() {
        for p in [
            Principal::User("alice".into()),
            Principal::Agent("helper".into()),
            Principal::Team("engineering".into()),
            Principal::Public,
        ] {
            let s = p.to_string();
            let parsed = Principal::from_str(&s).expect("round-trip");
            assert_eq!(parsed, p, "round-trip mismatch for {s}");
        }
    }

    #[test]
    fn test_display_format() {
        assert_eq!(Principal::User("alice".into()).to_string(), "user:alice");
        assert_eq!(Principal::Agent("helper".into()).to_string(), "agent:helper");
        assert_eq!(Principal::Team("eng".into()).to_string(), "team:eng");
        assert_eq!(Principal::Public.to_string(), "public");
    }

    #[test]
    fn test_from_str_variants() {
        assert_eq!(
            Principal::from_str("user:alice").unwrap(),
            Principal::User("alice".into())
        );
        assert_eq!(
            Principal::from_str("agent:helper").unwrap(),
            Principal::Agent("helper".into())
        );
        assert_eq!(
            Principal::from_str("team:eng").unwrap(),
            Principal::Team("eng".into())
        );
        assert_eq!(Principal::from_str("public").unwrap(), Principal::Public);
    }

    #[test]
    fn test_from_str_errors() {
        assert!(Principal::from_str("").is_err());
        assert!(Principal::from_str("alice").is_err()); // no kind:id
        assert!(Principal::from_str("user:").is_err()); // empty id
        assert!(Principal::from_str("agent:").is_err());
        assert!(Principal::from_str("team:").is_err());
        assert!(Principal::from_str("admin:root").is_err()); // unknown kind
    }

    #[test]
    fn test_kind() {
        assert_eq!(Principal::User("a".into()).kind(), SubjectKind::User);
        assert_eq!(Principal::Agent("a".into()).kind(), SubjectKind::Agent);
        assert_eq!(Principal::Team("a".into()).kind(), SubjectKind::Team);
        assert_eq!(Principal::Public.kind(), SubjectKind::Public);
    }

    #[test]
    fn test_subject_id_and_equality() {
        assert_eq!(Principal::User("alice".into()).subject_id(), "alice");
        assert_eq!(Principal::Agent("alice".into()).subject_id(), "alice");
        assert_eq!(Principal::Team("eng".into()).subject_id(), "eng");
        assert_eq!(Principal::Public.subject_id(), "public");

        // Same kind + same id -> equal
        assert_eq!(
            Principal::User("a".into()),
            Principal::User("a".into())
        );
        // Different kind, same id -> not equal (cross-kind guard)
        assert_ne!(
            Principal::User("a".into()),
            Principal::Agent("a".into())
        );
        assert_ne!(Principal::Team("a".into()), Principal::Agent("a".into()));
    }

    #[test]
    fn test_is_session_peer() {
        assert!(Principal::User("a".into()).is_session_peer());
        assert!(Principal::Agent("a".into()).is_session_peer());
        assert!(!Principal::Team("a".into()).is_session_peer());
        assert!(!Principal::Public.is_session_peer());
    }

    #[test]
    fn test_peer_compat_methods() {
        let p = Principal::User("alice".into());
        assert_eq!(p.id(), "alice");
        assert_eq!(p.peer_type(), "user");
        assert!(p.is_user());
        assert!(!p.is_agent());

        let p = Principal::Agent("helper".into());
        assert_eq!(p.id(), "helper");
        assert_eq!(p.peer_type(), "agent");
        assert!(p.is_agent());
        assert!(!p.is_user());

        assert_eq!(Principal::Public.peer_type(), "public");
        assert_eq!(Principal::Team("eng".into()).peer_type(), "team");
    }

    /// Issue #26: the tunnel dispatcher stamps audit events with a
    /// `Principal` projection of the bridge caller string. The
    /// `"anonymous"` fallback is semantically unauthenticated →
    /// `Principal::Public`; everything else is a pekohub user with the
    /// `user:` prefix (matching `CallerContext::subject()`'s
    /// `Identity::User` projection).
    #[test]
    fn test_from_bridge_user() {
        // Real user — gets the `user:` prefix.
        assert_eq!(
            Principal::from_bridge_user("alice"),
            Principal::User("user:alice".to_string())
        );

        // The JWT-validated path returns a pekohub sub like
        // `user-42`; the prefix still gets added so the kind tag is
        // present on the wire.
        assert_eq!(
            Principal::from_bridge_user("user-42"),
            Principal::User("user:user-42".to_string())
        );

        // "anonymous" fallback → unauthenticated, not a user.
        assert_eq!(Principal::from_bridge_user("anonymous"), Principal::Public);

        // Empty string is *not* a special case — it projects to
        // `Principal::User("user:")`, distinguishable from
        // `Principal::Public`. (Caller-side validation is responsible
        // for not passing empty strings here; this constructor just
        // applies the prefix.)
        assert_eq!(
            Principal::from_bridge_user(""),
            Principal::User("user:".to_string())
        );
    }

    #[test]
    fn test_toml_inline_table_parses_via_derive() {
        // Sanity check: the `#[serde(tag = "kind", content = "id")]`
        // derive parses a TOML inline table directly. If this fails,
        // the deserializer-of-the-value-via-shim approach is needed
        // (see the shim in `deserialize_owner_principal`).
        #[derive(serde::Deserialize, Debug)]
        struct Wrap {
            owner: Principal,
        }
        let toml_str = r#"owner = { kind = "agent", id = "helper" }"#;
        let w: Wrap = toml::from_str(toml_str).expect("inline table parses");
        assert_eq!(w.owner, Principal::Agent("helper".into()));
    }
}
