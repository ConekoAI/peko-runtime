//! Catalog of built-in tool names registered by the framework.
//!
//! Shared by the principal catalog and built-in extension adapter.
//! These are wire names; Rust module and configuration names remain
//! snake_case. Registration depends on the required runtime bindings.

/// Installation scope and inventory are defined with the factories.
pub use crate::tools::installation::{
    BuiltinScope, AGENT_SPECIFIC_TOOL_NAMES, BUILTIN_INSTALLATIONS, GLOBAL_TOOL_NAMES,
};

/// Legacy wire spellings, accepted on lookup but never advertised.
#[must_use]
pub(crate) fn legacy_builtin_name(name: &str) -> Option<&'static str> {
    match name {
        "session" => Some("Session"),
        "model_list" => Some("ModelList"),
        "role_catalog" => Some("RoleCatalog"),
        _ => None,
    }
}

/// Every built-in wire name, derived from the installation manifest.
#[must_use]
pub fn all_tool_names() -> Vec<&'static str> {
    BUILTIN_INSTALLATIONS
        .iter()
        .map(|entry| entry.name)
        .collect()
}

/// True iff `name` (case-insensitive) is in [`all_tool_names`].
#[must_use]
pub fn is_builtin_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    let name = legacy_builtin_name(&lower).unwrap_or(&lower);
    all_tool_names()
        .iter()
        .any(|&n| n.eq_ignore_ascii_case(name))
}

/// True iff the tool has a run-owned binding in the installation manifest.
#[must_use]
pub fn is_agent_specific_builtin_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    let name = legacy_builtin_name(&lower).unwrap_or(&lower);
    BUILTIN_INSTALLATIONS
        .iter()
        .any(|entry| entry.run_binding && entry.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tool_names_includes_both_lists() {
        let names = all_tool_names();
        assert!(names.contains(&"Bash"));
        assert!(names.contains(&"Agent"));
        assert_eq!(names.len(), 37);
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
        for name in names {
            assert!(name.starts_with(char::is_uppercase), "{name}");
            assert!(name.chars().all(char::is_alphanumeric), "{name}");
        }
        assert!(!is_agent_specific_builtin_tool("PlanCreate"));
        assert!(!is_agent_specific_builtin_tool("RoleCatalog"));
        assert!(!is_agent_specific_builtin_tool("AsyncStatus"));
    }

    #[test]
    fn is_builtin_tool_is_case_insensitive() {
        assert!(is_builtin_tool("Bash"));
        assert!(is_builtin_tool("bash"));
        assert!(is_builtin_tool("MODEL_LIST"));
        assert!(is_builtin_tool("ROLE_CATALOG"));
        assert!(is_agent_specific_builtin_tool("Agent"));
        assert!(!is_builtin_tool("nope"));
    }
}
