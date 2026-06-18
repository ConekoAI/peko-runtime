//! Ownership and Permission Model (ADR-033, ADR-039)
//!
//! Provides per-resource RBAC-lite permission checks that run on the same
//! code path for local IPC and remote access.
//!
//! After ADR-039, the canonical actor is `crate::auth::principal::Principal`.
//! `SubjectType` is retained as the IPC wire-side tag for back-compat
//! (see `RequestPacket`); the in-memory `PermissionGrant` collapses
//! `subject_id + subject_type` into a single `subject: Principal`.

use serde::{Deserialize, Serialize};

use crate::auth::principal::Principal;

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

/// Type tag for a subject in a permission grant. Used on the IPC wire.
///
/// After ADR-039, the in-memory representation is a full `Principal`;
/// `SubjectType` is kept for back-compat with the IPC `RequestPacket`
/// shape, which still carries `(subject_id, subject_type)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectType {
    /// A pekohub user or local DID
    User,
    /// A peko agent instance (added in ADR-039)
    Agent,
    /// A team (all members get the permission)
    Team,
    /// Unauthenticated public access
    Public,
}

impl SubjectType {
    /// Convert a `SubjectType` wire tag to the corresponding kind.
    #[must_use]
    pub fn kind(self) -> crate::auth::principal::SubjectKind {
        match self {
            Self::User => crate::auth::principal::SubjectKind::User,
            Self::Agent => crate::auth::principal::SubjectKind::Agent,
            Self::Team => crate::auth::principal::SubjectKind::Team,
            Self::Public => crate::auth::principal::SubjectKind::Public,
        }
    }
}

/// Build a `Principal` from a `(subject_id, subject_type)` IPC pair.
#[must_use]
pub fn principal_from_wire(subject_id: &str, subject_type: SubjectType) -> Principal {
    match subject_type {
        SubjectType::User => Principal::User(subject_id.to_string()),
        SubjectType::Agent => Principal::Agent(subject_id.to_string()),
        SubjectType::Team => Principal::Team(subject_id.to_string()),
        SubjectType::Public => Principal::Public,
    }
}

/// A single permission grant on a resource.
///
/// After ADR-039, the subject is a full `Principal`. The IPC wire still
/// carries `(subject_id, subject_type)` for back-compat — the bridge is
/// in `ipc/server.rs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionGrant {
    /// The subject this grant applies to.
    pub subject: Principal,
    /// Granted permission
    pub permission: Permission,
    /// ISO 8601 timestamp
    pub granted_at: String,
    /// Granter's identity.
    pub granted_by: Principal,
}

/// Resource being accessed — either an agent or a team
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Agent resource with ownership and permission data
    Agent {
        name: String,
        owner: Principal,
        permissions: Vec<PermissionGrant>,
    },
    /// Team resource with ownership, permission data, and member roles
    Team {
        name: String,
        owner: Principal,
        permissions: Vec<PermissionGrant>,
        members: Vec<crate::common::types::membership::TeamMember>,
    },
}

impl Resource {
    /// Get the resource identifier for error messages
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Agent { name, .. } => format!("agent:{name}"),
            Self::Team { name, .. } => format!("team:{name}"),
        }
    }

    /// Get the owner of this resource.
    #[must_use]
    pub fn owner(&self) -> &Principal {
        match self {
            Self::Agent { owner, .. } | Self::Team { owner, .. } => owner,
        }
    }

    /// Get the explicit permission grants on this resource
    #[must_use]
    pub fn permissions(&self) -> &[PermissionGrant] {
        match self {
            Self::Agent { permissions, .. } => permissions,
            Self::Team { permissions, .. } => permissions,
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
/// 1. Owner always passes (Principal equality).
/// 2. Look up explicit grants for this subject, plus a `Public` grant
///    applies to any caller.
/// 3. For team-scoped actions, check team member role.
///
/// # Errors
/// Returns `PermissionDenied` if the caller is not authorized.
pub fn check_permission(
    resource: &Resource,
    action: Permission,
    caller: &Principal,
) -> Result<(), PermissionDenied> {
    // 1. Owner always passes (Principal equality, not string equality).
    if resource.owner() == caller {
        return Ok(());
    }

    // 2. Look up explicit grants for this subject, plus `Public` wildcard.
    for grant in resource.permissions().iter() {
        if !grant.permission.covers(&action) {
            continue;
        }
        if &grant.subject == caller || grant.subject == Principal::Public {
            return Ok(());
        }
    }

    // 3. For team-scoped actions, check team member role.
    //    Team members are `Principal::Agent(member.agent)`.
    if let Resource::Team { members, .. } = resource {
        let caller_agent_name = match caller {
            Principal::Agent(name) => Some(name.as_str()),
            _ => None,
        };
        if let Some(name) = caller_agent_name {
            if let Some(member) = members.iter().find(|m| m.agent == name) {
                use crate::common::types::membership::MembershipRole;
                let role_covers = match member.role {
                    MembershipRole::Owner => true,
                    MembershipRole::Admin => {
                        matches!(
                            action,
                            Permission::Chat
                                | Permission::ViewSettings
                                | Permission::ManageExtensions
                                | Permission::ManageMembers
                        )
                    }
                    MembershipRole::Member => {
                        matches!(action, Permission::Chat | Permission::ViewSettings)
                    }
                };
                if role_covers {
                    return Ok(());
                }
            }
        }
    }

    Err(PermissionDenied {
        resource: resource.id(),
        action,
        caller: caller.to_string(),
    })
}

/// Build a `Resource::Agent` from an `AgentConfig` and name.
#[must_use]
pub fn agent_resource(name: &str, config: &crate::types::agent::AgentConfig) -> Resource {
    Resource::Agent {
        name: name.to_string(),
        owner: config.owner.clone(),
        permissions: config.permissions.clone(),
    }
}

/// Build a `Resource::Team` from a `TeamMetadata`, members, and name.
#[must_use]
pub fn team_resource(
    name: &str,
    metadata: &crate::common::types::team::TeamMetadata,
    members: Vec<crate::common::types::membership::TeamMember>,
) -> Resource {
    Resource::Team {
        name: name.to_string(),
        owner: metadata.owner.clone(),
        permissions: metadata.permissions.clone(),
        members,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::membership::{MembershipRole, TeamMember};

    #[test]
    fn test_owner_always_allowed() {
        let owner = Principal::User("user:123".into());
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner: owner.clone(),
            permissions: vec![],
        };
        assert!(check_permission(&resource, Permission::Delete, &owner).is_ok());
        assert!(check_permission(&resource, Permission::Chat, &owner).is_ok());
    }

    #[test]
    fn test_explicit_grant_allows() {
        let owner = Principal::User("user:123".into());
        let grantee = Principal::User("user:456".into());
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner: owner.clone(),
            permissions: vec![PermissionGrant {
                subject: grantee.clone(),
                permission: Permission::Chat,
                granted_at: "2026-06-07T10:00:00Z".to_string(),
                granted_by: owner.clone(),
            }],
        };
        assert!(check_permission(&resource, Permission::Chat, &grantee).is_ok());
        assert!(check_permission(&resource, Permission::Delete, &grantee).is_err());
    }

    #[test]
    fn test_public_grant_allows_unauthenticated() {
        let owner = Principal::User("user:123".into());
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner,
            permissions: vec![PermissionGrant {
                subject: Principal::Public,
                permission: Permission::Chat,
                granted_at: "2026-06-07T11:00:00Z".to_string(),
                granted_by: Principal::User("user:123".into()),
            }],
        };
        let anyone = Principal::User("anyone".into());
        assert!(check_permission(&resource, Permission::Chat, &anyone).is_ok());
        assert!(check_permission(&resource, Permission::Delete, &anyone).is_err());
    }

    #[test]
    fn test_non_owner_without_grant_denied() {
        let owner = Principal::User("user:123".into());
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner,
            permissions: vec![],
        };
        let stranger = Principal::User("user:999".into());
        assert!(check_permission(&resource, Permission::Chat, &stranger).is_err());
    }

    // -- ADR-039 acceptance-criteria tests --

    #[test]
    fn test_agent_caller_denied_for_user_owned_resource() {
        // Owner is a User; caller is an Agent; action is Delete.
        // Agent is not the owner (cross-kind), and there's no grant,
        // so this MUST be denied.
        let owner = Principal::User("user:123".into());
        let caller = Principal::Agent("helper".into());
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner,
            permissions: vec![],
        };
        let result = check_permission(&resource, Permission::Delete, &caller);
        assert!(
            result.is_err(),
            "agent caller must be denied for user-owned resource"
        );
    }

    #[test]
    fn test_agent_caller_allowed_for_agent_owned_resource() {
        // Owner is an Agent; caller is the SAME Agent; any action allowed
        // because the owner always passes.
        let owner = Principal::Agent("helper".into());
        let resource = Resource::Agent {
            name: "helper".to_string(),
            owner: owner.clone(),
            permissions: vec![],
        };
        assert!(check_permission(&resource, Permission::Delete, &owner).is_ok());
        assert!(check_permission(&resource, Permission::Chat, &owner).is_ok());
    }

    #[test]
    fn test_agent_caller_denied_for_other_agent_owned_resource() {
        // Owner is Agent "helper"; caller is Agent "other"; no grant.
        // Cross-id, cross-kind check — must be denied.
        let owner = Principal::Agent("helper".into());
        let caller = Principal::Agent("other".into());
        let resource = Resource::Agent {
            name: "helper".to_string(),
            owner,
            permissions: vec![],
        };
        assert!(check_permission(&resource, Permission::Delete, &caller).is_err());
    }

    // -- Team role tests (rewritten to use Principal) --

    #[test]
    fn test_team_role_member_allows_chat_and_view() {
        let owner = Principal::User("user:123".into());
        let caller = Principal::Agent("alice".into());
        let resource = Resource::Team {
            name: "engineering".to_string(),
            owner,
            permissions: vec![],
            members: vec![TeamMember {
                agent: "alice".to_string(),
                joined_at: "2026-06-06T10:00:00Z".to_string(),
                role: MembershipRole::Member,
            }],
        };
        assert!(check_permission(&resource, Permission::Chat, &caller).is_ok());
        assert!(check_permission(&resource, Permission::ViewSettings, &caller).is_ok());
        assert!(check_permission(&resource, Permission::ManageMembers, &caller).is_err());
    }

    #[test]
    fn test_team_role_admin_allows_more() {
        let owner = Principal::User("user:123".into());
        let caller = Principal::Agent("alice".into());
        let resource = Resource::Team {
            name: "engineering".to_string(),
            owner,
            permissions: vec![],
            members: vec![TeamMember {
                agent: "alice".to_string(),
                joined_at: "2026-06-06T10:00:00Z".to_string(),
                role: MembershipRole::Admin,
            }],
        };
        assert!(check_permission(&resource, Permission::Chat, &caller).is_ok());
        assert!(check_permission(&resource, Permission::ManageMembers, &caller).is_ok());
        assert!(check_permission(&resource, Permission::Delete, &caller).is_err());
    }

    #[test]
    fn test_team_non_member_denied() {
        let owner = Principal::User("user:123".into());
        let caller = Principal::Agent("bob".into());
        let resource = Resource::Team {
            name: "engineering".to_string(),
            owner,
            permissions: vec![],
            members: vec![],
        };
        assert!(check_permission(&resource, Permission::Chat, &caller).is_err());
    }

    #[test]
    fn test_team_user_caller_does_not_match_agent_members() {
        // A User principal named "alice" must NOT be treated as the agent
        // member "alice" — cross-kind guard.
        let owner = Principal::User("user:123".into());
        let caller = Principal::User("alice".into());
        let resource = Resource::Team {
            name: "engineering".to_string(),
            owner,
            permissions: vec![],
            members: vec![TeamMember {
                agent: "alice".to_string(),
                joined_at: "2026-06-06T10:00:00Z".to_string(),
                role: MembershipRole::Admin,
            }],
        };
        assert!(check_permission(&resource, Permission::Chat, &caller).is_err());
    }

    // -- Wire-bridge helper --

    #[test]
    fn test_principal_from_wire() {
        assert_eq!(
            principal_from_wire("user:123", SubjectType::User),
            Principal::User("user:123".into())
        );
        assert_eq!(
            principal_from_wire("helper", SubjectType::Agent),
            Principal::Agent("helper".into())
        );
        assert_eq!(
            principal_from_wire("eng", SubjectType::Team),
            Principal::Team("eng".into())
        );
        assert_eq!(
            principal_from_wire("ignored", SubjectType::Public),
            Principal::Public
        );
    }
}
