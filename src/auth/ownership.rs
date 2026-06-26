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

/// Resource being accessed — either an agent or a team
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Agent resource with ownership and permission data
    Agent {
        name: String,
        owner: Subject,
        permissions: Vec<PermissionGrant>,
    },
    /// Team resource with ownership, permission data, and member roles
    Team {
        name: String,
        owner: Subject,
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
    pub fn owner(&self) -> &Subject {
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
/// 1. Owner always passes (Subject equality).
/// 2. Look up explicit grants for this subject, plus a `Public` grant
///    applies to any caller.
/// 3. For team-scoped actions, check team member role.
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

    // 3. For team-scoped actions, check team member role.
    //    Team members are `Subject::Principal(member.agent)`.
    if let Resource::Team { members, .. } = resource {
        let caller_agent_name = match caller {
            Subject::Principal(name) => Some(name.as_str()),
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
pub fn agent_resource(name: &str, config: &crate::agents::agent_config::AgentConfig) -> Resource {
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
        let owner = Subject::User("user:123".into());
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
        let owner = Subject::User("user:123".into());
        let grantee = Subject::User("user:456".into());
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
        let owner = Subject::User("user:123".into());
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner,
            permissions: vec![PermissionGrant {
                subject: Subject::Public,
                permission: Permission::Chat,
                granted_at: "2026-06-07T11:00:00Z".to_string(),
                granted_by: Subject::User("user:123".into()),
            }],
        };
        let anyone = Subject::User("anyone".into());
        assert!(check_permission(&resource, Permission::Chat, &anyone).is_ok());
        assert!(check_permission(&resource, Permission::Delete, &anyone).is_err());
    }

    #[test]
    fn test_non_owner_without_grant_denied() {
        let owner = Subject::User("user:123".into());
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner,
            permissions: vec![],
        };
        let stranger = Subject::User("user:999".into());
        assert!(check_permission(&resource, Permission::Chat, &stranger).is_err());
    }

    // -- ADR-039 acceptance-criteria tests --

    #[test]
    fn test_agent_caller_denied_for_user_owned_resource() {
        // Owner is a User; caller is an Agent; action is Delete.
        // Agent is not the owner (cross-kind), and there's no grant,
        // so this MUST be denied.
        let owner = Subject::User("user:123".into());
        let caller = Subject::Principal("helper".into());
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
        let owner = Subject::Principal("helper".into());
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
        let owner = Subject::Principal("helper".into());
        let caller = Subject::Principal("other".into());
        let resource = Resource::Agent {
            name: "helper".to_string(),
            owner,
            permissions: vec![],
        };
        assert!(check_permission(&resource, Permission::Delete, &caller).is_err());
    }

    // -- Issue #24 acceptance-criterion tests --

    /// Issue #24 acceptance criterion #2: a `PermissionGrant` with
    /// `subject = Subject::Principal("helper")` on the target agent is
    /// honored when a2a_send originates the call (caller is
    /// `Subject::Principal("helper")`).
    ///
    /// Pre-fix, `a2a_send` set the caller's peer to
    /// `Subject::User("helper")`. The cross-kind guard from ADR-039
    /// (in `check_permission`, line ~212: `&grant.subject == caller`)
    /// would compare `Subject::User("helper") != Subject::Principal("helper")`
    /// and deny — the grant would never apply to a2a-originated
    /// calls. Post-fix, `a2a_send` constructs `Subject::Principal("helper")`,
    /// so the grant matches.
    #[test]
    fn test_agent_grant_honored_for_a2a_originated_call_issue_24() {
        let owner = Subject::User("alice".into());
        let caller = Subject::Principal("helper".into());
        // alice granted helper (the a2a caller) Chat permission on her agent.
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner,
            permissions: vec![PermissionGrant {
                subject: Subject::Principal("helper".into()),
                permission: Permission::Chat,
                granted_at: "2026-06-19T00:00:00Z".to_string(),
                granted_by: Subject::User("alice".into()),
            }],
        };

        // Agent caller matches the agent grant → allowed.
        assert!(
            check_permission(&resource, Permission::Chat, &caller).is_ok(),
            "Agent caller must be allowed when an Agent grant exists for that agent"
        );
        // Same caller without the Chat grant would be denied
        // (Chat ≠ Delete on the same grant).
        assert!(
            check_permission(&resource, Permission::Delete, &caller).is_err(),
            "Chat grant must not cover Delete"
        );

        // A different agent caller is still denied (no grant for it).
        let other_agent = Subject::Principal("other".into());
        assert!(
            check_permission(&resource, Permission::Chat, &other_agent).is_err(),
            "grant is scoped to a specific agent subject; other agents must not match"
        );

        // The cross-kind guard from ADR-039 must still bite: a User
        // caller with the same id ("helper") must NOT match the
        // Agent grant — that's the masquerade pre-#24 silently
        // enabled, and the post-#24 fix correctly rejects.
        let masquerade_user = Subject::User("helper".into());
        assert!(
            check_permission(&resource, Permission::Chat, &masquerade_user).is_err(),
            "User masquerade must NOT match an Agent grant (cross-kind guard)"
        );
    }

    /// Companion to the above: an a2a-originated call from `helper`
    /// is denied when there is no Agent grant for `helper` on the
    /// target, even if a User grant for `"helper"` exists. This is
    /// the deny-path that pre-#24 silently let through (the
    /// masquerade made the User grant match).
    #[test]
    fn test_a2a_originated_call_does_not_match_user_grant_issue_24() {
        let owner = Subject::User("alice".into());
        // alice granted user:"helper" Chat — but `helper` is the a2a
        // caller, not a real user. Pre-fix, this grant would have
        // matched. Post-fix, the cross-kind guard denies.
        let resource = Resource::Agent {
            name: "alice".to_string(),
            owner,
            permissions: vec![PermissionGrant {
                subject: Subject::User("helper".into()),
                permission: Permission::Chat,
                granted_at: "2026-06-19T00:00:00Z".to_string(),
                granted_by: Subject::User("alice".into()),
            }],
        };

        let caller = Subject::Principal("helper".into());
        assert!(
            check_permission(&resource, Permission::Chat, &caller).is_err(),
            "Agent caller must not be allowed by a User grant (cross-kind guard)"
        );
    }

    // -- Team role tests (rewritten to use Subject) --

    #[test]
    fn test_team_role_member_allows_chat_and_view() {
        let owner = Subject::User("user:123".into());
        let caller = Subject::Principal("alice".into());
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
        let owner = Subject::User("user:123".into());
        let caller = Subject::Principal("alice".into());
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
        let owner = Subject::User("user:123".into());
        let caller = Subject::Principal("bob".into());
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
        let owner = Subject::User("user:123".into());
        let caller = Subject::User("alice".into());
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

    // `principal_from_wire` was removed in issue #30; the IPC resolver
    // no longer needs a wire-side bridge because every grant/revoke
    // packet carries a `Subject` directly.
}
