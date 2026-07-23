//! Authentication and Authorization Module (ADR-034)
//!
//! Layered authentication for the peko daemon:
//! - Local Trust: Unix socket / localhost UDP (OS trust boundary)
//! - Pekohub JWT: Remote access via pekohub-issued tokens
//! - API Key: Programmatic access with scoped permissions
//!
//! Phase 4 of the post-migration cleanup: this crate replaces the
//! root `src/auth/` directory. The trait ports in [`host`] abstract
//! root-only deps (`PathResolver`, `PrincipalConfig`) so the crate
//! stays a leaf.

pub mod api_key;
pub mod caller;
pub mod config;
pub mod host;
pub mod jwt;
pub mod ownership;
pub mod permissions;
pub mod rate_limit;
pub mod types;

pub use api_key::{ApiKeyStore, ApiKeyVerifier};
pub use caller::{AuthMethod, CallerContext, Identity};
pub use config::{enforce_auth_for_public_bind, is_loopback, AuthConfig, RateLimitConfig};
pub use host::{principal_resource_from_view, Exposure, PrincipalResourceView, RuntimePaths};
pub use jwt::{JwkEntry, JwksResponse, JwtError, JwtValidator, ValidatedJwt};
pub use ownership::{
    check_permission as check_ownership_permission, Permission, PermissionDenied, PermissionGrant,
    Resource as OwnedResource,
};
pub use permissions::{check_permission, Action, AuthError, Resource};
pub use rate_limit::{RateLimitEntry, RateLimiter};
pub use types::{ApiKeyEntry, ApiKeyScope, ApiKeysFile, AuthConfigFile, RateLimitConfigFile};

// Re-export `Subject` from `peko-subject` so downstream callers can
// keep using `peko_auth::Subject` (the historical `crate::auth::Subject`
// path was a re-export of `crate::subject::Subject`).
pub use peko_subject::{
    subject_from_string_with_default_user, Subject, SubjectKind, SubjectParseError,
};
