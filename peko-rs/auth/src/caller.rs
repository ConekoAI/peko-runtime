//! Caller context — resolved identity for every incoming request

use peko_subject::{PrincipalDID, Subject};

use super::types::ApiKeyScope;

/// Resolved identity of the caller
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    /// Unix socket / localhost UDP — OS is the trust boundary
    Local,
    /// pekohub user (sub claim from JWT)
    User(String),
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
    /// Create a local-trust caller context
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
        let bucket = format!("user:{sub}");
        Self {
            identity: Identity::User(sub),
            auth_method: AuthMethod::PekohubJwt,
            rate_limit_bucket: bucket,
            api_key_scopes: Vec::new(), // N/A for JWT
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

    /// Check if this caller has local trust (owner equivalent)
    #[must_use]
    pub fn is_local(&self) -> bool {
        matches!(self.identity, Identity::Local)
    }

    /// Get the subject ID for ownership/permission checks (ADR-033).
    ///
    /// - Local → `local` (bare token; matches the legacy wire shape)
    /// - User → `user:{sub}`
    /// - ApiKey → `apikey:{key_id}`
    #[must_use]
    pub fn subject_id(&self) -> String {
        match &self.identity {
            Identity::Local => "local".to_string(),
            Identity::User(sub) => format!("user:{sub}"),
            Identity::ApiKey(key_id) => format!("apikey:{key_id}"),
        }
    }

    /// Get the caller's `Subject` projection (ADR-039, ADR-041).
    ///
    /// R5 — typed `Principal` projection for API keys (issue: per-PR
    /// audit): API-key callers are *not* pekohub users, they are a
    /// distinct actor class. They project as `Subject::Principal`
    /// carrying a typed `PrincipalDID` of the form `apikey:{key_id}`.
    /// `Local` and `User` retain their pre-R5 wire-compatible
    /// `Subject::User` form because their `subject_id()` text is what
    /// the on-disk grant list expects.
    ///
    /// Equality across kinds is intentionally prevented
    /// (`Subject::User("apikey:...") != Subject::Principal(...)` of the
    /// same id string) — switching to the typed projection means a
    /// grant issued for an API key cannot accidentally match a user
    /// with the same id, and vice versa.
    #[must_use]
    pub fn subject(&self) -> Subject {
        match &self.identity {
            Identity::Local => Subject::User("local".to_string()),
            Identity::User(sub) => Subject::User(format!("user:{sub}")),
            Identity::ApiKey(key_id) => {
                Subject::Principal(PrincipalDID(format!("apikey:{key_id}")))
            }
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

    /// Check if this caller's API key scopes include the given scope.
    /// Always returns true for Local and JWT identities.
    #[must_use]
    pub fn has_scope(&self, scope: &ApiKeyScope) -> bool {
        match self.identity {
            Identity::Local | Identity::User(_) => true,
            Identity::ApiKey(_) => self.api_key_scopes.contains(scope),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_subject::{PrincipalDID, SubjectKind};

    #[test]
    fn local_subject_is_user_local() {
        let s = CallerContext::local().subject();
        assert_eq!(s, Subject::User("local".to_string()));
    }

    #[test]
    fn jwt_subject_is_user_with_sub() {
        let s = CallerContext::from_jwt("alice".to_string()).subject();
        assert_eq!(s, Subject::User("user:alice".to_string()));
    }

    /// R5: API-key callers project as `Subject::Principal` carrying a
    /// typed `PrincipalDID` of the form `apikey:{key_id}`. Pin the
    /// kind + inner id separately — both halves are load-bearing for
    /// the audit trail (kind tag) and the permission grant match
    /// (subject_id() text).
    #[test]
    fn api_key_subject_is_principal_typed() {
        let s = CallerContext::from_api_key("pkr_abc123".to_string(), vec![]).subject();
        assert_eq!(
            s,
            Subject::Principal(PrincipalDID("apikey:pkr_abc123".to_string()))
        );
        assert_eq!(s.kind(), SubjectKind::Principal);
        assert_eq!(s.subject_id(), "apikey:pkr_abc123");
    }

    /// R5 cross-kind guard: a `Principal("apikey:foo")` is *not*
    /// equal to `User("apikey:foo")` even though their `subject_id()`
    /// texts are identical. The typed projection prevents a grant
    /// issued for an API key from accidentally matching a (hypothetical)
    /// user with the same id string, and vice versa.
    #[test]
    fn api_key_principal_subject_is_not_equal_to_user_with_same_id() {
        let principal =
            Subject::Principal(PrincipalDID("apikey:pkr_abc123".to_string()));
        let user = Subject::User("apikey:pkr_abc123".to_string());
        assert_ne!(
            principal, user,
            "cross-kind equality must be rejected (kind is part of identity)"
        );
        // subject_id() text equality is preserved — grants keyed on the
        // text match regardless of kind, so legacy text-keyed grants
        // still find api-key rows.
        assert_eq!(principal.subject_id(), user.subject_id());
    }

    /// subject_id() text must remain stable across the R5 change so
    /// the on-disk grant list (which keys on the bare `apikey:{key_id}`
    /// text) keeps matching API-key callers without a migration. The
    /// audit trail's `caller_subject.subject_id()` field stays as
    /// `apikey:...` for both kinds.
    #[test]
    fn api_key_subject_id_text_matches_legacy_wire_shape() {
        let c = CallerContext::from_api_key("pkr_xyz".to_string(), vec![]);
        assert_eq!(c.subject_id(), "apikey:pkr_xyz");
        assert_eq!(c.subject().subject_id(), "apikey:pkr_xyz");
    }

    /// `Subject::Display` (audit log + tunnel wire) now emits
    /// `principal:apikey:{key_id}` for API-key callers (was
    /// `user:apikey:{key_id}` pre-R5). Pin the new shape so the change
    /// is intentional, not silent.
    #[test]
    fn api_key_subject_display_uses_principal_prefix() {
        let s = CallerContext::from_api_key("pkr_xyz".to_string(), vec![]).subject();
        assert_eq!(s.to_string(), "principal:apikey:pkr_xyz");
    }
}
