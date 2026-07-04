//! Ownership and Permission Model (ADR-033, ADR-039)
//!
//! Provides per-resource RBAC-lite permission checks that run on the same
//! code path for local IPC and remote access.
//!
//! After ADR-039, the canonical actor is `crate::auth::Subject`.
//! The legacy `SubjectType` enum and `principal_from_wire` helper were
//! removed in issue #30; the IPC wire format now carries a single
//! `subject: Subject` on grant/revoke packets. `PermissionGrant` stores
//! that `Subject` directly.

use serde::{Deserialize, Serialize};

use crate::auth::Subject;

/// Actions that can be performed on agents and teams
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
    /// Add/remove team members
    ManageMembers,
    /// Configure private/public exposure
    Expose,
    /// Delete the resource
    Delete,
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
// grant/revoke packet; see `RequestPacket::resolved_subject` for the
// (now trivial) resolver.

/// A single permission grant on a resource.
///
/// After ADR-039, the subject is a full `Subject`. The IPC wire carries
/// the same `Subject` directly; see `ipc::packet::RequestPacket`.
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
        exposure: crate::tunnel::protocol::InstanceExposure,
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
/// 1. Owner always passes (Subject equality).
/// 2. Look up explicit grants for this subject, plus a `Public` grant
///    applies to any caller.
///
/// # Errors
/// Returns `PermissionDenied` if the caller is not authorized.
pub fn check_permission(
    resource: &Resource,
    action: Permission,
    caller: &Subject,
) -> Result<(), PermissionDenied> {
    // 1. Owner always passes (Subject equality, not string equality).
    if resource.owner() == caller {
        return Ok(());
    }

    // 2. Look up explicit grants for this subject, plus `Public` wildcard.
    for grant in resource.permissions() {
        if !grant.permission.covers(&action) {
            continue;
        }
        if &grant.subject == caller || grant.subject == Subject::Public {
            return Ok(());
        }
    }

    Err(PermissionDenied {
        resource: resource.id(),
        action,
        caller: caller.to_string(),
    })
}

/// Build a `Resource::Principal` from a `PrincipalConfig` and name.
#[must_use]
pub fn principal_resource(name: &str, config: &crate::principal::PrincipalConfig) -> Resource {
    Resource::Principal {
        name: name.to_string(),
        owner: config.owner.clone(),
        permissions: config.permissions.clone(),
        exposure: config.exposure.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_principal_resource_permission_checks() {
        let owner = Subject::User("user:123".into());
        let resource = Resource::Principal {
            name: "alpha".to_string(),
            owner: owner.clone(),
            permissions: vec![],
            exposure: crate::tunnel::protocol::InstanceExposure::Private,
        };

        assert!(check_permission(&resource, Permission::Chat, &owner).is_ok());
        assert!(
            check_permission(&resource, Permission::Chat, &Subject::User("other".into())).is_err()
        );

        let grantee = Subject::User("user:456".into());
        let resource = Resource::Principal {
            name: "alpha".to_string(),
            owner: owner.clone(),
            permissions: vec![PermissionGrant {
                subject: grantee.clone(),
                permission: Permission::Chat,
                granted_at: "2026-06-07T10:00:00Z".to_string(),
                granted_by: owner.clone(),
            }],
            exposure: crate::tunnel::protocol::InstanceExposure::Private,
        };
        assert!(check_permission(&resource, Permission::Chat, &grantee).is_ok());
        assert!(check_permission(&resource, Permission::Delete, &grantee).is_err());
    }

    // Tests for `Resource::Agent` / `Resource::Team` were removed when
    // those variants were dropped (Phases 1–5 of the principal-as-single-
    // actor migration). Cross-kind guard semantics are now exercised by
    // the principal path above; if non-principal resources are added
    // back, port the equivalent guard tests.
}
