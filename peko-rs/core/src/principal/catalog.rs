//! Per-principal catalog.
//!
//! Phase 1 of ADR-047 (`Principal Workspace as the Tooling Trust Boundary`).
//! Renamed from `ExtensionCatalog` to `PrincipalCatalog`; same flat
//! `(name → entry)` shape, same data sources, plus a workspace scan over
//! `<workspace>/{tools,skills,mcp,hooks,plugins}/` that surfaces
//! directories as catalog entries. The catalog is a derived snapshot —
//! it does not own handlers or lifecycle; tool dispatch still flows
//! through `peko_engine::funnel`.
//!
//! ADR-066 P2 deleted the capability evaluation: every detected entry is
//! `enabled` (presence = visibility = executability). Built from:
//! 1. Built-in tools (`builtin_tools::all_tool_names()`).
//! 2. Agent prompts under `<workspace>/agents/` (loaded by
//!    `agent_prompt::load_agent_prompt`, passed in here).
//! 3. Workspace scan over `<workspace>/{tools,skills,mcp,hooks,plugins}/`.
//!    Each subdirectory is one catalog entry (id = basename,
//!    kind = parent dir).

use std::collections::HashMap;
use std::path::Path;

use crate::principal::runtime::builtin_tools;
use crate::principal::AgentPrompt;

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
    /// Always `true` since ADR-066 P2 (no capability gate). The field
    /// stays so the `peko show` catalog payload keeps its shape.
    pub enabled: bool,
    /// Capabilities this entity declares it provides. Empty for entities
    /// (built-ins, agents, workspace scan entries) whose capability is
    /// implicit. Inert metadata since ADR-066 P2.
    pub provides: Vec<String>,
}

/// Per-principal snapshot of all detected tooling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalCatalog {
    entries: Vec<CatalogEntry>,
}

impl PrincipalCatalog {
    /// Build a `PrincipalCatalog` from the principal's workspace.
    ///
    /// * `workspace` — the principal's workspace root. The scan walks
    ///   `{tools,skills,mcp,hooks,plugins}/` if those directories exist;
    ///   missing directories produce no entries.
    /// * `agent_prompts` — agents discovered under `<workspace>/agents/`.
    #[must_use]
    pub fn build(workspace: &Path, agent_prompts: &HashMap<String, AgentPrompt>) -> Self {
        let mut entries: Vec<CatalogEntry> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1. Built-in tools.
        for name in builtin_tools::all_tool_names() {
            let id = format!("builtin:tool:{name}");
            if seen.insert(id.clone()) {
                entries.push(CatalogEntry {
                    id,
                    name: name.to_string(),
                    kind: "builtin".to_string(),
                    source: None,
                    enabled: true,
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
                    enabled: true,
                    provides: Vec::new(),
                });
            }
        }

        // 3. Workspace scan — additive over sources 1-2. A directory's
        //    basename is the entry id; the parent directory name is the
        //    kind.
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
                entries.push(CatalogEntry {
                    id: id.clone(),
                    name: id,
                    kind: (*kind).to_string(),
                    source: None,
                    enabled: true,
                    provides: Vec::new(),
                });
            }
        }

        Self { entries }
    }

    /// All entries in the catalog, ordered built-ins, agents, then
    /// workspace scan.
    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }
}

/// `(directory_name, kind)` pairs the workspace scanner looks at.
///
/// Order is significant — it controls the order entries are appended
/// to the catalog after sources 1-2.
const WORKSPACE_KIND_DIRS: &[(&str, &str)] = &[
    ("tools", "tool"),
    ("skills", "skill"),
    ("mcp", "mcp"),
    ("hooks", "hook"),
    ("plugins", "plugin"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn agent(name: &str) -> AgentPrompt {
        AgentPrompt {
            name: name.to_string(),
            path: PathBuf::from(format!("roles/{name}.md")),
            body: "body".to_string(),
            frontmatter: Default::default(),
        }
    }

    fn empty_workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn builtins_and_agents_all_present_and_enabled() {
        let workspace = empty_workspace();
        let mut agents = HashMap::new();
        agents.insert("math".to_string(), agent("math"));

        let catalog = PrincipalCatalog::build(workspace.path(), &agents);

        let bash = catalog
            .entries()
            .iter()
            .find(|e| e.id == "builtin:tool:Bash")
            .expect("Bash should be present");
        assert!(bash.enabled);
        let math = catalog
            .entries()
            .iter()
            .find(|e| e.id == "math")
            .expect("math agent should be present");
        assert!(math.enabled);
        assert_eq!(math.kind, "agent");
    }

    #[test]
    fn workspace_entries_appear_and_are_enabled() {
        let workspace = empty_workspace();
        std::fs::create_dir_all(workspace.path().join("tools").join("my-tool")).unwrap();
        std::fs::create_dir_all(workspace.path().join("skills").join("docker")).unwrap();
        std::fs::create_dir_all(workspace.path().join("plugins").join("weird-thing")).unwrap();

        let catalog = PrincipalCatalog::build(workspace.path(), &HashMap::new());
        for (id, kind) in [
            ("my-tool", "tool"),
            ("docker", "skill"),
            ("weird-thing", "plugin"),
        ] {
            let entry = catalog
                .entries()
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("{id} should be present"));
            assert_eq!(entry.kind, kind);
            assert!(entry.enabled, "{id} is enabled by presence");
        }
    }

    #[test]
    fn missing_workspace_dirs_are_silent() {
        let workspace = empty_workspace();
        // No tools/, skills/, etc. directories exist.
        let catalog = PrincipalCatalog::build(workspace.path(), &HashMap::new());
        let workspace_entries: Vec<_> = catalog
            .entries()
            .iter()
            .filter(|e| {
                matches!(
                    e.kind.as_str(),
                    "tool" | "skill" | "mcp" | "hook" | "plugin"
                )
            })
            .collect();
        assert!(workspace_entries.is_empty());
    }

    #[test]
    fn non_directory_workspace_entries_are_skipped() {
        let workspace = empty_workspace();
        std::fs::create_dir_all(workspace.path().join("tools")).unwrap();
        std::fs::write(workspace.path().join("tools").join("stray-file"), b"x").unwrap();

        let catalog = PrincipalCatalog::build(workspace.path(), &HashMap::new());
        let stray = catalog.entries().iter().find(|e| e.id == "stray-file");
        assert!(stray.is_none(), "files in tools/ must not become entries");
    }
}
