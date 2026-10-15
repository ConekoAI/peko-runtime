//! `ActorRef` — the canonical actor type (ADR-041).
//!
//! Before ADR-041, the runtime used the `Principal` enum (ADR-039) to model
//! "who is this?". ADR-041 elevates `Principal` to a top-level container
//! entity, so the actor enum is renamed to `ActorRef`.
//!
//! A `ActorRef` is any actor that can initiate an action or appear in an
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

    /// Wrap a DID or a principal route's text for APIs that accept identity
    /// references in a PrincipalId adapter. This does not resolve it to the
    /// principal's distinct `prin_*` storage key; use the principal
    /// directory for that conversion.
    #[must_use]
    pub fn from_did(did: &impl fmt::Display) -> Self {
        Self(did.to_string())
    }

    /// Canonical "system" sentinel for tools registered once on the shared
    /// `ToolingRuntime` (built-ins, MCP servers) and
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

/// Where an actor is reached (ADR-081 D1): a local route, issued by one
/// runtime, or a Hub route. Scope is a type, never a prefix inside an ID.
///
/// Text form (stable, used on the wire and on disk): an unqualified local
/// route is its bare ID (`local`, `prin_x`); a local route qualified with the
/// runtime that issued it is `local:<id>@<runtime DID>`; a Hub route is
/// `hub:<id>`. `local:<id>` parses as the unqualified local route.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Route {
    /// A route on one runtime. `runtime` is `None` when the route belongs to
    /// the runtime reading it, and the issuing runtime's DID when it does not.
    Local { id: String, runtime: Option<String> },
    /// A Hub route: the account, visitor, or principal UUID.
    Hub(String),
}

impl Route {
    /// An unqualified local route.
    #[must_use]
    pub fn local(id: impl Into<String>) -> Self {
        Self::Local {
            id: id.into(),
            runtime: None,
        }
    }

    /// A Hub route.
    #[must_use]
    pub fn hub(id: impl Into<String>) -> Self {
        Self::Hub(id.into())
    }

    /// Parse the text form.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        if let Some(hub) = text.strip_prefix("hub:") {
            return Self::Hub(hub.to_string());
        }
        match text.strip_prefix("local:") {
            Some(rest) => match rest.split_once('@') {
                Some((id, runtime)) => Self::Local {
                    id: id.to_string(),
                    runtime: Some(runtime.to_string()),
                },
                None => Self::local(rest),
            },
            None => Self::local(text),
        }
    }

    /// The ID within its scope.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Local { id, .. } | Self::Hub(id) => id,
        }
    }

    #[must_use]
    pub fn is_hub(&self) -> bool {
        matches!(self, Self::Hub(_))
    }

    /// The runtime a qualified local route belongs to.
    #[must_use]
    pub fn runtime(&self) -> Option<&str> {
        match self {
            Self::Local { runtime, .. } => runtime.as_deref(),
            Self::Hub(_) => None,
        }
    }

    /// `local` or `hub`.
    #[must_use]
    pub fn scope(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::Hub(_) => "hub",
        }
    }
}

impl Route {
    /// The text form, borrowed when it is the bare ID.
    #[must_use]
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Local { id, runtime: None } => std::borrow::Cow::Borrowed(id),
            other => std::borrow::Cow::Owned(other.to_string()),
        }
    }
}

impl PartialEq<str> for Route {
    /// Compares the text form.
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Route {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for Route {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local { id, runtime: None } => f.write_str(id),
            Self::Local {
                id,
                runtime: Some(runtime),
            } => write!(f, "local:{id}@{runtime}"),
            Self::Hub(id) => write!(f, "hub:{id}"),
        }
    }
}

impl Serialize for Route {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Route {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(deserializer)?))
    }
}

impl From<&str> for Route {
    fn from(text: &str) -> Self {
        Self::parse(text)
    }
}

impl From<String> for Route {
    fn from(text: String) -> Self {
        Self::parse(&text)
    }
}

impl From<&String> for Route {
    fn from(text: &String) -> Self {
        Self::parse(text)
    }
}

impl From<PrincipalDID> for Route {
    fn from(did: PrincipalDID) -> Self {
        Self::parse(&did.0)
    }
}

impl From<&PrincipalDID> for Route {
    fn from(did: &PrincipalDID) -> Self {
        Self::parse(&did.0)
    }
}

impl From<&PrincipalId> for Route {
    fn from(id: &PrincipalId) -> Self {
        Self::local(id.0.clone())
    }
}

/// A reference to an actor — a user, a principal, or a visitor — by one of
/// its routes, or the `Public` wildcard (ADR-081 D1). `Public` is a grant
/// wildcard and an anonymous caller, never an actor: [`ActorKeys::same_actor`]
/// is false for it.
///
/// Wire format: `{ "kind": "user" | "principal" | "visitor" | "public", "id": "<route>" }`
/// via `#[serde(tag = "kind", content = "id")]`; the route uses its text form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum ActorRef {
    /// A human: the runtime's local user or a Hub account.
    User(Route),
    /// A principal.
    Principal(Route),
    /// An anonymous Hub visitor (ADR-058 D5). Visitors are ordinary
    /// non-owner peers and can never alias the user, principal, or
    /// reserved `local` namespaces.
    Visitor(Route),
    /// The grant wildcard / an unauthenticated caller.
    Public,
}

impl Default for ActorRef {
    /// The empty user, read as "no owner recorded" by legacy configs.
    fn default() -> Self {
        ActorRef::User(Route::local(""))
    }
}

impl From<&PrincipalId> for ActorRef {
    /// Internal authorization adapter. Public addressing must resolve this
    /// storage key through ChannelPort::actor_info before exposing it.
    fn from(id: &PrincipalId) -> Self {
        ActorRef::Principal(Route::local(id.0.clone()))
    }
}

impl From<PrincipalId> for ActorRef {
    fn from(id: PrincipalId) -> Self {
        ActorRef::Principal(Route::local(id.0))
    }
}

/// Stable string tag for an actor kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    User,
    Principal,
    Visitor,
    Public,
}

impl fmt::Display for ActorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Principal => f.write_str("principal"),
            Self::Visitor => f.write_str("visitor"),
            Self::Public => f.write_str("public"),
        }
    }
}

impl ActorRef {
    /// The route, or `None` for `Public`.
    #[must_use]
    pub fn route(&self) -> Option<&Route> {
        match self {
            Self::User(route) | Self::Principal(route) | Self::Visitor(route) => Some(route),
            Self::Public => None,
        }
    }

    /// The canonical address (ADR-073/081): `kind:hub:<id>` for a Hub route,
    /// `kind:local:<id>` (with `@<runtime>` when qualified) otherwise, and
    /// `public` for the wildcard. This is the identity of an actor no
    /// registry knows; [`ActorKeys`] maps known actors' aliases onto their
    /// immutable key.
    #[must_use]
    pub fn canonical_address(&self) -> String {
        match self.route() {
            None => "public".into(),
            Some(Route::Hub(id)) => format!("{}:hub:{id}", self.kind()),
            Some(Route::Local { id, runtime: None }) => format!("{}:local:{id}", self.kind()),
            Some(Route::Local {
                id,
                runtime: Some(runtime),
            }) => format!("{}:local:{id}@{runtime}", self.kind()),
        }
    }

    /// Whether this is a local route (not Hub, not `public`).
    #[must_use]
    pub fn is_local_route(&self) -> bool {
        self.route().is_some_and(|route| !route.is_hub())
    }

    /// Whether this is a Hub route.
    #[must_use]
    pub fn is_hub_route(&self) -> bool {
        self.route().is_some_and(Route::is_hub)
    }

    /// The runtime a qualified local route belongs to (ADR-081 D1), if any.
    #[must_use]
    pub fn route_runtime(&self) -> Option<&str> {
        self.route().and_then(Route::runtime)
    }

    /// This reference as seen from outside `runtime` (ADR-081 D6): a local
    /// route of `runtime` gains its runtime qualifier so it can never be
    /// mistaken for a local actor of another runtime. Hub routes, `public`,
    /// and routes already qualified are unchanged.
    #[must_use]
    pub fn qualified_for(&self, runtime: &str) -> Self {
        match self.route() {
            Some(Route::Local { id, runtime: None }) => self.with_route(Route::Local {
                id: id.clone(),
                runtime: Some(runtime.to_string()),
            }),
            _ => self.clone(),
        }
    }

    /// This reference as seen from inside `runtime`: a route qualified with
    /// `runtime` becomes an ordinary local route again (it revives); every
    /// other reference is unchanged, so a foreign local route stays dormant.
    #[must_use]
    pub fn localized_on(&self, runtime: &str) -> Self {
        match self.route() {
            Some(Route::Local {
                id,
                runtime: Some(qualifier),
            }) if qualifier == runtime => self.with_route(Route::local(id.clone())),
            _ => self.clone(),
        }
    }

    /// The same kind of reference by another route.
    #[must_use]
    pub fn with_route(&self, route: Route) -> Self {
        match self {
            Self::User(_) => Self::User(route),
            Self::Principal(_) => Self::Principal(route),
            Self::Visitor(_) => Self::Visitor(route),
            Self::Public => Self::Public,
        }
    }

    /// Get the kind tag for this reference.
    #[must_use]
    pub fn kind(&self) -> ActorKind {
        match self {
            Self::User(_) => ActorKind::User,
            Self::Principal(_) => ActorKind::Principal,
            Self::Visitor(_) => ActorKind::Visitor,
            Self::Public => ActorKind::Public,
        }
    }

    /// The route's text form (`"public"` for the wildcard): what follows
    /// `kind:` in [`Display`](fmt::Display).
    #[must_use]
    pub fn subject_id(&self) -> String {
        self.route()
            .map_or_else(|| "public".to_string(), Route::to_string)
    }

    /// True if this reference can be used as a session peer.
    #[must_use]
    pub fn is_session_peer(&self) -> bool {
        self.route().is_some()
    }

    /// Parse an explicitly scoped reference — `kind:local:<id>` or
    /// `kind:hub:<id>` (ADR-073/081). Bare IDs, which name no scope, are
    /// refused; `public` is not an actor.
    pub fn parse_scoped(text: &str) -> Result<ActorRef, ActorRefParseError> {
        let scoped = text
            .split_once(':')
            .is_some_and(|(_, rest)| rest.starts_with("local:") || rest.starts_with("hub:"));
        if !scoped {
            return Err(ActorRefParseError(format!(
                "use kind:local:<id> or kind:hub:<id> (got `{text}`)"
            )));
        }
        text.parse()
    }

    /// ADR-058 D5: map a validated bridge token's typed claims into an
    /// `ActorRef`. The bridge JWT (minted by PekoHub for proxied chat)
    /// carries a `kind` claim alongside `sub`; only two kinds exist:
    ///
    /// - `kind == "user"` → `sub` is a hub account id → [`ActorRef::User`]
    /// - `kind == "visitor"` → `sub` is a hub-minted anonymous id →
    ///   [`ActorRef::Visitor`]
    ///
    /// Any other `kind` is an error (fail-closed). Empty subs, subs
    /// containing `:`, and a visitor sub of `"local"` are rejected so a
    /// hub-minted id can never alias another namespace.
    pub fn from_bridge_claim(kind: &str, sub: &str) -> Result<ActorRef, ActorRefParseError> {
        if sub.is_empty() {
            return Err(ActorRefParseError(format!(
                "bridge claim kind '{kind}' carries an empty sub"
            )));
        }
        if sub.contains(':') {
            return Err(ActorRefParseError(format!(
                "bridge claim sub must not contain ':' (namespace aliasing); got '{sub}'"
            )));
        }
        match kind {
            "user" => Ok(Self::User(Route::hub(sub))),
            "visitor" => {
                if sub == "local" {
                    Err(ActorRefParseError(
                        "visitor sub 'local' is reserved for the local owner".to_string(),
                    ))
                } else {
                    Ok(Self::Visitor(Route::hub(sub)))
                }
            }
            other => Err(ActorRefParseError(format!(
                "unknown bridge claim kind '{other}' (expected 'user' or 'visitor')"
            ))),
        }
    }
}

impl fmt::Display for ActorRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(route) => write!(f, "user:{route}"),
            Self::Principal(route) => write!(f, "principal:{route}"),
            Self::Visitor(route) => write!(f, "visitor:{route}"),
            Self::Public => f.write_str("public"),
        }
    }
}

/// Error returned when a string cannot be parsed into an `ActorRef`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorRefParseError(pub String);

impl fmt::Display for ActorRefParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid actor reference: {}", self.0)
    }
}

impl std::error::Error for ActorRefParseError {}

impl FromStr for ActorRef {
    type Err = ActorRefParseError;

    /// Parse `"kind:<route>"` (e.g. `"user:local"`, `"user:hub:<id>"`,
    /// `"principal:local:prin_x"`) or `"public"`. Empty IDs are rejected.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "public" {
            return Ok(Self::Public);
        }
        let (kind, id) = s
            .split_once(':')
            .ok_or_else(|| ActorRefParseError(format!("expected 'kind:id', got '{s}'")))?;
        let route = Route::parse(id);
        if route.id().is_empty() {
            return Err(ActorRefParseError(format!("empty id for kind '{kind}'")));
        }
        match kind {
            "user" => Ok(Self::User(route)),
            "principal" => Ok(Self::Principal(route)),
            "visitor" => Ok(Self::Visitor(route)),
            other => Err(ActorRefParseError(format!("unknown kind '{other}'"))),
        }
    }
}

/// An actor's immutable identity (ADR-081 D1). Two references name the same
/// actor exactly when their keys are equal; every authorization decision
/// compares keys, never address text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorKey(pub String);

/// The single identity comparison (ADR-081 D1). The runtime's actor
/// registry implements it so every alias of a known actor yields one key.
pub trait ActorKeys: Send + Sync {
    fn key_of(&self, subject: &ActorRef) -> ActorKey;

    /// Whether `a` and `b` are the same actor. `Public` is never an actor.
    fn same_actor(&self, a: &ActorRef, b: &ActorRef) -> bool {
        !matches!(a, ActorRef::Public)
            && !matches!(b, ActorRef::Public)
            && self.key_of(a) == self.key_of(b)
    }
}

/// Keys for actors no registry knows: the canonical address.
pub struct CanonicalKeys;

impl ActorKeys for CanonicalKeys {
    fn key_of(&self, subject: &ActorRef) -> ActorKey {
        ActorKey(subject.canonical_address())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-081 §3: scope is a type. The text form — and so every wire and
    /// on-disk form — is unchanged from the string-scoped era.
    #[test]
    fn routes_are_typed_and_keep_their_text_form() {
        for (text, route) in [
            ("local", Route::local("local")),
            ("prin_x", Route::local("prin_x")),
            ("hub:abc", Route::hub("abc")),
            (
                "local:prin_x@did:key:zA",
                Route::Local {
                    id: "prin_x".into(),
                    runtime: Some("did:key:zA".into()),
                },
            ),
        ] {
            assert_eq!(Route::parse(text), route);
            assert_eq!(route.to_string(), text);
        }
        // The explicit `local:` spelling is the same unqualified route.
        assert_eq!(Route::parse("local:prin_x"), Route::local("prin_x"));
        // `@` is a qualifier only on an explicit local route.
        assert_eq!(
            Route::parse("alice@example.com"),
            Route::local("alice@example.com")
        );

        let user: ActorRef = "user:hub:abc".parse().unwrap();
        assert_eq!(user, ActorRef::User(Route::hub("abc")));
        assert!(user.is_hub_route());
        assert_eq!(
            serde_json::to_string(&user).unwrap(),
            r#"{"kind":"user","id":"hub:abc"}"#
        );
        let local: ActorRef = serde_json::from_str(r#"{"kind":"user","id":"local"}"#).unwrap();
        assert_eq!(local, ActorRef::User(Route::local("local")));
        assert_eq!(local.canonical_address(), "user:local:local");
        assert_eq!(
            serde_json::to_string(&ActorRef::Public).unwrap(),
            r#"{"kind":"public"}"#
        );
    }

    #[test]
    fn scoped_references_must_name_their_scope() {
        assert!(ActorRef::parse_scoped("principal:local:prin_x").is_ok());
        assert!(ActorRef::parse_scoped("user:hub:abc").is_ok());
        assert!(ActorRef::parse_scoped("user:alice").is_err());
        assert!(ActorRef::parse_scoped("public").is_err());
    }

    /// ADR-081 D6: a local route leaves its runtime qualified, never
    /// matches a local actor elsewhere, and revives on its own runtime.
    #[test]
    fn local_routes_qualify_and_revive() {
        let keys = CanonicalKeys;
        let owner = ActorRef::User("local".into());
        let away = owner.qualified_for("did:key:zA");
        assert_eq!(away, ActorRef::User("local:local@did:key:zA".into()));
        assert_eq!(away.route_runtime(), Some("did:key:zA"));
        assert!(!keys.same_actor(&away, &owner));
        assert_eq!(away.qualified_for("did:key:zB"), away, "already qualified");
        // Elsewhere it stays dormant; at home it revives.
        assert_eq!(away.localized_on("did:key:zB"), away);
        assert!(keys.same_actor(&away.localized_on("did:key:zA"), &owner));
        let peer = ActorRef::Principal("prin_x".into());
        assert!(keys.same_actor(
            &peer.qualified_for("did:key:zA").localized_on("did:key:zA"),
            &peer
        ));
        // Hub routes and the wildcard are never qualified.
        let hub = ActorRef::User("hub:u".into());
        assert_eq!(hub.qualified_for("did:key:zA"), hub);
        assert_eq!(
            ActorRef::Public.qualified_for("did:key:zA"),
            ActorRef::Public
        );
        // The qualified form round-trips through its wire text.
        assert_eq!(away.to_string().parse::<ActorRef>().unwrap(), away);
    }

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
            ActorRef::from(&id),
            ActorRef::Principal(PrincipalDID("prin_abc".to_string()).into())
        );
        assert_eq!(
            ActorRef::from(id),
            ActorRef::Principal(PrincipalDID("prin_abc".to_string()).into())
        );
    }

    #[test]
    fn test_display_round_trip() {
        for p in [
            ActorRef::User("alice".into()),
            ActorRef::Principal("helper".into()),
            ActorRef::Visitor("vis_abc123".into()),
            ActorRef::Public,
        ] {
            let s = p.to_string();
            let parsed = ActorRef::from_str(&s).expect("round-trip");
            assert_eq!(parsed, p, "round-trip mismatch for {s}");
        }
    }

    #[test]
    fn test_display_format() {
        assert_eq!(ActorRef::User("alice".into()).to_string(), "user:alice");
        assert_eq!(
            ActorRef::Principal("helper".into()).to_string(),
            "principal:helper"
        );
        assert_eq!(
            ActorRef::Visitor("vis_abc123".into()).to_string(),
            "visitor:vis_abc123"
        );
        assert_eq!(ActorRef::Public.to_string(), "public");
    }

    #[test]
    fn test_from_str_variants() {
        assert_eq!(
            ActorRef::from_str("user:alice").unwrap(),
            ActorRef::User("alice".into())
        );
        assert_eq!(
            ActorRef::from_str("principal:helper").unwrap(),
            ActorRef::Principal("helper".into())
        );
        assert_eq!(
            ActorRef::from_str("visitor:vis_abc123").unwrap(),
            ActorRef::Visitor("vis_abc123".into())
        );
        assert_eq!(ActorRef::from_str("public").unwrap(), ActorRef::Public);
    }

    #[test]
    fn test_from_str_errors() {
        assert!(ActorRef::from_str("").is_err());
        assert!(ActorRef::from_str("alice").is_err()); // no kind:id
        assert!(ActorRef::from_str("user:").is_err()); // empty id
        assert!(ActorRef::from_str("principal:").is_err());
        assert!(ActorRef::from_str("admin:root").is_err()); // unknown kind
    }

    #[test]
    fn test_kind() {
        assert_eq!(ActorRef::User("a".into()).kind(), ActorKind::User);
        assert_eq!(ActorRef::Principal("a".into()).kind(), ActorKind::Principal);
        assert_eq!(ActorRef::Visitor("a".into()).kind(), ActorKind::Visitor);
        assert_eq!(ActorRef::Public.kind(), ActorKind::Public);
    }

    #[test]
    fn test_subject_id_and_equality() {
        assert_eq!(ActorRef::User("alice".into()).subject_id(), "alice");
        assert_eq!(ActorRef::Principal("alice".into()).subject_id(), "alice");
        assert_eq!(ActorRef::Visitor("alice".into()).subject_id(), "alice");
        assert_eq!(ActorRef::Public.subject_id(), "public");

        // Same kind + same id -> equal
        assert_eq!(ActorRef::User("a".into()), ActorRef::User("a".into()));
        // Different kind, same id -> not equal (cross-kind guard)
        assert_ne!(ActorRef::User("a".into()), ActorRef::Principal("a".into()));
        assert_ne!(ActorRef::User("a".into()), ActorRef::Visitor("a".into()));
    }

    #[test]
    fn test_is_session_peer() {
        assert!(ActorRef::User("a".into()).is_session_peer());
        assert!(ActorRef::Principal("a".into()).is_session_peer());
        assert!(ActorRef::Visitor("a".into()).is_session_peer());
        assert!(!ActorRef::Public.is_session_peer());
    }

    #[test]
    fn test_kind_display() {
        // The canonical replacement for the dropped `peer_type()`
        // method: `kind().to_string()` produces the same lowercase
        // string for every variant. Pin the contract here so any
        // future change to `ActorKind`'s Display impl surfaces.
        assert_eq!(ActorRef::User("alice".into()).kind().to_string(), "user");
        assert_eq!(
            ActorRef::Principal("helper".into()).kind().to_string(),
            "principal"
        );
        assert_eq!(
            ActorRef::Visitor("vis_abc123".into()).kind().to_string(),
            "visitor"
        );
        assert_eq!(ActorRef::Public.kind().to_string(), "public");
    }

    /// ADR-058 D5: the bridge token's typed `kind` claim maps to a
    /// typed `ActorRef`; the retired `from_bridge_user` string coercion
    /// (which let a hub-minted visitor id forge `principal:<did>` or
    /// `user:local`) is gone.
    #[test]
    fn test_from_bridge_claim_happy_paths() {
        assert_eq!(
            ActorRef::from_bridge_claim("user", "39").unwrap(),
            ActorRef::User("hub:39".into())
        );
        assert_eq!(
            ActorRef::from_bridge_claim("user", "user-42").unwrap(),
            ActorRef::User("hub:user-42".into())
        );
        assert_eq!(
            ActorRef::from_bridge_claim("visitor", "vis_abc123").unwrap(),
            ActorRef::Visitor("hub:vis_abc123".into())
        );
    }

    #[test]
    fn test_from_bridge_claim_rejects_unknown_kind() {
        // A bridge token can never again produce a principal subject.
        assert!(ActorRef::from_bridge_claim("principal", "did:key:z6MkX").is_err());
        assert!(ActorRef::from_bridge_claim("", "39").is_err());
        assert!(ActorRef::from_bridge_claim("admin", "39").is_err());
    }

    #[test]
    fn test_from_bridge_claim_rejects_empty_sub() {
        assert!(ActorRef::from_bridge_claim("user", "").is_err());
        assert!(ActorRef::from_bridge_claim("visitor", "").is_err());
    }

    #[test]
    fn test_from_bridge_claim_rejects_namespace_aliasing_subs() {
        // A ':' in the sub would smuggle a wire prefix into the wrong
        // namespace — the exact D5 forgery shapes.
        assert!(ActorRef::from_bridge_claim("visitor", "principal:did:key:z6MkX").is_err());
        assert!(ActorRef::from_bridge_claim("visitor", "user:39").is_err());
        assert!(ActorRef::from_bridge_claim("user", "principal:did:key:z6MkX").is_err());
        assert!(ActorRef::from_bridge_claim("user", "user:39").is_err());
    }

    #[test]
    fn test_from_bridge_claim_rejects_local_visitor() {
        // `local` is the reserved local-owner id; a visitor must never
        // claim it (it would land in the `/local-user` peer child).
        assert!(ActorRef::from_bridge_claim("visitor", "local").is_err());
    }
}
