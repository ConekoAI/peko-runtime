//! Agent Identifier Parser
//!
//! Provides utilities for parsing agent identifiers in formats:
//! - "agent-name" -> (None, "agent-name") - uses default team
//! - "team-name/agent-name" -> (Some("team-name"), "agent-name")
//!
//! Used across CLI commands for consistent agent identification.

use thiserror::Error;

/// Errors that can occur when parsing agent identifiers
#[derive(Debug, Error, PartialEq)]
pub enum IdentifierError {
    /// Empty identifier provided
    #[error("agent identifier cannot be empty")]
    Empty,

    /// Invalid team name
    #[error("invalid team name: {0}")]
    InvalidTeamName(String),

    /// Invalid agent name
    #[error("invalid agent name: {0}")]
    InvalidAgentName(String),

    /// Too many path separators (nested teams not supported)
    #[error("nested teams are not supported: {0}")]
    NestedTeams(String),

    /// Team name is empty
    #[error("team name cannot be empty in identifier: {0}")]
    EmptyTeam(String),

    /// Agent name is empty
    #[error("agent name cannot be empty in identifier: {0}")]
    EmptyAgent(String),
}

/// Parse an agent identifier into optional team and agent name.
///
/// Supports two formats:
/// - "agent-name" -> (None, "agent-name") - will use default team
/// - "team-name/agent-name" -> (Some("team-name"), "agent-name")
///
/// # Arguments
/// * `input` - The identifier string to parse
///
/// # Returns
/// * `Ok((Option<&str>, &str))` - Tuple of (optional team, agent name)
/// * `Err(IdentifierError)` - If the identifier is invalid
///
/// # Examples
/// ```
/// use pekobot::common::identifiers::parse_agent_identifier;
///
/// assert_eq!(parse_agent_identifier("my-agent").unwrap(), (None, "my-agent"));
/// assert_eq!(parse_agent_identifier("myteam/my-agent").unwrap(), (Some("myteam"), "my-agent"));
/// ```
pub fn parse_agent_identifier(input: &str) -> Result<(Option<&str>, &str), IdentifierError> {
    let input = input.trim();

    if input.is_empty() {
        return Err(IdentifierError::Empty);
    }

    // Check for nested teams (multiple slashes)
    if input.matches('/').count() > 1 {
        return Err(IdentifierError::NestedTeams(input.to_string()));
    }

    if let Some((team, agent)) = input.split_once('/') {
        let team = team.trim();
        let agent = agent.trim();

        if team.is_empty() {
            return Err(IdentifierError::EmptyTeam(input.to_string()));
        }

        if agent.is_empty() {
            return Err(IdentifierError::EmptyAgent(input.to_string()));
        }

        // Validate team name
        if let Err(e) = validate_team_name(team) {
            return Err(IdentifierError::InvalidTeamName(e.to_string()));
        }

        // Validate agent name
        if let Err(e) = validate_agent_name(agent) {
            return Err(IdentifierError::InvalidAgentName(e.to_string()));
        }

        Ok((Some(team), agent))
    } else {
        // No team prefix, validate agent name
        if let Err(e) = validate_agent_name(input) {
            return Err(IdentifierError::InvalidAgentName(e.to_string()));
        }
        Ok((None, input))
    }
}

/// Parse agent identifier with explicit team override.
///
/// This function handles the case where both an inline team/agent format
/// and an explicit --team flag may be provided. The explicit team flag
/// takes precedence if both are present.
///
/// # Arguments
/// * `input` - The identifier string (may contain team/ prefix)
/// * `explicit_team` - Optional explicit team from --team flag
///
/// # Returns
/// * `Ok((team, agent))` - Tuple of (team, agent name), team defaults to "default"
/// * `Err(IdentifierError)` - If the identifier is invalid
pub fn parse_agent_identifier_with_override<'a>(
    input: &'a str,
    explicit_team: Option<&'a str>,
) -> Result<(&'a str, &'a str), IdentifierError> {
    let (inline_team, agent) = parse_agent_identifier(input)?;

    // Explicit team flag takes precedence
    let team = explicit_team.or(inline_team).unwrap_or("default");

    Ok((team, agent))
}

/// Validate a team name.
///
/// Rules:
/// - Must be 1-64 characters
/// - Can contain alphanumeric characters, hyphens, and underscores
/// - Cannot contain path separators (/, \)
/// - Cannot be ".." or "."
/// - Cannot start or end with hyphen
///
/// # Arguments
/// * `name` - The team name to validate
pub fn validate_team_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() {
        return Err(ValidationError::Empty);
    }

    if name.len() > 64 {
        return Err(ValidationError::TooLong(64));
    }

    // Reserved names that could cause issues
    if name == "." || name == ".." {
        return Err(ValidationError::Reserved(name.to_string()));
    }

    // Check for path separators
    if name.contains('/') || name.contains('\\') {
        return Err(ValidationError::ContainsPathSeparators);
    }

    // Check first and last characters
    if name.starts_with('-') || name.ends_with('-') {
        return Err(ValidationError::InvalidHyphenPlacement);
    }

    // Validate characters
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_' {
            return Err(ValidationError::InvalidCharacter(ch));
        }
    }

    Ok(())
}

/// Validate an agent name.
///
/// Rules:
/// - Must be 1-64 characters
/// - Can contain alphanumeric characters, hyphens, and underscores
/// - Cannot contain path separators
/// - Cannot start or end with hyphen
pub fn validate_agent_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() {
        return Err(ValidationError::Empty);
    }

    if name.len() > 64 {
        return Err(ValidationError::TooLong(64));
    }

    // Check for path separators
    if name.contains('/') || name.contains('\\') {
        return Err(ValidationError::ContainsPathSeparators);
    }

    // Check first and last characters
    if name.starts_with('-') || name.ends_with('-') {
        return Err(ValidationError::InvalidHyphenPlacement);
    }

    // Validate characters
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_' {
            return Err(ValidationError::InvalidCharacter(ch));
        }
    }

    Ok(())
}

/// Validation error types
#[derive(Debug, Error, PartialEq)]
pub enum ValidationError {
    #[error("name cannot be empty")]
    Empty,

    #[error("name exceeds maximum length of {0} characters")]
    TooLong(usize),

    #[error("'{0}' is a reserved name")]
    Reserved(String),

    #[error("name cannot contain path separators")]
    ContainsPathSeparators,

    #[error("name cannot start or end with a hyphen")]
    InvalidHyphenPlacement,

    #[error("name contains invalid character: '{0}'")]
    InvalidCharacter(char),
}

/// Check if a string looks like a team/agent identifier (contains '/')
#[must_use]
pub fn is_qualified_identifier(input: &str) -> bool {
    input.trim().contains('/') && input.trim().matches('/').count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse_agent_identifier_tests {
        use super::*;

        #[test]
        fn test_simple_agent_name() {
            assert_eq!(
                parse_agent_identifier("my-agent").unwrap(),
                (None, "my-agent")
            );
        }

        #[test]
        fn test_qualified_identifier() {
            assert_eq!(
                parse_agent_identifier("myteam/my-agent").unwrap(),
                (Some("myteam"), "my-agent")
            );
        }

        #[test]
        fn test_trims_whitespace() {
            assert_eq!(
                parse_agent_identifier("  my-agent  ").unwrap(),
                (None, "my-agent")
            );
            assert_eq!(
                parse_agent_identifier("  myteam  /  my-agent  ").unwrap(),
                (Some("myteam"), "my-agent")
            );
        }

        #[test]
        fn test_empty_identifier() {
            assert_eq!(
                parse_agent_identifier("").unwrap_err(),
                IdentifierError::Empty
            );
        }

        #[test]
        fn test_whitespace_only() {
            assert_eq!(
                parse_agent_identifier("   ").unwrap_err(),
                IdentifierError::Empty
            );
        }

        #[test]
        fn test_nested_teams() {
            assert_eq!(
                parse_agent_identifier("team/subteam/agent").unwrap_err(),
                IdentifierError::NestedTeams("team/subteam/agent".to_string())
            );
        }

        #[test]
        fn test_empty_team() {
            assert_eq!(
                parse_agent_identifier("/agent").unwrap_err(),
                IdentifierError::EmptyTeam("/agent".to_string())
            );
        }

        #[test]
        fn test_empty_agent() {
            assert_eq!(
                parse_agent_identifier("team/").unwrap_err(),
                IdentifierError::EmptyAgent("team/".to_string())
            );
        }

        #[test]
        fn test_invalid_team_name() {
            assert!(matches!(
                parse_agent_identifier("../agent").unwrap_err(),
                IdentifierError::InvalidTeamName(_)
            ));
        }

        #[test]
        fn test_invalid_agent_name() {
            assert!(matches!(
                parse_agent_identifier("my/agent@bad").unwrap_err(),
                IdentifierError::InvalidAgentName(_)
            ));
        }
    }

    mod parse_with_override_tests {
        use super::*;

        #[test]
        fn test_no_override_no_inline() {
            assert_eq!(
                parse_agent_identifier_with_override("my-agent", None).unwrap(),
                ("default", "my-agent")
            );
        }

        #[test]
        fn test_inline_team_no_override() {
            assert_eq!(
                parse_agent_identifier_with_override("myteam/my-agent", None).unwrap(),
                ("myteam", "my-agent")
            );
        }

        #[test]
        fn test_no_inline_with_override() {
            assert_eq!(
                parse_agent_identifier_with_override("my-agent", Some("otherteam")).unwrap(),
                ("otherteam", "my-agent")
            );
        }

        #[test]
        fn test_override_takes_precedence() {
            assert_eq!(
                parse_agent_identifier_with_override("myteam/my-agent", Some("otherteam")).unwrap(),
                ("otherteam", "my-agent")
            );
        }
    }

    mod validate_team_name_tests {
        use super::*;

        #[test]
        fn test_valid_names() {
            assert!(validate_team_name("myteam").is_ok());
            assert!(validate_team_name("my-team").is_ok());
            assert!(validate_team_name("my_team").is_ok());
            assert!(validate_team_name("team123").is_ok());
            assert!(validate_team_name("a").is_ok());
        }

        #[test]
        fn test_empty_name() {
            assert_eq!(validate_team_name("").unwrap_err(), ValidationError::Empty);
        }

        #[test]
        fn test_reserved_names() {
            assert_eq!(
                validate_team_name(".").unwrap_err(),
                ValidationError::Reserved(".".to_string())
            );
            assert_eq!(
                validate_team_name("..").unwrap_err(),
                ValidationError::Reserved("..".to_string())
            );
        }

        #[test]
        fn test_path_separators() {
            assert_eq!(
                validate_team_name("team/name").unwrap_err(),
                ValidationError::ContainsPathSeparators
            );
            assert_eq!(
                validate_team_name("team\\name").unwrap_err(),
                ValidationError::ContainsPathSeparators
            );
        }

        #[test]
        fn test_hyphen_placement() {
            assert_eq!(
                validate_team_name("-team").unwrap_err(),
                ValidationError::InvalidHyphenPlacement
            );
            assert_eq!(
                validate_team_name("team-").unwrap_err(),
                ValidationError::InvalidHyphenPlacement
            );
        }

        #[test]
        fn test_invalid_characters() {
            assert!(matches!(
                validate_team_name("team@name").unwrap_err(),
                ValidationError::InvalidCharacter('@')
            ));
            assert!(matches!(
                validate_team_name("team.name").unwrap_err(),
                ValidationError::InvalidCharacter('.')
            ));
        }

        #[test]
        fn test_too_long() {
            let long_name = "a".repeat(65);
            assert_eq!(
                validate_team_name(&long_name).unwrap_err(),
                ValidationError::TooLong(64)
            );
        }
    }

    mod is_qualified_identifier_tests {
        use super::*;

        #[test]
        fn test_qualified() {
            assert!(is_qualified_identifier("team/agent"));
        }

        #[test]
        fn test_unqualified() {
            assert!(!is_qualified_identifier("agent"));
        }

        #[test]
        fn test_nested() {
            // Nested teams (multiple slashes) are not qualified identifiers
            assert!(!is_qualified_identifier("a/b/c"));
        }

        #[test]
        fn test_empty() {
            assert!(!is_qualified_identifier(""));
        }
    }
}
