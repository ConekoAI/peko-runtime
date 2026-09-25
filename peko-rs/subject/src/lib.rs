//! `Subject` — the canonical actor type (ADR-041).
//!
//! Before ADR-041, the runtime used the `Principal` enum (ADR-039) to model
//! "who is this?". ADR-041 elevates `Principal` to a top-level container
//! entity, so the actor enum is renamed to `Subject`.
//!
//! A `Subject` is any actor that can initiate an action or appear in an
//! ownership/grant record: a user, an AI principal, or the public.
//!
//! Display format: `"user:{id}" | "principal:{id}" | "visitor:{id}" | "public"`.
//! FromStr is the inverse. Round-trips are byte-stable.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub mod path_resolver;
pub use path_resolver::PathResolverLike;

// Actor identifiers live here (not in `principal`) so the actor module is a
// clean lower layer: both `principal` and `agents` depend on `subject`, and
// `subject` depends on neither. This breaks the principal↔agents cycle (F3).

/// Newtype wrapper for a stable principal identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalId(pub String);

impl PrincipalId {
    pub fn generate() -> Self {
        Self(format!("prin_{}", uuid::Uuid::new_v4().simple()))
    }

    /// Construct from a `PrincipalDID`. Both newtypes wrap the same
    /// canonical DID string — `PrincipalId` is the cron/agent identity
    /// key, `PrincipalDID` is the actor wire key, but their contents
    /// are interchangeable in practice (a single principal has one
    /// stable DID for both surfaces). Use this when bridging
    /// `Principal::did()` to a `CronJob::principal_id`.
    #[must_use]
    pub fn from_did(did: &PrincipalDID) -> Self {
        Self(did.0.clone())
    }

    /// Canonical "system" sentinel for tools registered once on the shared
    /// `ExtensionCore` (built-ins, MCP servers) and
    /// visible to every principal.
    ///
    /// The inner string is prefixed with `__` so generated ids
    /// (`prin_<uuid>` from [`PrincipalId::generate`]) cannot collide.
    /// Lookup helpers use this as the fallback target when a principal
    /// has no `(tool_name, principal_id)` entry of its own.
    #[must_use]
    pub fn system() -> &'static Self {
        static SYSTEM: std::sync::OnceLock<PrincipalId> = std::sync::OnceLock::new();
        SYSTEM.get_or_init(|| PrincipalId(String::from("__system__")))
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Thin wrapper around a DID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalDID(pub String);

impl PrincipalDID {
    /// Borrow the inner DID string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for PrincipalDID {
    fn from(s: String) -> Self {
        PrincipalDID(s)
    }
}

impl From<&str> for PrincipalDID {
    fn from(s: &str) -> Self {
        PrincipalDID(s.to_string())
    }
}

impl fmt::Display for PrincipalDID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A runtime actor: a user, a principal, a visitor, or the public.
///
/// `User`, `Principal`, and `Visitor` are valid session peers (they have
/// an id you can key a session on). `Public` is not — it has no identity.
/// See `Subject::is_session_peer`.
///
/// Wire format: `{ "kind": "user" | "principal" | "visitor" | "public", "id": "..." }`
/// via `#[serde(tag = "kind", content = "id")]`.
///
/// **Audit H6:** `Principal` carries a typed [`PrincipalDID`] newtype
/// rather than a bare `String`. This gives the principal id a
/// compile-time distinction from a plain name (a `String` name can no
/// longer be passed where a principal DID is expected) and provides a
/// single place to hang DID validation should we choose to enforce it.
/// `User` remains a plain `String` because its wire value is not a DID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Subject {
    /// A pekohub user or local DID.
    User(String),
    /// An AI principal, identified by its stable DID. Typed as
    /// [`PrincipalDID`] so the principal id surface can carry validation
    /// in one place.
    Principal(PrincipalDID),
    /// An anonymous visitor authenticated only by a hub-minted id
    /// (ADR-058 D5). Visitors are ordinary non-owner peers: they can
    /// chat with exposed principals (their conversations land in
    /// `/visitor-<id>` peer children) but carry no user-principal
    /// authority assumptions and can never alias the `user:`,
    /// `principal:`, or reserved `local` namespaces — the type-level
    /// separation is what makes that guarantee enforceable.
    Visitor(String),
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

impl From<&PrincipalId> for Subject {
    /// Bridge a `PrincipalId` to its actor form. Both newtypes wrap the
    /// same canonical id string (see [`PrincipalId::from_did`]) — this
    /// is the inverse direction, needed wherever a principal-typed API
    /// meets a `Subject`-typed one (ADR-049 channel membership).
    fn from(id: &PrincipalId) -> Self {
        Subject::Principal(PrincipalDID(id.0.clone()))
    }
}

impl From<PrincipalId> for Subject {
    fn from(id: PrincipalId) -> Self {
        Subject::Principal(PrincipalDID(id.0))
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
    Visitor,
    Public,
}

impl fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Principal => f.write_str("principal"),
            Self::Visitor => f.write_str("visitor"),
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
            Self::Visitor(_) => SubjectKind::Visitor,
            Self::Public => SubjectKind::Public,
        }
    }

    /// Opaque, comparable subject identifier (the "id" component, or
    /// `"public"` for the unauthenticated case). String equality on
    /// this is the contract for owner/grant matching.
    #[must_use]
    pub fn subject_id(&self) -> &str {
        match self {
            Self::User(id) => id,
            Self::Principal(id) => id.as_str(),
            Self::Visitor(id) => id,
            Self::Public => "public",
        }
    }

    /// True if this subject can be used as a session peer.
    ///
    /// `User`, `Principal`, and `Visitor` carry a per-session identity.
    /// `Public` is not a peer identity.
    #[must_use]
    pub fn is_session_peer(&self) -> bool {
        matches!(self, Self::User(_) | Self::Principal(_) | Self::Visitor(_))
    }

    /// ADR-058 D5: map a validated bridge token's typed claims into a
    /// `Subject`. The bridge JWT (minted by PekoHub for proxied chat)
    /// carries a `kind` claim alongside `sub`; only two kinds exist:
    ///
    /// - `kind == "user"` → `sub` is a hub account id → [`Subject::User`]
    /// - `kind == "visitor"` → `sub` is a hub-minted anonymous id →
    ///   [`Subject::Visitor`]
    ///
    /// Any other `kind` is an error (fail-closed — a token without a
    /// recognized `kind` claim is rejected by the caller). The `sub`
    /// is additionally constrained so a hub-minted id can never alias
    /// another identity namespace:
    ///
    /// - empty subs are rejected for both kinds;
    /// - subs containing `:` are rejected for both kinds (a `:` would
    ///   let a single string smuggle a `principal:`/`user:`/`visitor:`
    ///   wire prefix into the wrong namespace);
    /// - a visitor sub of `"local"` is rejected (it would collide with
    ///   the reserved local-owner id).
    ///
    /// This replaces the retired `from_bridge_user` string coercion
    /// (whose `principal:`-prefix mapping let a hub-minted visitor id
    /// forge a principal subject — the D5 bug).
    pub fn from_bridge_claim(kind: &str, sub: &str) -> Result<Subject, SubjectParseError> {
        if sub.is_empty() {
            return Err(SubjectParseError(format!(
                "bridge claim kind '{kind}' carries an empty sub"
            )));
        }
        if sub.contains(':') {
            return Err(SubjectParseError(format!(
                "bridge claim sub must not contain ':' (namespace aliasing); got '{sub}'"
            )));
        }
        match kind {
            "user" => Ok(Self::User(sub.to_string())),
            "visitor" => {
                if sub == "local" {
                    Err(SubjectParseError(
                        "visitor sub 'local' is reserved for the local owner".to_string(),
                    ))
                } else {
                    Ok(Self::Visitor(sub.to_string()))
                }
            }
            other => Err(SubjectParseError(format!(
                "unknown bridge claim kind '{other}' (expected 'user' or 'visitor')"
            ))),
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
            Self::Visitor(id) => write!(f, "visitor:{id}"),
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
    /// `"kind:id"` (e.g. `"user:alice"`, `"principal:helper"`)
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
            "principal" => Ok(Self::Principal(PrincipalDID::from(id))),
            "visitor" => Ok(Self::Visitor(id.to_string())),
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
/// - `"principal:helper"` / `"public"` → resolved via
///   `Subject::from_str` (the full string is the kind:id pair or public)
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
    fn test_system_id_is_static_and_stable() {
        // Same address on every call — the &'static guarantee is load-bearing
        // for hot-path callers that don't want to clone.
        let a: *const PrincipalId = PrincipalId::system();
        let b: *const PrincipalId = PrincipalId::system();
        assert_eq!(a, b, "system() must return the same static address");
        // Display form is the documented sentinel.
        assert_eq!(PrincipalId::system().to_string(), "__system__");
        // Generated ids cannot collide with the sentinel.
        let generated = PrincipalId::generate().to_string();
        assert!(!generated.starts_with("__system__"));
    }

    #[test]
    fn test_principal_id_into_subject() {
        let id = PrincipalId("prin_abc".to_string());
        assert_eq!(
            Subject::from(&id),
            Subject::Principal(PrincipalDID("prin_abc".to_string()))
        );
        assert_eq!(
            Subject::from(id),
            Subject::Principal(PrincipalDID("prin_abc".to_string()))
        );
    }

    #[test]
    fn test_display_round_trip() {
        for p in [
            Subject::User("alice".into()),
            Subject::Principal("helper".into()),
            Subject::Visitor("vis_abc123".into()),
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
        assert_eq!(
            Subject::Visitor("vis_abc123".into()).to_string(),
            "visitor:vis_abc123"
        );
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
            Subject::from_str("visitor:vis_abc123").unwrap(),
            Subject::Visitor("vis_abc123".into())
        );
        assert_eq!(Subject::from_str("public").unwrap(), Subject::Public);
    }

    #[test]
    fn test_from_str_errors() {
        assert!(Subject::from_str("").is_err());
        assert!(Subject::from_str("alice").is_err()); // no kind:id
        assert!(Subject::from_str("user:").is_err()); // empty id
        assert!(Subject::from_str("principal:").is_err());
        assert!(Subject::from_str("admin:root").is_err()); // unknown kind
    }

    #[test]
    fn test_kind() {
        assert_eq!(Subject::User("a".into()).kind(), SubjectKind::User);
        assert_eq!(
            Subject::Principal("a".into()).kind(),
            SubjectKind::Principal
        );
        assert_eq!(Subject::Visitor("a".into()).kind(), SubjectKind::Visitor);
        assert_eq!(Subject::Public.kind(), SubjectKind::Public);
    }

    #[test]
    fn test_subject_id_and_equality() {
        assert_eq!(Subject::User("alice".into()).subject_id(), "alice");
        assert_eq!(Subject::Principal("alice".into()).subject_id(), "alice");
        assert_eq!(Subject::Visitor("alice".into()).subject_id(), "alice");
        assert_eq!(Subject::Public.subject_id(), "public");

        // Same kind + same id -> equal
        assert_eq!(Subject::User("a".into()), Subject::User("a".into()));
        // Different kind, same id -> not equal (cross-kind guard)
        assert_ne!(Subject::User("a".into()), Subject::Principal("a".into()));
        assert_ne!(Subject::User("a".into()), Subject::Visitor("a".into()));
    }

    #[test]
    fn test_is_session_peer() {
        assert!(Subject::User("a".into()).is_session_peer());
        assert!(Subject::Principal("a".into()).is_session_peer());
        assert!(Subject::Visitor("a".into()).is_session_peer());
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
        assert_eq!(
            Subject::Visitor("vis_abc123".into()).kind().to_string(),
            "visitor"
        );
        assert_eq!(Subject::Public.kind().to_string(), "public");
    }

    /// ADR-058 D5: the bridge token's typed `kind` claim maps to a
    /// typed `Subject`; the retired `from_bridge_user` string coercion
    /// (which let a hub-minted visitor id forge `principal:<did>` or
    /// `user:local`) is gone.
    #[test]
    fn test_from_bridge_claim_happy_paths() {
        assert_eq!(
            Subject::from_bridge_claim("user", "39").unwrap(),
            Subject::User("39".to_string())
        );
        assert_eq!(
            Subject::from_bridge_claim("user", "user-42").unwrap(),
            Subject::User("user-42".to_string())
        );
        assert_eq!(
            Subject::from_bridge_claim("visitor", "vis_abc123").unwrap(),
            Subject::Visitor("vis_abc123".to_string())
        );
    }

    #[test]
    fn test_from_bridge_claim_rejects_unknown_kind() {
        // A bridge token can never again produce a principal subject.
        assert!(Subject::from_bridge_claim("principal", "did:key:z6MkX").is_err());
        assert!(Subject::from_bridge_claim("", "39").is_err());
        assert!(Subject::from_bridge_claim("admin", "39").is_err());
    }

    #[test]
    fn test_from_bridge_claim_rejects_empty_sub() {
        assert!(Subject::from_bridge_claim("user", "").is_err());
        assert!(Subject::from_bridge_claim("visitor", "").is_err());
    }

    #[test]
    fn test_from_bridge_claim_rejects_namespace_aliasing_subs() {
        // A ':' in the sub would smuggle a wire prefix into the wrong
        // namespace — the exact D5 forgery shapes.
        assert!(Subject::from_bridge_claim("visitor", "principal:did:key:z6MkX").is_err());
        assert!(Subject::from_bridge_claim("visitor", "user:39").is_err());
        assert!(Subject::from_bridge_claim("user", "principal:did:key:z6MkX").is_err());
        assert!(Subject::from_bridge_claim("user", "user:39").is_err());
    }

    #[test]
    fn test_from_bridge_claim_rejects_local_visitor() {
        // `local` is the reserved local-owner id; a visitor must never
        // claim it (it would land in the `/local-user` peer child).
        assert!(Subject::from_bridge_claim("visitor", "local").is_err());
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
