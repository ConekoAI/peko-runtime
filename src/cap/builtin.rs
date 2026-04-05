//! Built-in Capability Registry
//!
//! Provides metadata for all built-in capabilities that are compiled into the runtime.
//! This registry is the source of truth for built-in capability discovery.
//!
//! Built-in capabilities include:
//! - Filesystem tools (read_file, write_file, glob, grep, str_replace_file)
//! - Shell tool
//! - Session tools (sessions_list, sessions_history, session_status)
//! - Cron tool
//! - Legacy tools (filesystem, apply_patch)

use crate::cap::CapabilityInfo;
use std::collections::HashMap;

/// Metadata for a single built-in capability
pub struct BuiltInCapability {
    /// Capability name (unique identifier)
    pub name: &'static str,
    /// Human-readable description
    pub description: &'static str,
    /// Category for grouping
    pub category: &'static str,
    /// Since which version
    pub since: &'static str,
}

/// Registry of all built-in capabilities
pub struct BuiltInCapabilityRegistry;

impl BuiltInCapabilityRegistry {
    /// All built-in capabilities
    fn capabilities() -> Vec<BuiltInCapability> {
        vec![
            // Legacy tools (deprecated)
            BuiltInCapability {
                name: "filesystem",
                description: "Legacy monolithic filesystem tool (deprecated, use granular tools)",
                category: "filesystem",
                since: "0.1.0",
            },
            BuiltInCapability {
                name: "apply_patch",
                description: "Apply patch to file content (deprecated)",
                category: "filesystem",
                since: "0.1.0",
            },
            // Granular filesystem tools
            BuiltInCapability {
                name: "read_file",
                description: "Read contents of a file",
                category: "filesystem",
                since: "0.9.0",
            },
            BuiltInCapability {
                name: "write_file",
                description: "Write content to a file (creates or overwrites)",
                category: "filesystem",
                since: "0.9.0",
            },
            BuiltInCapability {
                name: "glob",
                description: "Find files matching a glob pattern",
                category: "filesystem",
                since: "0.9.0",
            },
            BuiltInCapability {
                name: "grep",
                description: "Search for text patterns in files",
                category: "filesystem",
                since: "0.9.0",
            },
            BuiltInCapability {
                name: "str_replace_file",
                description: "Make targeted string replacements in a file",
                category: "filesystem",
                since: "0.9.0",
            },
            // Shell
            BuiltInCapability {
                name: "shell",
                description: "Execute shell commands",
                category: "system",
                since: "0.1.0",
            },
            BuiltInCapability {
                name: "process",
                description: "Legacy alias for shell (deprecated, use shell)",
                category: "system",
                since: "0.1.0",
            },
            // Session tools
            BuiltInCapability {
                name: "sessions_list",
                description: "List all sessions for the agent",
                category: "session",
                since: "0.5.0",
            },
            BuiltInCapability {
                name: "sessions_history",
                description: "Get message history for a session",
                category: "session",
                since: "0.5.0",
            },
            BuiltInCapability {
                name: "session_status",
                description: "Get status of a specific session",
                category: "session",
                since: "0.5.0",
            },
            BuiltInCapability {
                name: "sessions_send",
                description: "Send a message to a specific session",
                category: "session",
                since: "0.5.0",
            },
            // Agent management
            BuiltInCapability {
                name: "agent_spawn",
                description: "Spawn a new agent instance",
                category: "agent",
                since: "0.3.0",
            },
            BuiltInCapability {
                name: "agent_spawn_status",
                description: "Check status of a spawned agent",
                category: "agent",
                since: "0.3.0",
            },
            BuiltInCapability {
                name: "agent_spawn_list",
                description: "List all spawned agents",
                category: "agent",
                since: "0.3.0",
            },
            BuiltInCapability {
                name: "agents_list",
                description: "List all agents in the system",
                category: "agent",
                since: "0.3.0",
            },
            BuiltInCapability {
                name: "agent_info",
                description: "Get information about an agent",
                category: "agent",
                since: "0.3.0",
            },
            // Cron/scheduling
            BuiltInCapability {
                name: "cron",
                description: "Schedule and manage cron jobs",
                category: "scheduling",
                since: "0.4.0",
            },
        ]
    }

    /// List all built-in capabilities with metadata
    pub fn list_all() -> Vec<CapabilityInfo> {
        Self::capabilities()
            .into_iter()
            .map(|c| {
                CapabilityInfo::builtin(c.name, c.description)
            })
            .collect()
    }

    /// Get capability info by name
    pub fn get(name: &str) -> Option<CapabilityInfo> {
        Self::capabilities()
            .into_iter()
            .find(|c| c.name == name)
            .map(|c| CapabilityInfo::builtin(c.name, c.description))
    }

    /// Check if a name is a built-in capability
    pub fn is_builtin(name: &str) -> bool {
        Self::capabilities()
            .iter()
            .any(|c| c.name == name)
    }

    /// Get all capability names
    pub fn names() -> Vec<&'static str> {
        Self::capabilities()
            .iter()
            .map(|c| c.name)
            .collect()
    }

    /// Get capabilities by category
    pub fn by_category() -> HashMap<&'static str, Vec<&'static str>> {
        let mut map: HashMap<&'static str, Vec<&'static str>> = HashMap::new();
        for cap in Self::capabilities() {
            map.entry(cap.category)
                .or_default()
                .push(cap.name);
        }
        map
    }

    /// Default enabled capabilities (when no whitelist is set)
    pub fn default_enabled() -> Vec<&'static str> {
        vec!["shell", "session_status"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_all() {
        let caps = BuiltInCapabilityRegistry::list_all();
        assert!(!caps.is_empty());
        assert!(caps.iter().all(|c| c.cap_type == crate::cap::CapabilityType::BuiltIn));
    }

    #[test]
    fn test_is_builtin() {
        assert!(BuiltInCapabilityRegistry::is_builtin("shell"));
        assert!(BuiltInCapabilityRegistry::is_builtin("read_file"));
        assert!(!BuiltInCapabilityRegistry::is_builtin("mcp_browser"));
    }

    #[test]
    fn test_default_enabled() {
        let defaults = BuiltInCapabilityRegistry::default_enabled();
        assert!(defaults.contains(&"shell"));
        assert!(defaults.contains(&"session_status"));
    }
}
