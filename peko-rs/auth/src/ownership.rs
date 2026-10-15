//! Ownership and Permission Model (ADR-033, ADR-039)
//!
//! Provides per-resource RBAC-lite permission checks that run on the same
//! code path for local IPC and remote access.
//!
//! After ADR-039, the canonical actor is `peko_auth::Subject` (re-exported
//! from `peko_subject::Subject`). The legacy `SubjectType` enum and
//! `principal_from_wire` helper were removed in issue #30; the IPC wire
//! format now carries a single `subject: Subject` on grant/revoke
//! packets. `PermissionGrant` stores that `Subject` directly.

use peko_subject::{ActorKeys, Subject};
use serde::{Deserialize, Serialize};

use crate::host::{Exposure, PrincipalResourceView};

/// Actions that can be performed on principals
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Send messages / chat
    Chat,
    /// Read config and settings
    ViewSettings,
    /// Write config and settings
    ManageSettings,
    /// Enable/disable extensions
    ManageExtensions,
    /// Add/remove principal delegates
    ManageMembers,
    /// Configure private/public exposure
    Expose,
    /// Delete the resource
    Delete,
    /// The owner's consent to trust this actor with the principal's full
    /// tool surface (ADR-078 §2). Never matched through a `Public` grant.
    Trusted,
}

impl Permission {
    /// Check if this permission covers another permission.
    ///
    /// In v0.1.0 permissions are atomic — a grant for `ManageSettings`
    /// does not imply `ViewSettings`. This may change in future ADRs.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

// `SubjectType` and `principal_from_wire` were removed in issue #30.
// The IPC wire format now carries a single `subject: Subject` per
// grant/revoke packet; see `peko_protocol::RequestPacket::resolved_subject`
// for the (now trivial) resolver.

/// A single permission grant on a resource.
///
/// After ADR-039, the subject is a full `Subject`. The IPC wire carries
/// the same `Subject` directly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionGrant {
    /// The subject this grant applies to.
    pub subject: Subject,
    /// Granted permission
    pub permission: Permission,
    /// ISO 8601 timestamp
    pub granted_at: String,
    /// Granter's identity.
    pub granted_by: Subject,
}

/// Resource being accessed — currently only a Principal. Agent/Team
/// variants were removed when the standalone agent CRUD surface was
/// rescoped into Principal-as-single-actor (Phases 1–5 of the
/// `parallel-sauteeing-gadget` plan). If non-principal resources ever
/// reappear (e.g. delegated subresources), add the variants back here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Principal resource with ownership, permission data, and exposure.
    Principal {
        name: String,
        owner: Subject,
        permissions: Vec<PermissionGrant>,
        exposure: Exposure,
    },
}

impl Resource {
    /// Get the resource identifier for error messages
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Principal { name, .. } => format!("principal:{name}"),
        }
    }

    /// Get the owner of this resource.
    #[must_use]
    pub fn owner(&self) -> &Subject {
        match self {
            Self::Principal { owner, .. } => owner,
        }
    }

    /// Get the explicit permission grants on this resource
    #[must_use]
    pub fn permissions(&self) -> &[PermissionGrant] {
        match self {
            Self::Principal { permissions, .. } => permissions,
        }
    }
}

/// Error when a permission check fails
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionDenied {
    pub resource: String,
    pub action: Permission,
    pub caller: String,
}

impl std::fmt::Display for PermissionDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Permission denied: {} cannot perform {:?} on {}",
            self.caller, self.action, self.resource
        )
    }
}

impl std::error::Error for PermissionDenied {}

/// Check if a caller is permitted to perform an action on a resource.
///
/// 1. The owner always passes.
/// 2. Otherwise an explicit grant for the caller, or a `Public` grant,
///    must cover the action.
///
/// Identity is decided by `keys` (ADR-081 D1): every alias of one actor
/// matches, and equal ID text in different scopes never does.
///
/// # Errors
/// Returns `PermissionDenied` if the caller is not authorized.
pub fn check_permission(
    resource: &Resource,
    action: Permission,
    caller: &Subject,
    keys: &dyn ActorKeys,
) -> Result<(), PermissionDenied> {
    if keys.same_actor(resource.owner(), caller) {
        return Ok(());
    }
    for grant in resource.permissions() {
        if !grant.permission.covers(&action) {
            continue;
        }
        if grant.subject == Subject::Public || keys.same_actor(&grant.subject, caller) {
            return Ok(());
        }
    }

    Err(PermissionDenied {
        resource: resource.id(),
        action,
        caller: caller.canonical_address(),
    })
}

/// Build a `Resource::Principal` from any [`PrincipalResourceView`]
/// implementor.
///
/// Phase 4 migration: the function used to take `&PrincipalConfig`
/// directly, which created a `peko-auth ↔ peko-principal` cycle when
/// both became workspace crates. The trait port (declared in
/// [`crate::host`]) breaks the cycle by giving `peko-auth` a narrow
/// view over only the four fields this function needs. The principal
/// module implements the trait in root (`src/auth_compat.rs`).
#[must_use]
pub fn principal_resource(view: &dyn PrincipalResourceView) -> Resource {
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
    use peko_subject::CanonicalKeys;

    /// In-memory `PrincipalResourceView` mock so the leaf crate's
    /// tests don't need the root `PrincipalConfig` type.
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

    fn grant(subject: Subject, permission: Permission) -> PermissionGrant {
        PermissionGrant {
            subject,
            permission,
            granted_at: "2026-06-07T10:00:00Z".to_string(),
            granted_by: Subject::User("hub:123".into()),
        }
    }

    fn check(view: &MockPrincipal, action: Permission, caller: &Subject) -> bool {
        check_permission(&principal_resource(view), action, caller, &CanonicalKeys).is_ok()
    }

    #[test]
    fn test_principal_resource_permission_checks() {
        let owner = Subject::User("hub:123".into());
        let view = MockPrincipal {
            name: "alpha".to_string(),
            owner: owner.clone(),
            permissions: vec![],
            exposure: Exposure::Private,
        };
        assert!(check(&view, Permission::Chat, &owner));
        // Equal ID text in another scope is a different actor.
        assert!(!check(
            &view,
            Permission::Chat,
            &Subject::User("local:123".into())
        ));
        assert!(!check(
            &view,
            Permission::Chat,
            &Subject::User("other".into())
        ));

        let grantee = Subject::User("hub:456".into());
        let view = MockPrincipal {
            permissions: vec![grant(grantee.clone(), Permission::Chat)],
            ..view
        };
        assert!(check(&view, Permission::Chat, &grantee));
        // The retired `local_<id>` spelling no longer aliases a Hub account.
        assert!(!check(
            &view,
            Permission::Chat,
            &Subject::User("local_456".into())
        ));
        assert!(!check(
            &view,
            Permission::Chat,
            &Subject::User("local:456".into())
        ));
        for other_kind in [
            Subject::Principal("hub:456".into()),
            Subject::Visitor("hub:456".into()),
        ] {
            assert!(!check(&view, Permission::Chat, &other_kind));
        }
        assert!(!check(&view, Permission::Delete, &grantee));
    }

    /// Every alias a registry maps to one key matches the same grant.
    #[test]
    fn grants_match_through_actor_keys() {
        struct Registry;
        impl ActorKeys for Registry {
            fn key_of(&self, subject: &Subject) -> peko_subject::ActorKey {
                match subject.canonical_address().as_str() {
                    "principal:local:prin_y" | "principal:hub:v" => {
                        peko_subject::ActorKey("key-y".into())
                    }
                    other => peko_subject::ActorKey(other.into()),
                }
            }
        }
        let view = MockPrincipal {
            name: "alpha".to_string(),
            owner: Subject::User("local".into()),
            permissions: vec![grant(Subject::Principal("hub:v".into()), Permission::Chat)],
            exposure: Exposure::Private,
        };
        let resource = principal_resource(&view);
        let local = Subject::Principal("prin_y".into());
        assert!(check_permission(&resource, Permission::Chat, &local, &Registry).is_ok());
        assert!(check_permission(&resource, Permission::Chat, &local, &CanonicalKeys).is_err());
        // `Public` is a wildcard grant, never an owner or an actor.
        assert!(!Registry.same_actor(&Subject::Public, &Subject::Public));
    }

    // Tests for `Resource::Agent` / `Resource::Team` were removed when
    // those variants were dropped (Phases 1–5 of the principal-as-single-
    // actor migration). Cross-kind guard semantics are now exercised by
    // the principal path above; if non-principal resources are added
    // back, port the equivalent guard tests.
}
