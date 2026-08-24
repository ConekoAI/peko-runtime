//! Per-principal catalog.
//!
//! Phase 1 of ADR-047 (`Principal Workspace as the Tooling Trust Boundary`).
//! Renamed from `ExtensionCatalog` to `PrincipalCatalog`; same flat
//! `(name → entry)` shape, same data sources, plus a workspace scan over
//! `<workspace>/{tools,skills,mcp,hooks,plugins}/` that surfaces
//! directories as catalog entries. The catalog is a derived snapshot —
//! it does not own handlers or lifecycle; tool dispatch still flows
//! through `peko_engine::funnel`. Phase 3 of ADR-047 lifts that funnel
//! and the catalog becomes the dispatch source.
//!
//! Built from:
//! 1. Built-in tools (`builtin_tools::all_tool_names()`).
//! 2. Agent prompts under `<workspace>/agents/` (loaded by
//!    `agent_prompt::load_agent_prompt`, passed in here).
//! 3. Installed extensions from the process-wide
//!    [`ExtensionStore`](peko_extension_host::store::ExtensionStore).
//! 4. Workspace scan over `<workspace>/{tools,skills,mcp,hooks,plugins}/`.
//!    Each subdirectory is one catalog entry (id = basename,
//!    kind = parent dir). The scan is additive — it does not replace
//!    any of sources 1-3.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::extensions::framework::store_trait::GlobalExtensionItem;
use crate::principal::capability_evaluator::CapabilityEvaluator;
use crate::principal::runtime::builtin_tools;
use crate::principal::AgentPrompt;
use peko_extension_api::{Capabilities, Capability, ExtensionManifest};

/// A single row in the principal's catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// Canonical identifier used when enabling/disabling the entity.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Kind discriminator (`builtin`, `agent`, `skill`, `mcp`, `hook`,
    /// `tool`, `plugin`).
    pub kind: String,
    /// Optional registry/package source reference.
    pub source: Option<String>,
    /// Whether this entity is currently enabled for the principal.
    pub enabled: bool,
    /// Capabilities this entity declares it provides. Empty for entities
    /// (built-ins, agents, workspace scan entries) whose capability is
    /// implicit.
    pub provides: Vec<String>,
}

/// Per-principal snapshot of all detected tooling and their authority
/// state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalCatalog {
    entries: Vec<CatalogEntry>,
}

impl PrincipalCatalog {
    /// Build a `PrincipalCatalog` from the principal's current authority
    /// snapshot plus a workspace scan.
    ///
    /// * `workspace` — the principal's workspace root. The scan walks
    ///   `{tools,skills,mcp,hooks,plugins}/` if those directories exist;
    ///   missing directories produce no entries.
    /// * `capabilities` — the principal's capability grants.
    /// * `agent_prompts` — agents discovered under `<workspace>/agents/`.
    /// * `global_items` — plain data from the process-wide `ExtensionStore`.
    ///   When empty the catalog contains only built-ins, agents, and
    ///   any workspace-resident entries.
    #[must_use]
    pub fn build(
        workspace: &Path,
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

        let mut entries: Vec<CatalogEntry> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // 1. Built-in tools.
        for name in builtin_tools::all_tool_names() {
            let id = format!("builtin:tool:{name}");
            if seen.insert(id.clone()) {
                entries.push(CatalogEntry {
                    id: id.clone(),
                    name: name.to_string(),
                    kind: "builtin".to_string(),
                    source: None,
                    enabled: is_allowed(name),
                    provides: Vec::new(),
                });
            }
        }

        // 2. Principal-scoped agents.
        for (id, prompt) in agent_prompts {
            if seen.insert(id.clone()) {
                entries.push(CatalogEntry {
                    id: id.clone(),
                    name: prompt.name.clone(),
                    kind: "agent".to_string(),
                    source: None,
                    enabled: is_allowed_with_kind("agent", id)
                        || is_allowed_with_kind("agent", &prompt.name),
                    provides: Vec::new(),
                });
            }
        }

        // 3. Installed extensions from the global ExtensionStore.
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
                entries.push(CatalogEntry {
                    id: id.clone(),
                    name: loaded.name.clone(),
                    kind: loaded.ext_type.clone(),
                    source: loaded.source.clone(),
                    enabled,
                    provides: loaded.provides.clone(),
                });
            }
        }

        // 4. Workspace scan — additive over sources 1-3. A directory's
        //    basename is the entry id; the parent directory name is the
        //    kind. Plugin entries are always enabled (no capability
        //    gating); tool/skill/mcp/hook entries are gated like the
        //    global-store entries above.
        for (dir_name, kind) in WORKSPACE_KIND_DIRS {
            let dir = workspace.join(dir_name);
            let read = match std::fs::read_dir(&dir) {
                Ok(it) => it,
                Err(_) => continue,
            };
            for entry in read.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if !seen.insert(id.clone()) {
                    continue;
                }
                let enabled = match *kind {
                    "plugin" => true,
                    _ => {
                        let cap = Capability::new(format!("{kind}:{id}"));
                        capabilities.is_granted(&cap)
                    }
                };
                entries.push(CatalogEntry {
                    id: id.clone(),
                    name: id.clone(),
                    kind: (*kind).to_string(),
                    source: None,
                    enabled,
                    provides: Vec::new(),
                });
            }
        }

        Self { entries }
    }

    /// All entries in the catalog, ordered built-ins, agents, installed
    /// extensions, then workspace scan.
    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// Return the set of entry IDs that are currently enabled.
    #[must_use]
    pub fn active_entries(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.id.clone())
            .collect()
    }

    /// Return the set of extension IDs that are currently enabled.
    ///
    /// Kept under the `active_extensions` name (matching the
    /// pre-rename `ExtensionCatalog` API) so existing callers in
    /// `peko_extension_api::ActiveExtensionSet` consumers keep
    /// compiling. Returns the IDs of every enabled entry; the kind
    /// discrimination lives in `CatalogEntry::kind`.
    #[must_use]
    pub fn active_extensions(&self) -> peko_extension_api::ActiveExtensionSet {
        peko_extension_api::ActiveExtensionSet::with_ids(self.active_entries())
    }

    /// All capabilities declared by detected tooling (installed,
    /// built-in, agents, and workspace-resident), regardless of whether
    /// they are granted.
    #[must_use]
    pub fn detected_capabilities(&self) -> Vec<String> {
        let mut set = HashSet::new();
        for entry in &self.entries {
            if entry.provides.is_empty() {
                match entry.kind.as_str() {
                    "builtin" => {
                        set.insert(format!("tool:{}", entry.name));
                    }
                    "agent" => {
                        set.insert(format!("agent:{}", entry.id));
                        set.insert(format!("agent:{}", entry.name));
                    }
                    other => {
                        let kind = capability_kind_for_extension_type(other);
                        set.insert(format!("{kind}:{}", entry.id));
                    }
                }
            } else {
                for p in &entry.provides {
                    set.insert(p.clone());
                }
            }
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        v
    }

    /// Capabilities that are currently active: the entity is enabled and
    /// at least one of its provided/implied capabilities is granted.
    #[must_use]
    pub fn active_capabilities(&self, capabilities: &Capabilities) -> Vec<String> {
        let mut set = HashSet::new();
        for entry in &self.entries {
            if !entry.enabled {
                continue;
            }
            if entry.provides.is_empty() {
                match entry.kind.as_str() {
                    "builtin" => {
                        let cap = format!("tool:{}", entry.name);
                        if capabilities.is_granted(&Capability::new(&cap)) {
                            set.insert(cap);
                        }
                    }
                    "agent" => {
                        for cap in [format!("agent:{}", entry.id), format!("agent:{}", entry.name)] {
                            if capabilities.is_granted(&Capability::new(&cap)) {
                                set.insert(cap);
                            }
                        }
                    }
                    "plugin" => {
                        // Plugins carry no implicit capability; they're
                        // enabled purely by presence in the workspace.
                    }
                    other => {
                        let kind = capability_kind_for_extension_type(other);
                        let cap = format!("{kind}:{}", entry.id);
                        if capabilities.is_granted(&Capability::new(&cap)) {
                            set.insert(cap);
                        }
                    }
                }
            } else {
                for p in &entry.provides {
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

/// `(directory_name, kind)` pairs the workspace scanner looks at.
///
/// Order is significant — it controls the order entries are appended
/// to the catalog after sources 1-3. Phase 2 of ADR-047 adds per-format
/// readers; until then these names are the entire contract between the
/// principal's workspace layout and the catalog.
const WORKSPACE_KIND_DIRS: &[(&str, &str)] = &[
    ("tools", "tool"),
    ("skills", "skill"),
    ("mcp", "mcp"),
    ("hooks", "hook"),
    ("plugins", "plugin"),
];

/// Map an extension type / kind string to the capability kind used in
/// grant requirements.
#[must_use]
pub fn capability_kind_for_extension_type(ext_type: &str) -> String {
    match ext_type {
        "builtin" | "tool" => "tool".to_string(),
        "agent" => "agent".to_string(),
        "skill" => "skill".to_string(),
        "mcp" => "mcp".to_string(),
        "hook" => "hook".to_string(),
        "gateway" => "gateway".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> AgentPrompt {
        AgentPrompt {
            name: name.to_string(),
            path: PathBuf::from(format!("agents/{name}/AGENT.md")),
            body: "body".to_string(),
            frontmatter: Default::default(),
        }
    }

    fn empty_workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn empty_allowlist_marks_everything_disabled() {
        let workspace = empty_workspace();
        let catalog = PrincipalCatalog::build(
            workspace.path(),
            &Capabilities::default(),
            &HashMap::new(),
            &[],
        );

        assert!(
            !catalog.entries().is_empty(),
            "catalog should still contain built-ins"
        );
        assert!(
            catalog.entries().iter().all(|e| !e.enabled),
            "every entry should be disabled with an empty allowlist"
        );
    }

    #[test]
    fn builtin_enabled_by_tool_capability() {
        let workspace = empty_workspace();
        let mut allowed = Capabilities::new();
        allowed.push("tool:Bash");

        let catalog =
            PrincipalCatalog::build(workspace.path(), &allowed, &HashMap::new(), &[]);
        let bash = catalog
            .entries()
            .iter()
            .find(|e| e.id == "builtin:tool:Bash")
            .expect("Bash should be present");
        assert!(bash.enabled);
    }

    #[test]
    fn builtin_enabled_by_tool_capability_wildcard() {
        let workspace = empty_workspace();
        let allowed = Capabilities::with_grants(["tool:*"]);

        let catalog =
            PrincipalCatalog::build(workspace.path(), &allowed, &HashMap::new(), &[]);
        let read = catalog
            .entries()
            .iter()
            .find(|e| e.id == "builtin:tool:Read")
            .expect("Read should be present");
        assert!(read.enabled);
    }

    #[test]
    fn agent_enabled_by_name() {
        let workspace = empty_workspace();
        let mut allowed = Capabilities::new();
        allowed.push("agent:math");

        let mut agents = HashMap::new();
        agents.insert("math".to_string(), agent("math"));

        let catalog =
            PrincipalCatalog::build(workspace.path(), &allowed, &agents, &[]);
        let math = catalog
            .entries()
            .iter()
            .find(|e| e.id == "math")
            .expect("math agent should be present");
        assert!(math.enabled);
        assert_eq!(math.kind, "agent");
    }

    #[test]
    fn disabled_agent_surfaces_in_catalog() {
        let workspace = empty_workspace();
        let mut allowed = Capabilities::new();
        allowed.push("agent:writer");

        let mut agents = HashMap::new();
        agents.insert("writer".to_string(), agent("writer"));
        agents.insert("researcher".to_string(), agent("researcher"));

        let catalog =
            PrincipalCatalog::build(workspace.path(), &allowed, &agents, &[]);
        let researcher = catalog
            .entries()
            .iter()
            .find(|e| e.id == "researcher")
            .expect("researcher should be present");
        assert!(!researcher.enabled);
    }

    #[test]
    fn global_extension_item_enabled_by_provides() {
        let workspace = empty_workspace();
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

        let catalog =
            PrincipalCatalog::build(workspace.path(), &allowed, &HashMap::new(), &global);
        let docker = catalog
            .entries()
            .iter()
            .find(|e| e.id == "docker-skill")
            .expect("docker skill should be present");
        assert!(docker.enabled);
        assert_eq!(docker.kind, "skill");
    }

    #[test]
    fn global_extension_item_disabled_when_required_missing() {
        let workspace = empty_workspace();
        let allowed = Capabilities::default();

        let global = vec![GlobalExtensionItem {
            id: "net-skill".to_string(),
            name: "Network".to_string(),
            ext_type: "skill".to_string(),
            source: None,
            provides: vec!["skill:network".to_string()],
            requires: vec!["tool:Read".to_string()],
        }];

        let catalog =
            PrincipalCatalog::build(workspace.path(), &allowed, &HashMap::new(), &global);
        let net = catalog
            .entries()
            .iter()
            .find(|e| e.id == "net-skill")
            .expect("network skill should be present");
        assert!(!net.enabled);
    }

    #[test]
    fn workspace_tool_entry_appears_when_directory_exists() {
        let workspace = empty_workspace();
        std::fs::create_dir_all(workspace.path().join("tools").join("my-tool")).unwrap();

        let mut allowed = Capabilities::new();
        allowed.push("tool:my-tool");

        let catalog =
            PrincipalCatalog::build(workspace.path(), &allowed, &HashMap::new(), &[]);
        let tool = catalog
            .entries()
            .iter()
            .find(|e| e.id == "my-tool")
            .expect("workspace tool should be present");
        assert_eq!(tool.kind, "tool");
        assert!(tool.enabled);
    }

    #[test]
    fn workspace_skill_entry_disabled_without_grant() {
        let workspace = empty_workspace();
        std::fs::create_dir_all(workspace.path().join("skills").join("docker")).unwrap();

        let catalog = PrincipalCatalog::build(
            workspace.path(),
            &Capabilities::default(),
            &HashMap::new(),
            &[],
        );
        let skill = catalog
            .entries()
            .iter()
            .find(|e| e.id == "docker")
            .expect("workspace skill should be present");
        assert_eq!(skill.kind, "skill");
        assert!(!skill.enabled);
    }

    #[test]
    fn workspace_plugin_entry_always_enabled() {
        let workspace = empty_workspace();
        std::fs::create_dir_all(workspace.path().join("plugins").join("weird-thing"))
            .unwrap();

        let catalog = PrincipalCatalog::build(
            workspace.path(),
            &Capabilities::default(),
            &HashMap::new(),
            &[],
        );
        let plugin = catalog
            .entries()
            .iter()
            .find(|e| e.id == "weird-thing")
            .expect("workspace plugin should be present");
        assert_eq!(plugin.kind, "plugin");
        assert!(plugin.enabled, "plugin entries are always enabled");
    }

    #[test]
    fn missing_workspace_dirs_are_silent() {
        let workspace = empty_workspace();
        // No tools/, skills/, etc. directories exist.
        let catalog = PrincipalCatalog::build(
            workspace.path(),
            &Capabilities::default(),
            &HashMap::new(),
            &[],
        );
        let workspace_entries: Vec<_> = catalog
            .entries()
            .iter()
            .filter(|e| matches!(e.kind.as_str(), "tool" | "skill" | "mcp" | "hook" | "plugin"))
            .collect();
        assert!(workspace_entries.is_empty());
    }

    #[test]
    fn non_directory_workspace_entries_are_skipped() {
        let workspace = empty_workspace();
        std::fs::create_dir_all(workspace.path().join("tools")).unwrap();
        std::fs::write(workspace.path().join("tools").join("stray-file"), b"x").unwrap();

        let catalog = PrincipalCatalog::build(
            workspace.path(),
            &Capabilities::default(),
            &HashMap::new(),
            &[],
        );
        let stray = catalog.entries().iter().find(|e| e.id == "stray-file");
        assert!(stray.is_none(), "files in tools/ must not become entries");
    }
}