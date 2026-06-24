//! Agent-team membership types
//!
//! These types represent the explicit membership relationship between
//! agents and teams. Membership is stored bidirectionally for query
//! efficiency and robustness.
//!
//! # Storage Locations
//!
//! - Agent-side: `~/.peko/agents/{agent}/memberships.toml`
//! - Team-side: `~/.peko/teams/{team}/members.toml`

use serde::{Deserialize, Serialize};

/// Role an agent can have within a team
///
/// Roles form a hierarchy: `Member` < `Admin` < `Owner`.
/// This ordering is used for permission checks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRole {
    /// Regular team member
    Member,
    /// Team administrator — can manage members and settings
    Admin,
    /// Team owner — has full control, including destructive operations
    Owner,
}

impl Default for MembershipRole {
    fn default() -> Self {
        Self::Member
    }
}

impl MembershipRole {
    /// Check if this role has at least the permissions of `other`.
    ///
    /// # Examples
    ///
    /// ```
    /// use peko::common::types::membership::MembershipRole;
    ///
    /// assert!(MembershipRole::Admin.is_at_least(MembershipRole::Member));
    /// assert!(MembershipRole::Owner.is_at_least(MembershipRole::Admin));
    /// assert!(!MembershipRole::Member.is_at_least(MembershipRole::Admin));
    /// ```
    #[must_use]
    pub const fn is_at_least(self, other: Self) -> bool {
        self as u8 >= other as u8
    }
}

impl std::fmt::Display for MembershipRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MembershipRole::Member => write!(f, "member"),
            MembershipRole::Admin => write!(f, "admin"),
            MembershipRole::Owner => write!(f, "owner"),
        }
    }
}

/// A single membership entry from an agent's perspective
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMembership {
    /// Team name
    pub team: String,
    /// When the agent joined the team
    pub joined_at: String,
    /// Role within the team
    #[serde(default)]
    pub role: MembershipRole,
}

/// All teams an agent belongs to
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentMemberships {
    /// List of team memberships
    #[serde(default)]
    pub memberships: Vec<AgentMembership>,
}

impl AgentMemberships {
    /// Create empty memberships
    #[must_use]
    pub fn new() -> Self {
        Self {
            memberships: Vec::new(),
        }
    }

    /// Check if the agent belongs to a specific team
    #[must_use]
    pub fn belongs_to(&self, team: &str) -> bool {
        self.memberships
            .iter()
            .any(|m| m.team.eq_ignore_ascii_case(team))
    }

    /// Get membership for a specific team
    #[must_use]
    pub fn get(&self, team: &str) -> Option<&AgentMembership> {
        self.memberships
            .iter()
            .find(|m| m.team.eq_ignore_ascii_case(team))
    }

    /// Add a membership (idempotent)
    pub fn add(&mut self, membership: AgentMembership) {
        // Remove existing membership for the same team
        self.memberships
            .retain(|m| !m.team.eq_ignore_ascii_case(&membership.team));
        self.memberships.push(membership);
    }

    /// Remove membership for a specific team
    pub fn remove(&mut self, team: &str) {
        self.memberships
            .retain(|m| !m.team.eq_ignore_ascii_case(team));
    }

    /// List all team names
    #[must_use]
    pub fn teams(&self) -> Vec<&str> {
        self.memberships.iter().map(|m| m.team.as_str()).collect()
    }

    /// Number of memberships
    #[must_use]
    pub fn len(&self) -> usize {
        self.memberships.len()
    }

    /// Check if there are no memberships
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.memberships.is_empty()
    }

    /// Load from a TOML file
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content).unwrap_or_default())
    }

    /// Save to a TOML file
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

/// A single member entry from a team's perspective
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamMember {
    /// Agent name
    pub agent: String,
    /// When the agent joined the team
    pub joined_at: String,
    /// Role within the team
    #[serde(default)]
    pub role: MembershipRole,
}

/// All agents that belong to a team
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TeamMembers {
    /// List of team members
    #[serde(default)]
    pub members: Vec<TeamMember>,
}

impl TeamMembers {
    /// Create empty members list
    #[must_use]
    pub fn new() -> Self {
        Self {
            members: Vec::new(),
        }
    }

    /// Check if a specific agent is a member
    #[must_use]
    pub fn has_member(&self, agent: &str) -> bool {
        self.members
            .iter()
            .any(|m| m.agent.eq_ignore_ascii_case(agent))
    }

    /// Get member entry for a specific agent
    #[must_use]
    pub fn get(&self, agent: &str) -> Option<&TeamMember> {
        self.members
            .iter()
            .find(|m| m.agent.eq_ignore_ascii_case(agent))
    }

    /// Add a member (idempotent)
    pub fn add(&mut self, member: TeamMember) {
        // Remove existing member with the same name
        self.members
            .retain(|m| !m.agent.eq_ignore_ascii_case(&member.agent));
        self.members.push(member);
    }

    /// Remove a member by agent name
    pub fn remove(&mut self, agent: &str) {
        self.members
            .retain(|m| !m.agent.eq_ignore_ascii_case(agent));
    }

    /// List all agent names
    #[must_use]
    pub fn agents(&self) -> Vec<&str> {
        self.members.iter().map(|m| m.agent.as_str()).collect()
    }

    /// Number of members
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Check if there are no members
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Load from a TOML file
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content).unwrap_or_default())
    }

    /// Save to a TOML file
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

/// Result of adding an agent to a team
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamJoinResult {
    pub agent: String,
    pub team: String,
    pub role: MembershipRole,
}

/// Result of removing an agent from a team
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamLeaveResult {
    pub agent: String,
    pub team: String,
    pub was_member: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // AgentMemberships Tests
    // ========================================================================

    #[test]
    fn test_agent_memberships_new_is_empty() {
        let memberships = AgentMemberships::new();
        assert!(memberships.is_empty());
        assert_eq!(memberships.len(), 0);
    }

    #[test]
    fn test_agent_memberships_add_and_belongs_to() {
        let mut memberships = AgentMemberships::new();
        memberships.add(AgentMembership {
            team: "engineering".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        assert!(memberships.belongs_to("engineering"));
        assert!(!memberships.belongs_to("ops"));
        assert_eq!(memberships.len(), 1);
    }

    #[test]
    fn test_agent_memberships_add_is_idempotent() {
        let mut memberships = AgentMemberships::new();
        memberships.add(AgentMembership {
            team: "engineering".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });
        memberships.add(AgentMembership {
            team: "engineering".to_string(),
            joined_at: "2026-06-06T11:00:00Z".to_string(),
            role: MembershipRole::Admin,
        });

        assert_eq!(memberships.len(), 1);
        assert_eq!(
            memberships.get("engineering").unwrap().role,
            MembershipRole::Admin
        );
    }

    #[test]
    fn test_agent_memberships_case_insensitive() {
        let mut memberships = AgentMemberships::new();
        memberships.add(AgentMembership {
            team: "Engineering".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        assert!(memberships.belongs_to("engineering"));
        assert!(memberships.belongs_to("ENGINEERING"));
        assert!(memberships.belongs_to("Engineering"));
    }

    #[test]
    fn test_agent_memberships_remove() {
        let mut memberships = AgentMemberships::new();
        memberships.add(AgentMembership {
            team: "engineering".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });
        memberships.add(AgentMembership {
            team: "ops".to_string(),
            joined_at: "2026-06-06T11:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        memberships.remove("engineering");

        assert!(!memberships.belongs_to("engineering"));
        assert!(memberships.belongs_to("ops"));
        assert_eq!(memberships.len(), 1);
    }

    #[test]
    fn test_agent_memberships_teams_list() {
        let mut memberships = AgentMemberships::new();
        memberships.add(AgentMembership {
            team: "engineering".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });
        memberships.add(AgentMembership {
            team: "ops".to_string(),
            joined_at: "2026-06-06T11:00:00Z".to_string(),
            role: MembershipRole::Admin,
        });

        let teams = memberships.teams();
        assert_eq!(teams.len(), 2);
        assert!(teams.contains(&"engineering"));
        assert!(teams.contains(&"ops"));
    }

    #[test]
    fn test_agent_memberships_load_save_roundtrip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("memberships.toml");

        let mut memberships = AgentMemberships::new();
        memberships.add(AgentMembership {
            team: "engineering".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Admin,
        });

        memberships.save(&path).unwrap();
        let loaded = AgentMemberships::load(&path).unwrap();

        assert_eq!(memberships, loaded);
    }

    #[test]
    fn test_agent_memberships_load_missing_file_returns_empty() {
        let path = std::path::PathBuf::from("/nonexistent/path/memberships.toml");
        let memberships = AgentMemberships::load(&path).unwrap();
        assert!(memberships.is_empty());
    }

    #[test]
    fn test_agent_memberships_serialization_format() {
        let mut memberships = AgentMemberships::new();
        memberships.add(AgentMembership {
            team: "engineering".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        let toml = toml::to_string_pretty(&memberships).unwrap();
        assert!(toml.contains("[[memberships]]"));
        assert!(toml.contains("engineering"));
        assert!(toml.contains("joined_at"));
        assert!(toml.contains("role"));
    }

    // ========================================================================
    // TeamMembers Tests
    // ========================================================================

    #[test]
    fn test_team_members_new_is_empty() {
        let members = TeamMembers::new();
        assert!(members.is_empty());
        assert_eq!(members.len(), 0);
    }

    #[test]
    fn test_team_members_add_and_has_member() {
        let mut members = TeamMembers::new();
        members.add(TeamMember {
            agent: "alice".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        assert!(members.has_member("alice"));
        assert!(!members.has_member("bob"));
        assert_eq!(members.len(), 1);
    }

    #[test]
    fn test_team_members_add_is_idempotent() {
        let mut members = TeamMembers::new();
        members.add(TeamMember {
            agent: "alice".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });
        members.add(TeamMember {
            agent: "alice".to_string(),
            joined_at: "2026-06-06T11:00:00Z".to_string(),
            role: MembershipRole::Admin,
        });

        assert_eq!(members.len(), 1);
        assert_eq!(members.get("alice").unwrap().role, MembershipRole::Admin);
    }

    #[test]
    fn test_team_members_case_insensitive() {
        let mut members = TeamMembers::new();
        members.add(TeamMember {
            agent: "Alice".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        assert!(members.has_member("alice"));
        assert!(members.has_member("ALICE"));
        assert!(members.has_member("Alice"));
    }

    #[test]
    fn test_team_members_remove() {
        let mut members = TeamMembers::new();
        members.add(TeamMember {
            agent: "alice".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });
        members.add(TeamMember {
            agent: "bob".to_string(),
            joined_at: "2026-06-06T11:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        members.remove("alice");

        assert!(!members.has_member("alice"));
        assert!(members.has_member("bob"));
        assert_eq!(members.len(), 1);
    }

    #[test]
    fn test_team_members_agents_list() {
        let mut members = TeamMembers::new();
        members.add(TeamMember {
            agent: "alice".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Admin,
        });
        members.add(TeamMember {
            agent: "bob".to_string(),
            joined_at: "2026-06-06T11:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        let agents = members.agents();
        assert_eq!(agents.len(), 2);
        assert!(agents.contains(&"alice"));
        assert!(agents.contains(&"bob"));
    }

    #[test]
    fn test_team_members_load_save_roundtrip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("members.toml");

        let mut members = TeamMembers::new();
        members.add(TeamMember {
            agent: "alice".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Admin,
        });

        members.save(&path).unwrap();
        let loaded = TeamMembers::load(&path).unwrap();

        assert_eq!(members, loaded);
    }

    #[test]
    fn test_team_members_load_missing_file_returns_empty() {
        let path = std::path::PathBuf::from("/nonexistent/path/members.toml");
        let members = TeamMembers::load(&path).unwrap();
        assert!(members.is_empty());
    }

    #[test]
    fn test_team_members_serialization_format() {
        let mut members = TeamMembers::new();
        members.add(TeamMember {
            agent: "alice".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Member,
        });

        let toml = toml::to_string_pretty(&members).unwrap();
        assert!(toml.contains("[[members]]"));
        assert!(toml.contains("alice"));
        assert!(toml.contains("joined_at"));
        assert!(toml.contains("role"));
    }

    // ========================================================================
    // MembershipRole Tests
    // ========================================================================

    #[test]
    fn test_membership_role_display() {
        assert_eq!(MembershipRole::Member.to_string(), "member");
        assert_eq!(MembershipRole::Admin.to_string(), "admin");
        assert_eq!(MembershipRole::Owner.to_string(), "owner");
    }

    #[test]
    fn test_membership_role_default() {
        assert_eq!(MembershipRole::default(), MembershipRole::Member);
    }

    #[test]
    fn test_membership_role_deserialization() {
        let role: MembershipRole = serde_json::from_str("\"member\"").unwrap();
        assert_eq!(role, MembershipRole::Member);

        let role: MembershipRole = serde_json::from_str("\"admin\"").unwrap();
        assert_eq!(role, MembershipRole::Admin);

        let role: MembershipRole = serde_json::from_str("\"owner\"").unwrap();
        assert_eq!(role, MembershipRole::Owner);
    }

    #[test]
    fn test_membership_role_ordering() {
        assert!(MembershipRole::Member < MembershipRole::Admin);
        assert!(MembershipRole::Admin < MembershipRole::Owner);
        assert!(MembershipRole::Member < MembershipRole::Owner);
    }

    #[test]
    fn test_membership_role_is_at_least() {
        assert!(MembershipRole::Member.is_at_least(MembershipRole::Member));
        assert!(!MembershipRole::Member.is_at_least(MembershipRole::Admin));
        assert!(!MembershipRole::Member.is_at_least(MembershipRole::Owner));

        assert!(MembershipRole::Admin.is_at_least(MembershipRole::Member));
        assert!(MembershipRole::Admin.is_at_least(MembershipRole::Admin));
        assert!(!MembershipRole::Admin.is_at_least(MembershipRole::Owner));

        assert!(MembershipRole::Owner.is_at_least(MembershipRole::Member));
        assert!(MembershipRole::Owner.is_at_least(MembershipRole::Admin));
        assert!(MembershipRole::Owner.is_at_least(MembershipRole::Owner));
    }

    // ========================================================================
    // Cross-consistency Tests
    // ========================================================================

    #[test]
    fn test_membership_roundtrip_consistency() {
        // Simulate: agent joins team, both files are updated
        let agent_membership = AgentMembership {
            team: "engineering".to_string(),
            joined_at: "2026-06-06T10:00:00Z".to_string(),
            role: MembershipRole::Admin,
        };

        let team_member = TeamMember {
            agent: "alice".to_string(),
            joined_at: agent_membership.joined_at.clone(),
            role: agent_membership.role,
        };

        // Both should reference each other
        assert_eq!(agent_membership.team, "engineering");
        assert_eq!(team_member.agent, "alice");
        assert_eq!(agent_membership.role, team_member.role);
        assert_eq!(agent_membership.joined_at, team_member.joined_at);
    }
}
