//! Agent name parser
//!
//! Provides utilities for parsing a plain agent name from CLI input.
//! Team-qualified identifiers (`team/agent`) are no longer supported;
//! the `team` concept was removed in issue #92.

use thiserror::Error;

/// Errors that can occur when parsing an agent name.
#[derive(Debug, Error, PartialEq)]
pub enum IdentifierError {
    /// Empty identifier provided
    #[error("agent name cannot be empty")]
    Empty,

    /// Invalid agent name
    #[error("invalid agent name: {0}")]
    InvalidAgentName(String),
}

/// Parse and validate a plain agent name.
///
/// Trims whitespace, then validates the name using [`validate_agent_name`].
/// Team-qualified identifiers such as `team/agent` are rejected because the
/// `team` namespace no longer exists.
///
/// # Returns
/// * `Ok(&str)` - The validated, trimmed agent name
/// * `Err(IdentifierError)` - If the name is empty or invalid
pub fn parse_agent_name(input: &str) -> Result<&str, IdentifierError> {
    let input = input.trim();

    if input.is_empty() {
        return Err(IdentifierError::Empty);
    }

    validate_agent_name(input).map_err(|e| IdentifierError::InvalidAgentName(e.to_string()))?;

    Ok(input)
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

    #[error("name cannot contain path separators")]
    ContainsPathSeparators,

    #[error("name cannot start or end with a hyphen")]
    InvalidHyphenPlacement,

    #[error("name contains invalid character: '{0}'")]
    InvalidCharacter(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse_agent_name_tests {
        use super::*;

        #[test]
        fn test_simple_agent_name() {
            assert_eq!(parse_agent_name("my-agent").unwrap(), "my-agent");
        }

        #[test]
        fn test_trims_whitespace() {
            assert_eq!(parse_agent_name("  my-agent  ").unwrap(), "my-agent");
        }

        #[test]
        fn test_empty_identifier() {
            assert_eq!(parse_agent_name("").unwrap_err(), IdentifierError::Empty);
        }

        #[test]
        fn test_whitespace_only() {
            assert_eq!(parse_agent_name("   ").unwrap_err(), IdentifierError::Empty);
        }

        #[test]
        fn test_qualified_identifier_rejected() {
            // `team/agent` is no longer a valid identifier format.
            assert!(matches!(
                parse_agent_name("myteam/my-agent").unwrap_err(),
                IdentifierError::InvalidAgentName(_)
            ));
        }

        #[test]
        fn test_nested_teams_rejected() {
            assert!(matches!(
                parse_agent_name("team/subteam/agent").unwrap_err(),
                IdentifierError::InvalidAgentName(_)
            ));
        }

        #[test]
        fn test_invalid_agent_name() {
            assert!(matches!(
                parse_agent_name("my/agent@bad").unwrap_err(),
                IdentifierError::InvalidAgentName(_)
            ));
        }
    }
}
