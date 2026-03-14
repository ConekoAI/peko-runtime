//! Subagent Session Key Utilities
//!
//! Provides standardized key formats for subagent sessions,
//! following `OpenClaw`'s pattern: `agent:{agent}:subagent:{uuid}`

use uuid::Uuid;

/// Generate a new subagent session key
///
/// Format: `agent:{agent_name}:subagent:{uuid}`
///
/// # Examples
/// ```
/// let key = generate_subagent_key("myagent");
/// // agent:myagent:subagent:550e8400-e29b-41d4-a716-446655440000
/// ```
#[must_use]
pub fn generate_subagent_key(agent_name: &str) -> String {
    format!("agent:{}:subagent:{}", agent_name, Uuid::new_v4())
}

/// Parse a subagent session key to extract components
///
/// Returns (`agent_name`, `subagent_uuid`) if valid, None otherwise
#[must_use]
pub fn parse_subagent_key(key: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = key.split(':').collect();

    // Expected format: agent:{agent_name}:subagent:{uuid}
    if parts.len() == 4
        && parts[0] == "agent"
        && parts[2] == "subagent"
        && !parts[1].is_empty()
        && !parts[3].is_empty()
    {
        Some((parts[1].to_string(), parts[3].to_string()))
    } else {
        None
    }
}

/// Check if a session key is a subagent key
#[must_use]
pub fn is_subagent_key(key: &str) -> bool {
    key.contains(":subagent:")
}

/// Extract the agent name from a subagent key
#[must_use]
pub fn extract_agent_name(key: &str) -> Option<String> {
    parse_subagent_key(key).map(|(agent, _)| agent)
}

/// Extract the subagent UUID from a key
#[must_use]
pub fn extract_subagent_uuid(key: &str) -> Option<String> {
    parse_subagent_key(key).map(|(_, uuid)| uuid)
}

/// Get the parent session key from a subagent key
///
/// For OpenClaw-style subagent keys, we don't have the parent encoded.
/// This function returns None to indicate the parent must be tracked separately.
#[must_use]
pub fn get_parent_key(_subagent_key: &str) -> Option<String> {
    // OpenClaw-style keys don't encode parent information
    // The parent must be tracked in the SubagentRegistry
    None
}

/// Convert a peer-based session key to a display format
#[must_use]
pub fn format_display_key(key: &str) -> String {
    if is_subagent_key(key) {
        if let Some((agent, uuid)) = parse_subagent_key(key) {
            format!("{agent}:subagent:{uuid:.8}...")
        } else {
            key.to_string()
        }
    } else {
        key.to_string()
    }
}

/// Build a subagent key with parent reference (hybrid format)
///
/// This is an alternative format that includes parent information:
/// `agent:{agent}:peer:{type}:{id}:subagent:{uuid}`
#[must_use]
pub fn generate_subagent_key_with_parent(_agent_name: &str, parent_session_key: &str) -> String {
    format!("{}:subagent:{}", parent_session_key, Uuid::new_v4())
}

/// Parse a hybrid subagent key with parent info
///
/// Returns (`agent_name`, `parent_key`, `subagent_uuid`) if valid
#[must_use]
pub fn parse_hybrid_subagent_key(key: &str) -> Option<(String, String, String)> {
    // Check if it contains :subagent:
    if let Some(pos) = key.find(":subagent:") {
        let parent_key = &key[..pos];
        let subagent_part = &key[pos + 10..]; // After ":subagent:"

        // Extract agent name from parent key
        if let Some(agent) = extract_agent_from_key(parent_key) {
            return Some((agent, parent_key.to_string(), subagent_part.to_string()));
        }
    }
    None
}

/// Extract agent name from a peer-based key
fn extract_agent_from_key(key: &str) -> Option<String> {
    let parts: Vec<&str> = key.split(':').collect();
    if parts.len() >= 2 && parts[0] == "agent" {
        Some(parts[1].to_string())
    } else {
        None
    }
}

/// Get depth from a subagent key
///
/// For hybrid keys that encode nesting, count the number of :subagent: segments
#[must_use]
pub fn get_key_depth(key: &str) -> u32 {
    key.matches(":subagent:").count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_subagent_key() {
        let key = generate_subagent_key("myagent");
        assert!(key.starts_with("agent:myagent:subagent:"));
        assert_eq!(key.split(':').count(), 4);
    }

    #[test]
    fn test_parse_subagent_key() {
        let key = "agent:myagent:subagent:550e8400-e29b-41d4-a716-446655440000";
        let parsed = parse_subagent_key(key);
        assert!(parsed.is_some());

        let (agent, uuid) = parsed.unwrap();
        assert_eq!(agent, "myagent");
        assert_eq!(uuid, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_parse_invalid_key() {
        // Not a subagent key
        assert!(parse_subagent_key("agent:myagent:peer:user:alice").is_none());

        // Wrong format
        assert!(parse_subagent_key("some:random:key").is_none());

        // Empty parts
        assert!(parse_subagent_key("agent::subagent:").is_none());
    }

    #[test]
    fn test_is_subagent_key() {
        assert!(is_subagent_key("agent:myagent:subagent:uuid"));
        assert!(!is_subagent_key("agent:myagent:peer:user:alice"));
        assert!(!is_subagent_key("some:other:key"));
    }

    #[test]
    fn test_extract_agent_name() {
        assert_eq!(
            extract_agent_name("agent:myagent:subagent:uuid"),
            Some("myagent".to_string())
        );
        assert!(extract_agent_name("not:subagent:key").is_none());
    }

    #[test]
    fn test_generate_with_parent() {
        let parent = "agent:myagent:peer:user:alice";
        let key = generate_subagent_key_with_parent("myagent", parent);

        assert!(key.starts_with(parent));
        assert!(key.contains(":subagent:"));
    }

    #[test]
    fn test_parse_hybrid_key() {
        let key = "agent:myagent:peer:user:alice:subagent:uuid-here";
        let parsed = parse_hybrid_subagent_key(key);

        assert!(parsed.is_some());
        let (agent, parent, uuid) = parsed.unwrap();
        assert_eq!(agent, "myagent");
        assert_eq!(parent, "agent:myagent:peer:user:alice");
        assert_eq!(uuid, "uuid-here");
    }

    #[test]
    fn test_get_key_depth() {
        assert_eq!(get_key_depth("agent:myagent:subagent:uuid"), 1);
        assert_eq!(
            get_key_depth("agent:myagent:peer:user:alice:subagent:uuid"),
            1
        );
        assert_eq!(
            get_key_depth("agent:myagent:subagent:uuid1:subagent:uuid2"),
            2
        );
        assert_eq!(get_key_depth("agent:myagent:peer:user:alice"), 0);
    }

    #[test]
    fn test_format_display_key() {
        let key = "agent:myagent:subagent:550e8400-e29b-41d4-a716-446655440000";
        let display = format_display_key(key);
        assert!(display.contains("myagent"));
        assert!(display.contains("subagent"));
        assert!(display.contains("...")); // Truncated
    }
}
