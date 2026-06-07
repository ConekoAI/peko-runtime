//! Caller context — resolved identity for every incoming request

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
    /// - Local → `local:{runtime_did}` (must be provided by caller)
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
