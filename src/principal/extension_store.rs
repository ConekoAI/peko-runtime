//! Per-principal extension store.
//!
//! The `ExtensionStore` is a lightweight, per-message snapshot of every
//! extension-related entity the principal could conceivably use: built-in
//! tools, installed extensions, and principal-scoped agents. Each entry
//! carries an `enabled` flag derived from the principal's
//! `allowed_extensions` so callers (notably `agent_catalog`) can surface
//! installed-but-disabled entries without claiming they are callable.

use std::collections::{HashMap, HashSet};

use crate::extensions::framework::manager::ExtensionManager;
use crate::principal::agent_prompt::AgentPrompt;
use crate::principal::config::AllowedExtensions;

/// A single row in the principal's extension store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionStoreItem {
    /// Canonical identifier used when enabling/disabling the entity.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Extension type discriminator (`builtin`, `agent`, `skill`, `mcp`, etc.).
    pub ext_type: String,
    /// Optional registry/package source reference.
    pub source: Option<String>,
    /// Whether this entity is currently enabled for the principal.
    pub enabled: bool,
}

/// Per-principal snapshot of all detected extensions and their authority state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionStore {
    items: Vec<ExtensionStoreItem>,
}

impl ExtensionStore {
    /// Build an `ExtensionStore` from the principal's current authority
    /// snapshot.
    ///
    /// * `allowed_extensions` — the principal's allowlist.
    /// * `agent_prompts` — agents discovered under `<workspace>/agents/`.
    /// * `extension_manager` — optional daemon extension manager; when
    ///   absent the store contains only built-ins and principal agents.
    #[must_use]
    pub fn build(
        allowed_extensions: &AllowedExtensions,
        agent_prompts: &HashMap<String, AgentPrompt>,
        extension_manager: Option<&ExtensionManager>,
    ) -> Self {
        let allowlist: HashSet<String> = allowed_extensions
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();

        let is_allowed = |name: &str| allowlist.contains(&name.to_ascii_lowercase());

        let mut items: Vec<ExtensionStoreItem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // Built-in tools.
        for name in crate::extensions::framework::adapters::builtin_tools::all_tool_names() {
            let id = name.to_string();
            let canonical = format!("builtin:tool:{name}");
            if seen.insert(id.clone()) {
                items.push(ExtensionStoreItem {
                    id: id.clone(),
                    name: id.clone(),
                    ext_type: "builtin".to_string(),
                    source: None,
                    enabled: is_allowed(&id) || is_allowed(&canonical),
                });
            }
        }

        // Principal-scoped agents.
        for (id, prompt) in agent_prompts {
            if seen.insert(id.clone()) {
                items.push(ExtensionStoreItem {
                    id: id.clone(),
                    name: prompt.name.clone(),
                    ext_type: "agent".to_string(),
                    source: None,
                    enabled: is_allowed(id) || is_allowed(&prompt.name),
                });
            }
        }

        // Installed extensions from the daemon extension manager.
        if let Some(manager) = extension_manager {
            for loaded in manager.list_extensions() {
                let id = loaded.manifest.id.0.clone();
                if seen.insert(id.clone()) {
                    items.push(ExtensionStoreItem {
                        id: id.clone(),
                        name: loaded.manifest.name.clone(),
                        ext_type: loaded.extension_type.clone(),
                        source: loaded.manifest.source.clone(),
                        enabled: is_allowed(&id) || is_allowed(&loaded.manifest.name),
                    });
                }
            }
        }

        Self { items }
    }

    /// All items in the store, ordered built-ins, agents, then installed
    /// extensions.
    #[must_use]
    pub fn items(&self) -> &[ExtensionStoreItem] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn agent(name: &str) -> AgentPrompt {
        AgentPrompt {
            name: name.to_string(),
            path: PathBuf::from(format!("agents/{name}/AGENT.md")),
            body: "body".to_string(),
            frontmatter: Default::default(),
        }
    }

    #[test]
    fn empty_allowlist_marks_everything_disabled() {
        let store = ExtensionStore::build(&AllowedExtensions::default(), &HashMap::new(), None);

        assert!(
            !store.items().is_empty(),
            "store should still contain built-ins"
        );
        assert!(
            store.items().iter().all(|i| !i.enabled),
            "every entry should be disabled with an empty allowlist"
        );
    }

    #[test]
    fn builtin_enabled_by_bare_name() {
        let mut allowed = AllowedExtensions::new();
        allowed.push("Bash");

        let store = ExtensionStore::build(&allowed, &HashMap::new(), None);
        let bash = store
            .items()
            .iter()
            .find(|i| i.id == "Bash")
            .expect("Bash should be present");
        assert!(bash.enabled);
    }

    #[test]
    fn builtin_enabled_by_canonical_id() {
        let mut allowed = AllowedExtensions::new();
        allowed.push("builtin:tool:Read");

        let store = ExtensionStore::build(&allowed, &HashMap::new(), None);
        let read = store
            .items()
            .iter()
            .find(|i| i.id == "Read")
            .expect("Read should be present");
        assert!(read.enabled);
    }

    #[test]
    fn agent_enabled_case_insensitive() {
        let mut allowed = AllowedExtensions::new();
        allowed.push("MATH");

        let mut agents = HashMap::new();
        agents.insert("math".to_string(), agent("math"));

        let store = ExtensionStore::build(&allowed, &agents, None);
        let math = store
            .items()
            .iter()
            .find(|i| i.id == "math")
            .expect("math agent should be present");
        assert!(math.enabled);
        assert_eq!(math.ext_type, "agent");
    }

    #[test]
    fn disabled_agent_surfaces_in_store() {
        let mut allowed = AllowedExtensions::new();
        allowed.push("writer");

        let mut agents = HashMap::new();
        agents.insert("writer".to_string(), agent("writer"));
        agents.insert("researcher".to_string(), agent("researcher"));

        let store = ExtensionStore::build(&allowed, &agents, None);
        let researcher = store
            .items()
            .iter()
            .find(|i| i.id == "researcher")
            .expect("researcher should be present");
        assert!(!researcher.enabled);
    }
}
