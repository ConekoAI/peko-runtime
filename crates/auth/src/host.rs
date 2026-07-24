//! Trait ports that abstract root-only deps consumed by `peko-auth`.
//!
//! `peko-auth` is a leaf crate — it must not depend on root or on any
//! other workspace crate that has root in its dep tree (e.g.
//! `peko-engine`, `peko-agents`, `peko-session`, `peko-extension-host`,
//! `peko-providers`). Two narrow traits cover everything auth needs
//! from the host:
//!
//! - [`RuntimePaths`] — abstracts `crate::common::paths::PathResolver`.
//!   `AuthConfig::load` and `ApiKeyStore::load` need the runtime
//!   directory to locate `auth_config.toml` / `api_keys.toml`. The
//!   implementor is `PathResolver` in root.
//! - [`PrincipalResourceView`] — abstracts the `PrincipalConfig` fields
//!   that `auth::ownership::principal_resource` reads to build an
//!   `auth::Resource::Principal` value. Breaks the
//!   `peko-auth ↔ peko-principal` cycle: auth does not import
//!   `PrincipalConfig`; principal implements the trait in root.
//!
//! Both traits are `Send + Sync + 'static` so they flow through
//! `Arc<dyn ...>` ownership shared between daemon and CLI.
//!
//! ## Orphan rule + impl location
//!
//! Rust's orphan rule forbids `impl ForeignTrait for ForeignType` in a
//! third crate. These traits live in `peko-auth`, but the
//! implementing types (`crate::common::paths::PathResolver`,
//! `peko_principal::config::PrincipalConfig`) live in root. The
//! implementations live in root's `src/auth_compat.rs`, with the
//! local-type side satisfying the orphan rule.
//!
//! ## Why `Exposure` lives in `peko-auth`
//!
//! `auth::Resource::Principal` embeds `Exposure` directly in its enum
//! variant, so the type must live in the same crate as `Resource`.
//! The principal module imports it from `peko_auth::Exposure` for
//! its `PrincipalConfig.exposure` field. This also breaks the cycle:
//! auth owns the wire-stable projection; principal does not need a
//! separate definition.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ownership::{PermissionGrant, Resource};
use peko_subject::Subject;

/// Minimal path-resolver surface needed by auth config / API key
/// store loaders.
///
/// `crate::common::paths::PathResolver` implements this trait in
/// root (`src/auth_compat.rs`). `peko-auth` never touches the concrete
/// `PathResolver` type.
///
/// Note: this trait has the same single-method shape as
/// `peko_identity::host::RuntimePaths` but is declared in
/// `peko-auth` rather than shared, because:
///
/// 1. Auth does not depend on `peko-identity` (and adding that
///    edge would force a workspace-graph change for a trivial
///    method-shape overlap).
/// 2. Keeping the trait in the consumer crate is the simpler
///    ownership contract — auth owns the trait, root owns the impl.
pub trait RuntimePaths: Send + Sync + 'static {
    /// The `{config_dir}/runtime` directory used to persist
    /// `auth_config.toml`, `api_keys.toml`, and `pekohub.toml`.
    fn runtime_dir(&self) -> PathBuf;
}

/// Network exposure level for a Principal (persisted in
/// `principal.toml`).
///
/// This used to live in `peko_principal::config::Exposure` and was
/// imported by `auth::Resource::Principal` to fill the `exposure`
/// field. Moving it to `peko-auth` makes `Resource` self-contained
/// (no inbound dep from peko-auth to peko-principal) and lets
/// `peko-principal` (the future Phase 14 extraction) import it from
/// `peko_auth` without a cycle.
///
/// The wire shape is unchanged: `snake_case` serde with
/// `private` / `public` / `unexposed` variants. Persisted
/// `principal.toml` files keep loading without migration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exposure {
    Private,
    Public,
    #[default]
    Unexposed,
}

impl std::fmt::Display for Exposure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Private => "private",
            Self::Public => "public",
            Self::Unexposed => "unexposed",
        };
        f.write_str(s)
    }
}

/// Narrow view over a `PrincipalConfig` exposing only the fields
/// `auth::ownership::principal_resource` needs to build a
/// `Resource::Principal`.
///
/// Implementor is `peko_principal::config::PrincipalConfig` in
/// root. The trait port breaks the
/// `peko-auth ↔ peko-principal` cycle: auth does not import the
/// concrete `PrincipalConfig`; principal implements the trait in root.
///
/// ## Method shape vs. the underlying type
///
/// The view intentionally returns borrowed slices / by-value copies
/// of small enums. `name` is borrowed because the caller (auth)
/// already has the name as a `&str` parameter; the other three
/// are returned by value because the principal module owns the
/// storage and the auth side clones them into a fresh `Resource`.
pub trait PrincipalResourceView: Send + Sync + 'static {
    /// Stable principal name (e.g. `"alpha"`).
    fn name(&self) -> &str;

    /// Subject who owns this principal (used as the `Resource::owner`
    /// for the `check_permission` owner-pass).
    fn owner(&self) -> &Subject;

    /// Explicit permission grants on this principal.
    fn permissions(&self) -> &[PermissionGrant];

    /// Current network exposure level.
    fn exposure(&self) -> Exposure;
}

/// Convenience constructor: build a `Resource::Principal` from any
/// `PrincipalResourceView` implementor. Lives in the trait-ports
/// module rather than `auth::ownership` so the leaf crate never
/// imports the concrete `PrincipalConfig` type.
#[must_use]
pub fn principal_resource_from_view<V: PrincipalResourceView + ?Sized>(view: &V) -> Resource {
    Resource::Principal {
        name: view.name().to_string(),
        owner: view.owner().clone(),
        permissions: view.permissions().to_vec(),
        exposure: view.exposure(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory `PrincipalResourceView` mock. Mirrors the principal
    /// config shape but lives entirely in this test module so the
    /// leaf crate does not depend on the root `PrincipalConfig` type.
    struct MockPrincipal {
        name: String,
        owner: Subject,
        permissions: Vec<PermissionGrant>,
        exposure: Exposure,
    }

    impl PrincipalResourceView for MockPrincipal {
        fn name(&self) -> &str {
            &self.name
        }
        fn owner(&self) -> &Subject {
            &self.owner
        }
        fn permissions(&self) -> &[PermissionGrant] {
            &self.permissions
        }
        fn exposure(&self) -> Exposure {
            self.exposure
        }
    }

    #[test]
    fn exposure_default_is_unexposed() {
        assert_eq!(Exposure::default(), Exposure::Unexposed);
    }

    #[test]
    fn exposure_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&Exposure::Private).unwrap(),
            "\"private\""
        );
        assert_eq!(
            serde_json::to_string(&Exposure::Public).unwrap(),
            "\"public\""
        );
        assert_eq!(
            serde_json::to_string(&Exposure::Unexposed).unwrap(),
            "\"unexposed\""
        );
    }

    #[test]
    fn exposure_deserializes_snake_case() {
        assert_eq!(
            serde_json::from_str::<Exposure>("\"private\"").unwrap(),
            Exposure::Private
        );
        assert_eq!(
            serde_json::from_str::<Exposure>("\"public\"").unwrap(),
            Exposure::Public
        );
    }

    #[test]
    fn principal_resource_from_view_populates_resource() {
        let owner = Subject::User("user:1".into());
        let grant = PermissionGrant {
            subject: Subject::User("user:2".into()),
            permission: crate::ownership::Permission::Chat,
            granted_at: "2026-07-21T00:00:00Z".to_string(),
            granted_by: owner.clone(),
        };
        let view = MockPrincipal {
            name: "alpha".to_string(),
            owner: owner.clone(),
            permissions: vec![grant.clone()],
            exposure: Exposure::Public,
        };
        let resource = principal_resource_from_view(&view);
        match resource {
            Resource::Principal {
                name,
                owner: r_owner,
                permissions,
                exposure,
            } => {
                assert_eq!(name, "alpha");
                assert_eq!(r_owner, owner);
                assert_eq!(permissions, vec![grant]);
                assert_eq!(exposure, Exposure::Public);
            }
            // `_` arm is unreachable today but kept as a defensive
            // trip-wire if the `Resource` enum ever grows new
            // variants that are NOT carrying a principal.
            #[allow(unreachable_patterns)]
            _ => panic!("expected Resource::Principal"),
        }
    }

    #[test]
    fn runtime_paths_is_object_safe() {
        // Compile-time check that the trait can be used as
        // `Arc<dyn RuntimePaths>`. The function pointer does not
        // need to be called — the type assertion is what matters.
        fn _assert_object_safe(_: &dyn RuntimePaths) {}
        // Avoid the unused-import warning for HashMap in tests.
        let _ = HashMap::<String, String>::new();
    }
}
