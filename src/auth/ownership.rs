//! Ownership and Permission Model (ADR-033)
//!
//! Provides per-resource RBAC-lite permission checks that run on the same
//! code path for local IPC and remote access.

use serde::{Deserialize, Serialize};

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

/// Type of subject in a permission grant
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectType {
    /// A pekohub user or DID
    User,
    /// A team (all members get the permission)
    Team,
    /// Unauthenticated public access
    Public,
}

/// A single permission grant on a resource
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionGrant {
    /// Subject ID: user_id, did, team name, or "public"
    pub subject_id: String,
    /// Type of subject
    pub subject_type: SubjectType,
    /// Granted permission
    pub permission: Permission,
    /// ISO 8601 timestamp
    pub granted_at: String,
    /// user_id of the granter (for audit)
    pub granted_by: String,
}

/// Resource being accessed — either an agent or a team
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Agent resource with ownership and permission data
    Agent {
        name: String,
        owner_id: String,
        permissions: Vec<PermissionGrant>,
    },
    /// Team resource with ownership, permission data, and member roles
    Team {
        name: String,
        owner_id: String,
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

    /// Get the owner_id of this resource
    #[must_use]
    pub fn owner_id(&self) -> &str {
        match self {
            Self::Agent { owner_id, .. } => owner_id,
            Self::Team { owner_id, .. } => owner_id,
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
/// 1. Owner always passes.
/// 2. Look up explicit grants for this subject (including "public").
/// 3. For team-scoped actions, check team member role.
///
/// # Errors
/// Returns `PermissionDenied` if the caller is not authorized.
pub fn check_permission(
    resource: &Resource,
    action: Permission,
    caller_subject_id: &str,
) -> Result<(), PermissionDenied> {
    // 1. Owner always passes
    if resource.owner_id() == caller_subject_id {
        return Ok(());
    }

    // 2. Look up explicit grants for this subject or "public"
    let grants = resource
        .permissions()
        .iter()
        .filter(|g| g.subject_id == caller_subject_id || g.subject_id == "public");

    for grant in grants {
        if grant.permission.covers(&action) {
            return Ok(());
        }
    }

    // 3. For team-scoped actions, check team role
    if let Resource::Team { members, .. } = resource {
        if let Some(member) = members.iter().find(|m| m.agent == caller_subject_id) {
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

    Err(PermissionDenied {
        resource: resource.id(),
        action,
        caller: caller_subject_id.to_string(),
    })
}

/// Build a `Resource::Agent` from an `AgentConfig` and name.
#[must_use]
pub fn agent_resource(name: &str, config: &crate::types::agent::AgentConfig) -> Resource {
    Resource::Agent {
        name: name.to_string(),
        owner_id: config.owner_id.clone(),
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
        owner_id: metadata.owner_id.clone(),
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
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner_id: "user:123".to_string(),
            permissions: vec![],
        };
        assert!(check_permission(&resource, Permission::Delete, "user:123").is_ok());
        assert!(check_permission(&resource, Permission::Chat, "user:123").is_ok());
    }

    #[test]
    fn test_explicit_grant_allows() {
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner_id: "user:123".to_string(),
            permissions: vec![PermissionGrant {
                subject_id: "user:456".to_string(),
                subject_type: SubjectType::User,
                permission: Permission::Chat,
                granted_at: "2026-06-07T10:00:00Z".to_string(),
                granted_by: "user:123".to_string(),
            }],
        };
        assert!(check_permission(&resource, Permission::Chat, "user:456").is_ok());
        assert!(check_permission(&resource, Permission::Delete, "user:456").is_err());
    }

    #[test]
    fn test_public_grant_allows_unauthenticated() {
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner_id: "user:123".to_string(),
            permissions: vec![PermissionGrant {
                subject_id: "public".to_string(),
                subject_type: SubjectType::Public,
                permission: Permission::Chat,
                granted_at: "2026-06-07T11:00:00Z".to_string(),
                granted_by: "user:123".to_string(),
            }],
        };
        assert!(check_permission(&resource, Permission::Chat, "anyone").is_ok());
        assert!(check_permission(&resource, Permission::Delete, "anyone").is_err());
    }

    #[test]
    fn test_non_owner_without_grant_denied() {
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner_id: "user:123".to_string(),
            permissions: vec![],
        };
        assert!(check_permission(&resource, Permission::Chat, "user:999").is_err());
    }

    #[test]
    fn test_team_role_member_allows_chat_and_view() {
        let resource = Resource::Team {
            name: "engineering".to_string(),
            owner_id: "user:123".to_string(),
            permissions: vec![],
            members: vec![TeamMember {
                agent: "alice".to_string(),
                joined_at: "2026-06-06T10:00:00Z".to_string(),
                role: MembershipRole::Member,
            }],
        };
        assert!(check_permission(&resource, Permission::Chat, "alice").is_ok());
        assert!(check_permission(&resource, Permission::ViewSettings, "alice").is_ok());
        assert!(check_permission(&resource, Permission::ManageMembers, "alice").is_err());
    }

    #[test]
    fn test_team_role_admin_allows_more() {
        let resource = Resource::Team {
            name: "engineering".to_string(),
            owner_id: "user:123".to_string(),
            permissions: vec![],
            members: vec![TeamMember {
                agent: "alice".to_string(),
                joined_at: "2026-06-06T10:00:00Z".to_string(),
                role: MembershipRole::Admin,
            }],
        };
        assert!(check_permission(&resource, Permission::Chat, "alice").is_ok());
        assert!(check_permission(&resource, Permission::ManageMembers, "alice").is_ok());
        assert!(check_permission(&resource, Permission::Delete, "alice").is_err());
    }

    #[test]
    fn test_team_non_member_denied() {
        let resource = Resource::Team {
            name: "engineering".to_string(),
            owner_id: "user:123".to_string(),
            permissions: vec![],
            members: vec![],
        };
        assert!(check_permission(&resource, Permission::Chat, "bob").is_err());
    }
}
