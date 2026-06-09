//! Permission checks (ADR-034 integration)
//!
//! Coarse-grained permissions: read, write, admin.
//! Per-resource ACLs are future work.

use super::caller::CallerContext;
use super::types::ApiKeyScope;

/// Actions that can be performed on resources
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Read agents, sessions, teams, extensions
    Read,
    /// Create, update, delete agents, sessions, teams, extensions
    Write,
    /// Administrative operations (system clean, shutdown, runtime config)
    Admin,
    /// Execute agent messages
    Execute,
}

impl Action {
    /// Get the API key scope required for this action
    #[must_use]
    pub fn required_scope(&self) -> ApiKeyScope {
        match self {
            Self::Read => ApiKeyScope::Read,
            Self::Write => ApiKeyScope::Write,
            Self::Admin => ApiKeyScope::Admin,
            Self::Execute => ApiKeyScope::Write,
        }
    }
}

/// Resource being accessed
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Agent resource
    Agent { name: String, team: Option<String> },
    /// Team resource
    Team { name: String },
    /// Session resource
    Session { id: String },
    /// Extension resource
    Extension { id: String },
    /// System-level resource
    System,
    /// Runtime-level resource
    Runtime,
}

/// Authentication/authorization errors
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    PermissionDenied,
    InvalidCredential,
    ExpiredCredential,
    RateLimited,
    LocalTrustDisabled,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PermissionDenied => write!(f, "Permission denied"),
            Self::InvalidCredential => write!(f, "Invalid credential"),
            Self::ExpiredCredential => write!(f, "Expired credential"),
            Self::RateLimited => write!(f, "Rate limit exceeded"),
            Self::LocalTrustDisabled => {
                write!(f, "Local trust is disabled for non-localhost binds")
            }
        }
    }
}

impl std::error::Error for AuthError {}

/// Check if a caller is permitted to perform an action.
///
/// - Local trust = owner, all actions allowed.
/// - JWT users = full access (when JWT is enabled; currently disabled in v0.1.0).
/// - API keys = checked against their scopes stored in `CallerContext`.
pub fn check_permission(
    caller: &CallerContext,
    _resource: &Resource,
    action: Action,
) -> Result<(), AuthError> {
    let required = action.required_scope();
    if caller.has_scope(&required) {
        Ok(())
    } else {
        Err(AuthError::PermissionDenied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_trust_always_allowed() {
        let caller = CallerContext::local();
        assert!(check_permission(&caller, &Resource::System, Action::Admin).is_ok());
    }

    #[test]
    fn test_api_key_read_allowed() {
        let caller = CallerContext::from_api_key("pkr_abc123".to_string(), vec![ApiKeyScope::Read]);
        assert!(check_permission(&caller, &Resource::System, Action::Read).is_ok());
    }

    #[test]
    fn test_api_key_write_denied_without_write_scope() {
        let caller = CallerContext::from_api_key("pkr_abc123".to_string(), vec![ApiKeyScope::Read]);
        assert_eq!(
            check_permission(&caller, &Resource::System, Action::Write).unwrap_err(),
            AuthError::PermissionDenied
        );
    }

    #[test]
    fn test_api_key_write_allowed_with_write_scope() {
        let caller = CallerContext::from_api_key(
            "pkr_abc123".to_string(),
            vec![ApiKeyScope::Read, ApiKeyScope::Write],
        );
        assert!(check_permission(&caller, &Resource::System, Action::Write).is_ok());
    }

    #[test]
    fn test_api_key_admin_denied_without_admin_scope() {
        let caller = CallerContext::from_api_key(
            "pkr_abc123".to_string(),
            vec![ApiKeyScope::Read, ApiKeyScope::Write],
        );
        assert_eq!(
            check_permission(&caller, &Resource::System, Action::Admin).unwrap_err(),
            AuthError::PermissionDenied
        );
    }

    #[test]
    fn test_api_key_admin_allowed_with_admin_scope() {
        let caller =
            CallerContext::from_api_key("pkr_abc123".to_string(), vec![ApiKeyScope::Admin]);
        assert!(check_permission(&caller, &Resource::System, Action::Admin).is_ok());
    }

    #[test]
    fn test_caller_has_scope_local() {
        let caller = CallerContext::local();
        assert!(caller.has_scope(&ApiKeyScope::Read));
        assert!(caller.has_scope(&ApiKeyScope::Write));
        assert!(caller.has_scope(&ApiKeyScope::Admin));
    }

    #[test]
    fn test_caller_has_scope_jwt() {
        let caller = CallerContext::from_jwt("user123".to_string());
        assert!(caller.has_scope(&ApiKeyScope::Read));
        assert!(caller.has_scope(&ApiKeyScope::Write));
        assert!(caller.has_scope(&ApiKeyScope::Admin));
    }

    #[test]
    fn test_caller_has_scope_api_key() {
        let caller = CallerContext::from_api_key("pkr_abc".to_string(), vec![ApiKeyScope::Read]);
        assert!(caller.has_scope(&ApiKeyScope::Read));
        assert!(!caller.has_scope(&ApiKeyScope::Write));
        assert!(!caller.has_scope(&ApiKeyScope::Admin));
    }
}
