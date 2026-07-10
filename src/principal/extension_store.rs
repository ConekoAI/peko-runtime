//! Per-principal extension catalog.
//!
//! The `ExtensionCatalog` is a lightweight, per-message snapshot of every
//! extension-related entity the principal could conceivably use: built-in
//! tools, installed extensions, and principal-scoped agents. Each entry
//! carries an `enabled` flag derived from the principal's
//! `capabilities` so callers (notably `agent_catalog`) can surface
//! installed-but-disabled entries without claiming they are callable.
//!
//! It is built from plain [`GlobalExtensionItem`] data produced by the
//! process-wide [`ExtensionStore`](crate::extensions::framework::store::ExtensionStore)
//! so the per-Principal view does not need to hold a reference to the global
//! store or acquire its lock.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::extensions::framework::store::GlobalExtensionItem;
use crate::extensions::framework::types::ExtensionManifest;
use crate::principal::agent_prompt::AgentPrompt;
use crate::principal::capability::{Capabilities, Capability};
use crate::principal::capability_evaluator::CapabilityEvaluator;

/// A single row in the principal's extension catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCatalogItem {
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
    /// Capabilities this entity declares it provides. Empty for entities
    /// (built-ins, agents) whose capability is implicit.
    pub provides: Vec<String>,
}

/// Per-principal snapshot of all detected extensions and their authority state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionCatalog {
    items: Vec<ExtensionCatalogItem>,
}

impl ExtensionCatalog {
    /// Build an `ExtensionCatalog` from the principal's current authority
    /// snapshot.
    ///
    /// * `capabilities` — the principal's capability grants.
    /// * `agent_prompts` — agents discovered under `<workspace>/agents/`.
    /// * `global_items` — plain data from the process-wide `ExtensionStore`.
    ///   When empty the catalog contains only built-ins and principal agents.
    #[must_use]
    pub fn build(
        capabilities: &Capabilities,
        agent_prompts: &HashMap<String, AgentPrompt>,
        global_items: &[GlobalExtensionItem],
    ) -> Self {
        let has_any_grant = !capabilities.is_empty();

        let is_allowed = |name: &str| {
            if !has_any_grant {
                return false;
            }
            let required = Capability::new(format!("tool:{name}"));
            capabilities.is_granted(&required)
        };

        let is_allowed_with_kind = |kind: &str, name: &str| {
            if !has_any_grant {
                return false;
            }
            let required = Capability::new(format!("{kind}:{name}"));
            capabilities.is_granted(&required)
        };

        let mut items: Vec<ExtensionCatalogItem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // Built-in tools.
        for name in crate::extensions::framework::adapters::builtin_tools::all_tool_names() {
            let id = format!("builtin:tool:{name}");
            if seen.insert(id.clone()) {
                items.push(ExtensionCatalogItem {
                    id: id.clone(),
                    name: name.to_string(),
                    ext_type: "builtin".to_string(),
                    source: None,
                    enabled: is_allowed(name),
                    provides: Vec::new(),
                });
            }
        }

        // Principal-scoped agents.
        for (id, prompt) in agent_prompts {
            if seen.insert(id.clone()) {
                items.push(ExtensionCatalogItem {
                    id: id.clone(),
                    name: prompt.name.clone(),
                    ext_type: "agent".to_string(),
                    source: None,
                    enabled: is_allowed_with_kind("agent", id)
                        || is_allowed_with_kind("agent", &prompt.name),
                    provides: Vec::new(),
                });
            }
        }

        // Installed extensions from the global ExtensionStore.
        let evaluator = CapabilityEvaluator::new();
        for loaded in global_items {
            let id = loaded.id.clone();
            if seen.insert(id.clone()) {
                let kind = capability_kind_for_extension_type(&loaded.ext_type);
                let mut manifest = ExtensionManifest::new(
                    &loaded.id,
                    &loaded.ext_type,
                    &loaded.name,
                    "",
                    "0.0.0",
                    PathBuf::new(),
                );
                manifest.provides.clone_from(&loaded.provides);
                manifest.requires.clone_from(&loaded.requires);
                let enabled = evaluator.is_extension_active(&manifest, capabilities, Some(&kind));
                items.push(ExtensionCatalogItem {
                    id: id.clone(),
                    name: loaded.name.clone(),
                    ext_type: loaded.ext_type.clone(),
                    source: loaded.source.clone(),
                    enabled,
                    provides: loaded.provides.clone(),
                });
            }
        }

        Self { items }
    }

    /// All items in the catalog, ordered built-ins, agents, then installed
    /// extensions.
    #[must_use]
    pub fn items(&self) -> &[ExtensionCatalogItem] {
        &self.items
    }

    /// Return the set of extension IDs that are currently enabled.
    #[must_use]
    pub fn active_extensions(&self) -> crate::principal::ActiveExtensionSet {
        crate::principal::ActiveExtensionSet::with_ids(
            self.items
                .iter()
                .filter(|i| i.enabled)
                .map(|i| i.id.clone()),
        )
    }

    /// All capabilities declared by detected extensions (installed, built-in,
    /// and principal-scoped agents), regardless of whether they are granted.
    #[must_use]
    pub fn detected_capabilities(&self) -> Vec<String> {
        let mut set = HashSet::new();
        for item in &self.items {
            if item.provides.is_empty() {
                match item.ext_type.as_str() {
                    "builtin" => {
                        set.insert(format!("tool:{}", item.name));
                    }
                    "agent" => {
                        set.insert(format!("agent:{}", item.id));
                        set.insert(format!("agent:{}", item.name));
                    }
                    other => {
                        let kind = capability_kind_for_extension_type(other);
                        set.insert(format!("{kind}:{}", item.id));
                    }
                }
            } else {
                for p in &item.provides {
                    set.insert(p.clone());
                }
            }
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        v
    }

    /// Capabilities that are currently active: the entity is enabled and at
    /// least one of its provided/implied capabilities is granted.
    #[must_use]
    pub fn active_capabilities(&self, capabilities: &Capabilities) -> Vec<String> {
        let mut set = HashSet::new();
        for item in &self.items {
            if !item.enabled {
                continue;
            }
            if item.provides.is_empty() {
                match item.ext_type.as_str() {
                    "builtin" => {
                        let cap = format!("tool:{}", item.name);
                        if capabilities.is_granted(&Capability::new(&cap)) {
                            set.insert(cap);
                        }
                    }
                    "agent" => {
                        for cap in [format!("agent:{}", item.id), format!("agent:{}", item.name)] {
                            if capabilities.is_granted(&Capability::new(&cap)) {
                                set.insert(cap);
                            }
                        }
                    }
                    other => {
                        let kind = capability_kind_for_extension_type(other);
                        let cap = format!("{kind}:{}", item.id);
                        if capabilities.is_granted(&Capability::new(&cap)) {
                            set.insert(cap);
                        }
                    }
                }
            } else {
                for p in &item.provides {
                    if capabilities.is_granted(&Capability::new(p)) {
                        set.insert(p.clone());
                    }
                }
            }
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        v
    }
}

/// Map an extension type string to the capability kind used in grant
/// requirements.
pub(crate) fn capability_kind_for_extension_type(ext_type: &str) -> String {
    match ext_type {
        "builtin" | "tool" => "tool".to_string(),
        "agent" => "agent".to_string(),
        "skill" => "skill".to_string(),
        "mcp" => "mcp".to_string(),
        "gateway" => "gateway".to_string(),
        other => other.to_string(),
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
        let catalog = ExtensionCatalog::build(&Capabilities::default(), &HashMap::new(), &[]);

        assert!(
            !catalog.items().is_empty(),
            "catalog should still contain built-ins"
        );
        assert!(
            catalog.items().iter().all(|i| !i.enabled),
            "every entry should be disabled with an empty allowlist"
        );
    }

    #[test]
    fn builtin_enabled_by_tool_capability() {
        let mut allowed = Capabilities::new();
        allowed.push("tool:Bash");

        let catalog = ExtensionCatalog::build(&allowed, &HashMap::new(), &[]);
        let bash = catalog
            .items()
            .iter()
            .find(|i| i.id == "builtin:tool:Bash")
            .expect("Bash should be present");
        assert!(bash.enabled);
    }

    #[test]
    fn builtin_enabled_by_tool_capability_wildcard() {
        let allowed = Capabilities::with_grants(["tool:*"]);

        let catalog = ExtensionCatalog::build(&allowed, &HashMap::new(), &[]);
        let read = catalog
            .items()
            .iter()
            .find(|i| i.id == "builtin:tool:Read")
            .expect("Read should be present");
        assert!(read.enabled);
    }

    #[test]
    fn agent_enabled_by_name() {
        let mut allowed = Capabilities::new();
        allowed.push("agent:math");

        let mut agents = HashMap::new();
        agents.insert("math".to_string(), agent("math"));

        let catalog = ExtensionCatalog::build(&allowed, &agents, &[]);
        let math = catalog
            .items()
            .iter()
            .find(|i| i.id == "math")
            .expect("math agent should be present");
        assert!(math.enabled);
        assert_eq!(math.ext_type, "agent");
    }

    #[test]
    fn disabled_agent_surfaces_in_catalog() {
        let mut allowed = Capabilities::new();
        allowed.push("agent:writer");

        let mut agents = HashMap::new();
        agents.insert("writer".to_string(), agent("writer"));
        agents.insert("researcher".to_string(), agent("researcher"));

        let catalog = ExtensionCatalog::build(&allowed, &agents, &[]);
        let researcher = catalog
            .items()
            .iter()
            .find(|i| i.id == "researcher")
            .expect("researcher should be present");
        assert!(!researcher.enabled);
    }

    #[test]
    fn global_extension_item_enabled_by_provides() {
        let mut allowed = Capabilities::new();
        allowed.push("skill:docker");

        let global = vec![GlobalExtensionItem {
            id: "docker-skill".to_string(),
            name: "Docker".to_string(),
            ext_type: "skill".to_string(),
            source: None,
            provides: vec!["skill:docker".to_string()],
            requires: vec![],
        }];

        let catalog = ExtensionCatalog::build(&allowed, &HashMap::new(), &global);
        let docker = catalog
            .items()
            .iter()
            .find(|i| i.id == "docker-skill")
            .expect("docker skill should be present");
        assert!(docker.enabled);
        assert_eq!(docker.ext_type, "skill");
    }

    #[test]
    fn global_extension_item_disabled_when_required_missing() {
        let allowed = Capabilities::new();

        let global = vec![GlobalExtensionItem {
            id: "net-skill".to_string(),
            name: "Network".to_string(),
            ext_type: "skill".to_string(),
            source: None,
            provides: vec!["skill:network".to_string()],
            requires: vec!["tool:Read".to_string()],
        }];

        let catalog = ExtensionCatalog::build(&allowed, &HashMap::new(), &global);
        let net = catalog
            .items()
            .iter()
            .find(|i| i.id == "net-skill")
            .expect("network skill should be present");
        assert!(!net.enabled);
    }
}
