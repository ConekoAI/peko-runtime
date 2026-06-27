//! Authentication and Authorization Module (ADR-034)
//!
//! Provides layered authentication for the peko daemon:
//! - Local Trust: Unix socket / localhost UDP (OS trust boundary)
//! - Pekohub JWT: Remote access via pekohub-issued tokens
//! - API Key: Programmatic access with scoped permissions

pub mod api_key;
pub mod caller;
pub mod config;
pub mod jwt;
pub mod ownership;
pub mod permissions;
pub mod rate_limit;
pub mod types;

pub use api_key::{ApiKeyStore, ApiKeyVerifier};
pub use caller::{AuthMethod, CallerContext, Identity};
pub use config::{AuthConfig, RateLimitConfig};
pub use jwt::{JwtValidator, ValidatedJwt};
pub use ownership::{
    check_permission as check_ownership_permission, Permission, PermissionDenied,
    PermissionGrant, Resource as OwnedResource,
};
pub use permissions::{check_permission, Action, AuthError, Resource};
pub use rate_limit::{RateLimitEntry, RateLimiter};
pub use types::{ApiKeyEntry, ApiKeyScope, PekohubConfig, PekohubCredential};

// ADR-041: the actor enum formerly named `Principal` is now `Subject`.
// The auth module re-exports it for ergonomic use within the codebase.
pub use crate::subject::{
    subject_from_string_with_default_user, Subject, SubjectKind, SubjectParseError,
};
