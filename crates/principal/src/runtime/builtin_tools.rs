//! Catalog of built-in tool names registered by the framework.
//!
//! Lives in `peko-principal` because [`super::ExtensionCatalog::build`] (the
//! per-principal extension catalog builder) reads these lists to compute
//! `enabled` flags for the catalog items. The host crate does not own these
//! names — they are the canonical contract between the framework's
//! `BuiltinToolAdapter::register_all()` and the principal layer's view of
//! which built-ins are available.

/// Tools registered once at daemon startup by `BuiltinToolAdapter::register_all()`.
pub const GLOBAL_TOOL_NAMES: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Glob",
    "Grep",
    "Edit",
    "session",
    "CronCreate",
    "CronDelete",
    "CronList",
    "AsyncStatus",
    "AsyncList",
    "AsyncStop",
    "Skill",
];

/// Tools registered per-agent in `Agent::init_builtins_async()`.
pub const AGENT_SPECIFIC_TOOL_NAMES: &[&str] = &[
    "Agent",
    "principal_send",
    "AsyncSpawn",
    "AsyncOutput",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskUpdate",
];

/// Concatenation of [`GLOBAL_TOOL_NAMES`] and [`AGENT_SPECIFIC_TOOL_NAMES`].
#[must_use]
pub fn all_tool_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = GLOBAL_TOOL_NAMES.to_vec();
    names.extend_from_slice(AGENT_SPECIFIC_TOOL_NAMES);
    names
}

/// True iff `name` (case-insensitive) is in [`all_tool_names`].
#[must_use]
pub fn is_builtin_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    all_tool_names().iter().any(|&n| n.to_lowercase() == lower)
}

/// True iff `name` (case-insensitive) is in [`AGENT_SPECIFIC_TOOL_NAMES`].
#[must_use]
pub fn is_agent_specific_builtin_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    AGENT_SPECIFIC_TOOL_NAMES
        .iter()
        .any(|&n| n.to_lowercase() == lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tool_names_includes_both_lists() {
        let names = all_tool_names();
        assert!(names.contains(&"Bash"));
        assert!(names.contains(&"Agent"));
    }

    #[test]
    fn is_builtin_tool_is_case_insensitive() {
        assert!(is_builtin_tool("Bash"));
        assert!(is_builtin_tool("bash"));
        assert!(!is_builtin_tool("nope"));
    }
}
