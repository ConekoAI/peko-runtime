//! Caller context — resolved identity for every incoming request

use peko_subject::Subject;

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
    /// ADR-057: hub user id bound to this runtime (the pekohub user
    /// that owns the runtime registration). Only set for `Local`
    /// callers — the daemon rereads authenticated login metadata
    /// on each request. `None` means "not logged into pekohub" and the
    /// local terminal attributes as `user:local`.
    pub hub_user_id: Option<String>,
}

impl CallerContext {
    /// Create a local-trust caller context
    #[must_use]
    pub fn local() -> Self {
        Self::local_with_hub_user_id(None)
    }

    /// Create a local-trust caller context with a known pekohub
    /// owner (ADR-057). `hub_user_id` is the hub user id captured from
    /// authenticated login metadata; `None` = not logged in.
    #[must_use]
    pub fn local_with_hub_user_id(hub_user_id: Option<String>) -> Self {
        Self {
            identity: Identity::Local,
            auth_method: AuthMethod::LocalTrust,
            rate_limit_bucket: "local".to_string(),
            api_key_scopes: vec![ApiKeyScope::Read, ApiKeyScope::Write, ApiKeyScope::Admin],
            hub_user_id,
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
            hub_user_id: None,
        }
    }

    #[must_use]
    pub fn from_visitor_jwt(sub: String) -> Self {
        Self {
            rate_limit_bucket: format!("visitor:{sub}"),
            identity: Identity::Visitor(sub),
            auth_method: AuthMethod::PekohubJwt,
            api_key_scopes: Vec::new(),
            hub_user_id: None,
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
            hub_user_id: None,
        }
    }

    /// Permission projection for a resource. Physical local trust is owner-equivalent;
    /// the message actor and its conversation remain the logged-in account.
    #[must_use]
    pub fn permission_subject(&self, owner: &Subject) -> Subject {
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
        self.subject().subject_id().to_string()
    }

    /// Get the caller's `Subject` projection (ADR-039, ADR-041).
    ///
    /// R5 — typed `Principal` projection for API keys (issue: per-PR
    /// audit): API-key callers are *not* pekohub users, they are a
    /// distinct actor class. They project as `Subject::Principal`
    /// carrying a typed `PrincipalDID` of the form `apikey:{key_id}`.
    /// `User` uses the same scoped Hub account ID for ownership, attribution and
    /// channel membership. Hub accounts have no runtime-local proxy identity.
    ///
    /// Equality across kinds is intentionally prevented
    /// (`Subject::User("apikey:...") != Subject::Principal(...)` of the
    /// same id string) — switching to the typed projection means a
    /// grant issued for an API key cannot accidentally match a user
    /// with the same id, and vice versa.
    #[must_use]
    pub fn subject(&self) -> Subject {
        match &self.identity {
            Identity::Local => self.attribution_subject(),
            Identity::User(sub) => Subject::User(format!("hub:{sub}")),
            Identity::Visitor(sub) => Subject::Visitor(format!("hub:{sub}")),
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
            hub_user_id: None,
        }
    }

    /// ADR-057: the single attribution identity this caller speaks
    /// as — derived from the caller, never from the request packet.
    ///
    /// There are exactly three identity classes (ADR-057):
    ///
    /// - **user** — a pekohub-verified human: a local terminal whose
    ///   runtime is logged into pekohub (`Subject::User(<hub owner
    ///   id>)`) or a remote pekohub JWT caller
    ///   (`Subject::User(<bare sub>)`; ownership checks use that same canonical subject).
    /// - **local** — the unverified local terminal
    ///   (`Subject::User("local")`).
    /// - **principal** — a cross-runtime actor, signature-verified
    ///   per event (not projected here; tunnel peers carry their own
    ///   DID).
    ///
    /// An API key is *not* a fourth class: it is a scoped credential
    /// of the runtime owner, so it attributes exactly as the owner
    /// would (hub user when logged in, else `local`). Its scopes —
    /// not its identity — are the least-privilege mechanism, and the
    /// audit trail records the credential id separately.
    #[must_use]
    pub fn attribution_subject(&self) -> Subject {
        match &self.identity {
            Identity::Local | Identity::ApiKey(_) => Subject::User(
                self.hub_user_id
                    .as_ref()
                    .map(|id| format!("hub:{id}"))
                    .unwrap_or_else(|| "local".to_string()),
            ),
            Identity::User(sub) => Subject::User(format!("hub:{sub}")),
            Identity::Visitor(sub) => Subject::Visitor(format!("hub:{sub}")),
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
        assert_eq!(s, Subject::User("local".to_string()));
    }

    /// ADR-057: attribution identity for a local caller not logged
    /// into pekohub is `user:local`.
    #[test]
    fn local_attribution_defaults_to_user_local() {
        let s = CallerContext::local().attribution_subject();
        assert_eq!(s, Subject::User("local".to_string()));
    }

    /// ADR-057: a local caller on a pekohub-logged-in runtime
    /// attributes as the hub owner id — while its grant-matching
    /// subject stays `user:local` (authority is unchanged; D2).
    #[test]
    fn local_attribution_uses_hub_owner_when_logged_in() {
        let c = CallerContext::local_with_hub_user_id(Some("39".to_string()));
        assert_eq!(c.attribution_subject(), Subject::User("hub:39".to_string()));
        assert_eq!(c.subject(), Subject::User("hub:39".to_string()));
        assert!(c.is_local());
    }

    /// ADR-057: JWT callers attribute as their bare sub (ADR-049 wire
    /// form), not the prefixed grant projection.
    #[test]
    fn jwt_attribution_is_scoped_hub_subject() {
        let c = CallerContext::from_jwt("39".to_string());
        assert_eq!(c.attribution_subject(), Subject::User("hub:39".to_string()));
        assert_eq!(c.subject(), Subject::User("hub:39".to_string()));
    }

    /// ADR-057: API-key callers attribute as the runtime owner (a key
    /// is a scoped credential of the owner, not an identity class) —
    /// while its grant-matching authority subject stays the typed
    /// `apikey:{id}` principal so owner grants don't silently apply.
    #[test]
    fn api_key_attribution_is_owner_identity() {
        let c = CallerContext::from_api_key("pkr_abc".to_string(), vec![]);
        assert_eq!(c.attribution_subject(), Subject::User("local".to_string()));
        // Authority stays key-typed (least privilege).
        assert_eq!(c.subject(), Subject::User("local".to_string()));
    }

    #[test]
    fn jwt_subject_is_user_with_sub() {
        let s = CallerContext::from_jwt("alice".to_string()).subject();
        assert_eq!(s, Subject::User("hub:alice".to_string()));
    }

    #[test]
    fn credentials_do_not_create_principal_actors() {
        let mut caller = CallerContext::from_api_key("pkr_key".into(), vec![]);
        assert_eq!(caller.subject(), Subject::User("local".into()));
        assert_eq!(caller.subject_id(), "local");
        assert_eq!(caller.rate_limit_bucket, "apikey:pkr_key");
        caller.hub_user_id = Some("account".into());
        assert_eq!(caller.subject(), Subject::User("hub:account".into()));
    }
    #[test]
    fn visitor_and_user_tokens_with_same_id_are_distinct() {
        let user = CallerContext::from_jwt("same".into());
        let visitor = CallerContext::from_visitor_jwt("same".into());
        assert_ne!(user.subject(), visitor.subject());
        assert_eq!(visitor.subject(), Subject::Visitor("hub:same".into()));
        assert!(!visitor.is_operator());
    }
    #[test]
    fn local_permission_projection_does_not_change_the_message_actor() {
        let caller = CallerContext::local_with_hub_user_id(Some("account".into()));
        let owner = Subject::User("local".into());
        assert_eq!(caller.permission_subject(&owner), owner);
        assert_eq!(caller.subject(), Subject::User("hub:account".into()));
    }
}
