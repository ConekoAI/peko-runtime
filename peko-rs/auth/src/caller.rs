//! Caller context — resolved identity for every incoming request

use peko_subject::ActorRef;

use super::types::ApiKeyScope;

/// Resolved identity of the caller
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    /// Unix socket / localhost UDP — OS is the trust boundary
    Local,
    /// pekohub user (sub claim from JWT)
    User(String),
    /// PekoHub anonymous visitor (never an account).
    Visitor(String),
    /// API key ID (prefix of the key, not the secret)
    ApiKey(String),
}

impl Identity {
    /// Get a string identifier for rate-limit bucketing
    #[must_use]
    pub fn rate_limit_bucket(&self) -> String {
        match self {
            Self::Local => "local".to_string(),
            Self::User(uid) => format!("user:{uid}"),
            Self::Visitor(uid) => format!("visitor:{uid}"),
            Self::ApiKey(key_id) => format!("apikey:{key_id}"),
        }
    }
}

/// Authentication method used
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    LocalTrust,
    PekohubJwt,
    ApiKey,
}

/// Full caller context attached to every request
///
/// ADR-057: the caller's *authority* (who may act) lives here and is
/// derived from the transport authentication only. The *attribution
/// identity* (who a message says it is from) is derived server-side
/// via [`CallerContext::attribution_subject`] — no request packet
/// carries a user identity.
#[derive(Clone, Debug)]
pub struct CallerContext {
    /// Resolved identity
    pub identity: Identity,
    /// Authentication method used
    pub auth_method: AuthMethod,
    /// Rate limit bucket key
    pub rate_limit_bucket: String,
    /// API key scopes (only populated for API key auth)
    pub api_key_scopes: Vec<ApiKeyScope>,
}

impl CallerContext {
    /// Create a local-trust caller context: the runtime's local user
    /// (ADR-081 D3).
    #[must_use]
    pub fn local() -> Self {
        Self {
            identity: Identity::Local,
            auth_method: AuthMethod::LocalTrust,
            rate_limit_bucket: "local".to_string(),
            api_key_scopes: vec![ApiKeyScope::Read, ApiKeyScope::Write, ApiKeyScope::Admin],
        }
    }

    /// Create a caller context from a pekohub JWT
    #[must_use]
    pub fn from_jwt(sub: String) -> Self {
        let sub = sub.strip_prefix("user:").unwrap_or(&sub).to_string();
        let bucket = format!("user:{sub}");
        Self {
            identity: Identity::User(sub),
            auth_method: AuthMethod::PekohubJwt,
            rate_limit_bucket: bucket,
            api_key_scopes: Vec::new(), // N/A for JWT
        }
    }

    #[must_use]
    pub fn from_visitor_jwt(sub: String) -> Self {
        Self {
            rate_limit_bucket: format!("visitor:{sub}"),
            identity: Identity::Visitor(sub),
            auth_method: AuthMethod::PekohubJwt,
            api_key_scopes: Vec::new(),
        }
    }

    /// Create a caller context from an API key
    #[must_use]
    pub fn from_api_key(key_id: String, scopes: Vec<ApiKeyScope>) -> Self {
        let bucket = format!("apikey:{key_id}");
        Self {
            identity: Identity::ApiKey(key_id),
            auth_method: AuthMethod::ApiKey,
            rate_limit_bucket: bucket,
            api_key_scopes: scopes,
        }
    }

    /// Permission projection for a resource. Physical local trust is owner-equivalent;
    /// the message actor and its conversation remain the logged-in account.
    #[must_use]
    pub fn permission_subject(&self, owner: &ActorRef) -> ActorRef {
        if self.is_operator() {
            owner.clone()
        } else {
            self.subject()
        }
    }

    /// Local transport or an explicitly administrative runtime API key.
    #[must_use]
    pub fn is_operator(&self) -> bool {
        self.is_local()
            || (matches!(self.identity, Identity::ApiKey(_))
                && self.api_key_scopes.contains(&ApiKeyScope::Admin))
    }

    /// Check if this caller has local trust (owner equivalent)
    #[must_use]
    pub fn is_local(&self) -> bool {
        matches!(self.identity, Identity::Local)
    }

    /// The local actor ID. Credential diagnostics use rate_limit_bucket separately.
    #[must_use]
    pub fn subject_id(&self) -> String {
        self.subject().subject_id()
    }

    /// Get the caller's `ActorRef` projection (ADR-039, ADR-041).
    ///
    /// R5 — typed `Principal` projection for API keys (issue: per-PR
    /// audit): API-key callers are *not* pekohub users, they are a
    /// distinct actor class. They project as `ActorRef::Principal`
    /// carrying a typed `PrincipalDID` of the form `apikey:{key_id}`.
    /// `User` uses the same scoped Hub account ID for ownership, attribution and
    /// channel membership. Hub accounts have no runtime-local proxy identity.
    ///
    /// Equality across kinds is intentionally prevented
    /// (`ActorRef::User("apikey:...") != ActorRef::Principal(...)` of the
    /// same id string) — switching to the typed projection means a
    /// grant issued for an API key cannot accidentally match a user
    /// with the same id, and vice versa.
    #[must_use]
    pub fn subject(&self) -> ActorRef {
        match &self.identity {
            Identity::Local => self.attribution_subject(),
            Identity::User(sub) => ActorRef::User(peko_subject::Route::hub(sub.clone())),
            Identity::Visitor(sub) => ActorRef::Visitor(peko_subject::Route::hub(sub.clone())),
            Identity::ApiKey(_) => self.attribution_subject(),
        }
    }

    /// Build a local caller with a specific runtime DID.
    #[must_use]
    pub fn local_with_did(runtime_did: String) -> Self {
        let bucket = format!("local:{runtime_did}");
        Self {
            identity: Identity::Local,
            auth_method: AuthMethod::LocalTrust,
            rate_limit_bucket: bucket.clone(),
            api_key_scopes: vec![ApiKeyScope::Read, ApiKeyScope::Write, ApiKeyScope::Admin],
        }
    }

    /// The single attribution identity this caller speaks as — derived
    /// from the caller, never from the request packet.
    ///
    /// The terminal is always the runtime's local user (ADR-081 D3); its
    /// Hub route, once the runtime is registered, is an alias of that one
    /// actor rather than a second identity. An API key is a scoped
    /// credential of the operator and attributes the same way. Remote
    /// callers attribute as their Hub account or visitor ID.
    #[must_use]
    pub fn attribution_subject(&self) -> ActorRef {
        match &self.identity {
            Identity::Local | Identity::ApiKey(_) => ActorRef::User("local".into()),
            Identity::User(sub) => ActorRef::User(peko_subject::Route::hub(sub.clone())),
            Identity::Visitor(sub) => ActorRef::Visitor(peko_subject::Route::hub(sub.clone())),
        }
    }

    /// Check if this caller's API key scopes include the given scope.
    /// Always returns true for Local and JWT identities.
    #[must_use]
    pub fn has_scope(&self, scope: &ApiKeyScope) -> bool {
        match self.identity {
            Identity::Local | Identity::User(_) | Identity::Visitor(_) => true,
            Identity::ApiKey(_) => self.api_key_scopes.contains(scope),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_subject_is_user_local() {
        let s = CallerContext::local().subject();
        assert_eq!(s, ActorRef::User("local".into()));
    }

    /// ADR-081 D3: the terminal is always the local user.
    #[test]
    fn local_attribution_is_the_local_user() {
        let c = CallerContext::local();
        assert_eq!(c.attribution_subject(), ActorRef::User("local".into()));
        assert_eq!(c.subject(), ActorRef::User("local".into()));
        assert!(c.is_local());
    }

    /// ADR-057: JWT callers attribute as their bare sub (ADR-049 wire
    /// form), not the prefixed grant projection.
    #[test]
    fn jwt_attribution_is_scoped_hub_subject() {
        let c = CallerContext::from_jwt("39".to_string());
        assert_eq!(c.attribution_subject(), ActorRef::User("hub:39".into()));
        assert_eq!(c.subject(), ActorRef::User("hub:39".into()));
    }

    /// ADR-057: API-key callers attribute as the runtime owner (a key
    /// is a scoped credential of the owner, not an identity class) —
    /// while its grant-matching authority subject stays the typed
    /// `apikey:{id}` principal so owner grants don't silently apply.
    #[test]
    fn api_key_attribution_is_owner_identity() {
        let c = CallerContext::from_api_key("pkr_abc".to_string(), vec![]);
        assert_eq!(c.attribution_subject(), ActorRef::User("local".into()));
        // Authority stays key-typed (least privilege).
        assert_eq!(c.subject(), ActorRef::User("local".into()));
    }

    #[test]
    fn jwt_subject_is_user_with_sub() {
        let s = CallerContext::from_jwt("alice".to_string()).subject();
        assert_eq!(s, ActorRef::User("hub:alice".into()));
    }

    #[test]
    fn credentials_do_not_create_principal_actors() {
        let caller = CallerContext::from_api_key("pkr_key".into(), vec![]);
        assert_eq!(caller.subject(), ActorRef::User("local".into()));
        assert_eq!(caller.subject_id(), "local");
        assert_eq!(caller.rate_limit_bucket, "apikey:pkr_key");
    }
    #[test]
    fn visitor_and_user_tokens_with_same_id_are_distinct() {
        let user = CallerContext::from_jwt("same".into());
        let visitor = CallerContext::from_visitor_jwt("same".into());
        assert_ne!(user.subject(), visitor.subject());
        assert_eq!(visitor.subject(), ActorRef::Visitor("hub:same".into()));
        assert!(!visitor.is_operator());
    }
    #[test]
    fn local_permission_projection_does_not_change_the_message_actor() {
        let caller = CallerContext::local();
        let owner = ActorRef::User("hub:account".into());
        assert_eq!(caller.permission_subject(&owner), owner);
        assert_eq!(caller.subject(), ActorRef::User("local".into()));
    }
}
