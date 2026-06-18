//! Authentication and Authorization Module (ADR-034)
//!
//! Provides layered authentication for the pekobot daemon:
//! - Local Trust: Unix socket / localhost UDP (OS trust boundary)
//! - Pekohub JWT: Remote access via pekohub-issued tokens
//! - API Key: Programmatic access with scoped permissions

pub mod api_key;
pub mod caller;
pub mod config;
pub mod jwt;
pub mod ownership;
pub mod permissions;
pub mod principal;
pub mod rate_limit;
pub mod types;

pub use api_key::{ApiKeyStore, ApiKeyVerifier};
pub use caller::{AuthMethod, CallerContext, Identity};
pub use config::{AuthConfig, RateLimitConfig};
pub use jwt::{JwtValidator, ValidatedJwt};
pub use ownership::{
    agent_resource, check_permission as check_ownership_permission, team_resource, Permission,
    PermissionDenied, PermissionGrant, Resource as OwnedResource, SubjectType,
};
pub use permissions::{check_permission, Action, AuthError, Resource};
pub use principal::{Principal, PrincipalParseError, SubjectKind};
pub use rate_limit::{RateLimitEntry, RateLimiter};
pub use types::{ApiKeyEntry, ApiKeyScope, PekohubConfig, PekohubCredential};
