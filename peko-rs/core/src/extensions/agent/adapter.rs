//! Agent adapter for the Extension system
//!
//! Discovers `AGENT.md` files in the principal's workspace
//! (`<workspace>/agents/<id>.md` or `<workspace>/agents/<id>/AGENT.md`)
//! and renders them into the `agents` system-prompt section via the
//! workspace-scanning [`WorkspaceAgentsPromptHandler`]. The engine
//! prompt renderer dispatches that hook on every iteration as part of
//! the per-turn volatile suffix (see
//! `peko-rs/engine/src/prompt/renderer.rs`), so agents added to the
//! workspace appear in the prompt on the next iteration.
//!
//! PR-C.4: `ExtensionTypeAdapter` trait impl + `AgentPromptHandlerFactory`
//! deleted. The trait impl was the framework-coupling path; both it
//! and the factory that wrapped `AgentPromptHandler` had zero callers
//! once `BuiltInAdapters` was gutted (PR-C.1).
//!
//! Part B (dynamic per-turn workspace catalog): the static per-agent
//! `AgentPromptHandler` + `register_agents_with_core` registration was
//! replaced by the single scanning `WorkspaceAgentsPromptHandler`,
//! which resolves the workspace from the hook context at invoke time
//! and re-scans `agents/` whenever the directory mtime changes.
//! Presence in the workspace = visible (ADR-047) — no capability or
//! active-extension filter. The remaining surface is
//! `AgentAdapter::discover_agents` (also called from
//! `principal/manager.rs`) + the data types it produces.

use crate::extensions::framework::adapters::parsing;
use crate::extensions::framework::core::{HookContext, HookHandler, HookPoint};
use crate::extensions::framework::types::{ExtensionManifest, HookOutput, HookResult};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use tracing::{debug, warn};

/// Agent extension type identifier
pub const AGENT_EXTENSION_TYPE: &str = "agent";

/// Default priority for agent prompt injection
pub const AGENT_HOOK_PRIORITY: i32 = 90;

/// Agent adapter for Extension system
#[derive(Debug)]
pub struct AgentAdapter;

impl AgentAdapter {
    /// Create a new agent adapter
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Discover agents from a directory.
    ///
    /// Supports two layouts:
    /// - Directory layout: `agents/<id>/AGENT.md`
    /// - Flat layout: `agents/<id>.md`
    ///
    /// The canonical agent id is the directory name for directory layouts and
    /// the file stem for flat layouts. The frontmatter `name` is used only as
    /// the human-readable display name.
    pub fn discover_agents(&self, path: &Path) -> Vec<DiscoveredAgent> {
        let mut agents = Vec::new();

        if !path.exists() {
            debug!("Agents directory does not exist: {:?}", path);
            return agents;
        }

        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read agents directory {:?}: {}", path, e);
                return agents;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                let agent_md = path.join("AGENT.md");
                if agent_md.exists() {
                    match self.parse_agent_manifest(&agent_md) {
                        Ok(manifest) => {
                            agents.push(DiscoveredAgent {
                                manifest,
                                file_path: agent_md,
                                base_dir: path,
                            });
                        }
                        Err(e) => {
                            warn!("Failed to parse agent from {:?}: {}", agent_md, e);
                        }
                    }
                }
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            {
                match self.parse_agent_manifest(&path) {
                    Ok(manifest) => {
                        agents.push(DiscoveredAgent {
                            manifest,
                            file_path: path.clone(),
                            base_dir: path
                                .parent()
                                .unwrap_or_else(|| Path::new("."))
                                .to_path_buf(),
                        });
                    }
                    Err(e) => {
                        warn!("Failed to parse agent from {:?}: {}", path, e);
                    }
                }
            }
        }

        agents
    }

    /// Parse an AGENT.md file into an extension manifest
    fn parse_agent_manifest(&self, path: &Path) -> Result<ExtensionManifest> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;

        let (meta, _body): (AgentFrontmatter, _) = parsing::parse_yaml_frontmatter_typed(&content)
            .with_context(|| format!("Failed to parse frontmatter in {path:?}"))?;

        if meta.name.is_empty() {
            anyhow::bail!("Agent name cannot be empty");
        }
        if meta.description.is_empty() {
            anyhow::bail!("Agent description cannot be empty");
        }

        let canonical_id = canonical_id_from_path(path);
        if canonical_id.is_empty() {
            anyhow::bail!("Agent canonical id cannot be empty for {path:?}");
        }

        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let mut manifest = ExtensionManifest::new(
            &canonical_id,
            AGENT_EXTENSION_TYPE,
            &meta.name,
            &meta.description,
            "1.0.0",
            base_dir,
        );

        manifest.set("agent_file", path.to_string_lossy().to_string());
        manifest.set("color", meta.color.unwrap_or_default());

        Ok(manifest)
    }
}

/// Derive the canonical agent id from its on-disk path.
///
/// For the directory layout (`agents/<id>/AGENT.md`) the id is the directory
/// name. For the flat layout (`agents/<id>.md`) the id is the file stem.
fn canonical_id_from_path(path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();

    if file_name.eq_ignore_ascii_case("AGENT.md") {
        path.parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

impl Default for AgentAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// A discovered agent before registration
#[derive(Debug, Clone)]
pub struct DiscoveredAgent {
    /// Extension manifest
    pub manifest: ExtensionManifest,
    /// Full path to AGENT.md
    pub file_path: PathBuf,
    /// Agent base directory
    pub base_dir: PathBuf,
}

/// YAML frontmatter from AGENT.md
#[derive(Debug, Deserialize)]
struct AgentFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    color: Option<String>,
}

/// Workspace-scanning handler for the `agents` prompt section.
///
/// Registered **once** per core (see `principal/context.rs`), not per
/// agent: at invoke time it reads the workspace from the hook context's
/// `ToolRuntimeContext`, scans `<workspace>/agents/` via
/// [`AgentAdapter::discover_agents`], and renders one line per agent in
/// the format `- {name} (id: {id}): {description} (location: {path})`.
///
/// Presence in the workspace = visible (ADR-047): there is deliberately
/// **no** capability or active-extension filter. A missing workspace or
/// an empty `agents/` directory yields [`HookResult::PassThrough`] so
/// the section is stripped from the prompt.
///
/// The scan result is cached in a `Mutex` keyed on the `agents/`
/// directory's mtime — each call `stat`s the directory (cheap) and only
/// re-reads the agent files when the mtime changed. That keeps the
/// handler well within the renderer's 2-second hook timeout.
#[derive(Debug, Default)]
pub struct WorkspaceAgentsPromptHandler {
    cache: Mutex<Option<((SystemTime, usize), String)>>,
}

impl WorkspaceAgentsPromptHandler {
    /// Create a new workspace-scanning agents handler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the agents catalog for `workspace`, using the mtime-keyed
    /// cache. Returns `None` when there is nothing to render (no
    /// `agents/` dir, or no agents discovered).
    ///
    /// The cache key is `(dir_mtime, immediate_child_count)`. Mtime alone
    /// is unreliable on Windows NTFS for fast back-to-back subdir/file
    /// creations (the parent dir's mtime can read back the same value
    /// before the metadata update has been flushed); counting children
    /// catches added/removed entries even when mtime is stale. Same
    /// shape as the sibling `WorkspaceSkillsPromptHandler` cache.
    fn render_catalog(&self, workspace: &str) -> Option<String> {
        let agents_dir = Path::new(workspace).join("agents");
        let metadata = std::fs::metadata(&agents_dir).ok()?;
        let mtime = metadata.modified().ok()?;
        let child_count = std::fs::read_dir(&agents_dir).ok()?.count();
        let key = (mtime, child_count);

        {
            let cache = self.cache.lock().expect("agents catalog cache poisoned");
            if let Some((cached_key, text)) = &*cache {
                if *cached_key == key {
                    return (!text.is_empty()).then(|| text.clone());
                }
            }
        }

        let agents = AgentAdapter::new().discover_agents(&agents_dir);
        let text = agents
            .iter()
            .map(|a| {
                // Normalize separators to `/` so the catalog renders
                // portably across platforms (Windows: `to_string_lossy()`
                // preserves `\`, which would break the substring
                // assertion in `workspace_agents_handler_renders_catalog`
                // and produce a less-readable location string for
                // users). Matches the format style used by the sibling
                // `WorkspaceSkillsPromptHandler` (skills/{name}/SKILL.md).
                let location = a.file_path.to_string_lossy().replace('\\', "/");
                format!(
                    "- {} (id: {}): {} (location: {})",
                    a.manifest.name,
                    a.manifest.id.0,
                    a.manifest.description,
                    location
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut cache = self.cache.lock().expect("agents catalog cache poisoned");
        *cache = Some((key, text.clone()));

        (!text.is_empty()).then_some(text)
    }
}

#[async_trait]
impl HookHandler for WorkspaceAgentsPromptHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        let workspace = ctx
            .get_state::<crate::extensions::framework::types::ToolRuntimeContext>("tool_context")
            .and_then(|rtc| rtc.workspace.clone());

        let Some(workspace) = workspace.filter(|w| !w.is_empty()) else {
            return HookResult::PassThrough;
        };

        match self.render_catalog(&workspace) {
            Some(text) => HookResult::Continue(HookOutput::Text(text)),
            None => HookResult::PassThrough,
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "agents".to_string(),
            priority: AGENT_HOOK_PRIORITY,
        }
    }

    fn priority(&self) -> i32 {
        AGENT_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        "WorkspaceAgentsPromptHandler".to_string()
    }
}

/// Helper to load agents from directory using the adapter
#[must_use]
pub fn load_agents_from_directory(path: &Path) -> Vec<DiscoveredAgent> {
    let adapter = AgentAdapter::new();
    adapter.discover_agents(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::core::ExtensionServices;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn create_test_agent(dir: &Path, name: &str, description: &str) -> PathBuf {
        let agent_dir = dir.join(name);
        std::fs::create_dir(&agent_dir).unwrap();

        let content = format!(
            r"---
name: {name}
description: {description}
color: '#ff0000'
---

# Test Agent

This is a test agent.
"
        );

        let agent_md = agent_dir.join("AGENT.md");
        std::fs::write(&agent_md, content).unwrap();
        agent_md
    }

    fn create_test_agent_flat(dir: &Path, name: &str, description: &str) -> PathBuf {
        let content = format!(
            r"---
name: {name}
description: {description}
color: '#ff0000'
---

# Test Agent

This is a test agent.
"
        );

        let agent_md = dir.join(format!("{name}.md"));
        std::fs::write(&agent_md, content).unwrap();
        agent_md
    }

    #[test]
    fn test_discover_agents() {
        let temp = TempDir::new().unwrap();

        create_test_agent(temp.path(), "agent1", "First agent");
        create_test_agent(temp.path(), "agent2", "Second agent");

        let adapter = AgentAdapter::new();
        let agents = adapter.discover_agents(temp.path());

        assert_eq!(agents.len(), 2);
        assert!(agents.iter().any(|a| a.manifest.id.0 == "agent1"));
        assert!(agents.iter().any(|a| a.manifest.id.0 == "agent2"));
    }

    #[test]
    fn test_discover_agents_flat_files() {
        let temp = TempDir::new().unwrap();

        create_test_agent_flat(temp.path(), "agent1", "First agent");
        create_test_agent_flat(temp.path(), "agent2", "Second agent");

        let adapter = AgentAdapter::new();
        let agents = adapter.discover_agents(temp.path());

        assert_eq!(agents.len(), 2);
        assert!(agents.iter().any(|a| a.manifest.id.0 == "agent1"));
        assert!(agents.iter().any(|a| a.manifest.id.0 == "agent2"));
        assert!(agents
            .iter()
            .any(|a| a.file_path == temp.path().join("agent1.md")));
        assert!(agents
            .iter()
            .any(|a| a.file_path == temp.path().join("agent2.md")));
    }

    #[test]
    fn test_discover_agents_mixed_layouts() {
        let temp = TempDir::new().unwrap();

        create_test_agent(temp.path(), "dir-agent", "Directory layout agent");
        create_test_agent_flat(temp.path(), "flat-agent", "Flat layout agent");

        let adapter = AgentAdapter::new();
        let agents = adapter.discover_agents(temp.path());

        assert_eq!(agents.len(), 2);
        assert!(agents.iter().any(|a| a.manifest.id.0 == "dir-agent"));
        assert!(agents.iter().any(|a| a.manifest.id.0 == "flat-agent"));
    }

    #[test]
    fn test_parse_agent_manifest() {
        let temp = TempDir::new().unwrap();
        let agent_md = create_test_agent(temp.path(), "math", "Math operations");

        let adapter = AgentAdapter::new();
        let manifest = adapter.parse_agent_manifest(&agent_md).unwrap();

        assert_eq!(manifest.id.0, "math");
        assert_eq!(manifest.name, "math");
        assert_eq!(manifest.description, "Math operations");
        assert_eq!(manifest.extension_type, "agent");
    }

    #[test]
    fn test_parse_agent_manifest_uses_canonical_id() {
        let temp = TempDir::new().unwrap();
        let agent_dir = temp.path().join("senior-developer");
        std::fs::create_dir(&agent_dir).unwrap();
        let agent_md = agent_dir.join("AGENT.md");
        std::fs::write(
            &agent_md,
            r"---
name: Senior Developer
description: Premium implementation specialist
color: '#ff0000'
---

# Test Agent
",
        )
        .unwrap();

        let adapter = AgentAdapter::new();
        let manifest = adapter.parse_agent_manifest(&agent_md).unwrap();

        assert_eq!(manifest.id.0, "senior-developer");
        assert_eq!(manifest.name, "Senior Developer");
        assert_eq!(manifest.description, "Premium implementation specialist");
    }

    /// Build a `PromptSystemSection { section: "agents" }` hook context,
    /// optionally carrying a workspace in the `tool_context` state.
    fn agents_hook_ctx(workspace: Option<&str>) -> HookContext {
        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "agents".to_string(),
                priority: AGENT_HOOK_PRIORITY,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        if let Some(ws) = workspace {
            ctx.set_state(
                "tool_context",
                crate::extensions::framework::types::ToolRuntimeContext::new()
                    .with_workspace(ws)
                    .with_principal_id("test-principal"),
            );
        }
        ctx
    }

    #[tokio::test]
    async fn workspace_agents_handler_renders_catalog() {
        let temp = TempDir::new().unwrap();
        let agents_dir = temp.path().join("agents");
        std::fs::create_dir(&agents_dir).unwrap();
        create_test_agent(&agents_dir, "math", "Math operations");
        create_test_agent_flat(&agents_dir, "reviewer", "Reviews code");

        let handler = WorkspaceAgentsPromptHandler::new();
        let result = handler
            .handle(agents_hook_ctx(Some(&temp.path().to_string_lossy())))
            .await;

        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(
                    text.contains("- math (id: math): Math operations"),
                    "got: {text}"
                );
                assert!(
                    text.contains("- reviewer (id: reviewer): Reviews code"),
                    "got: {text}"
                );
                assert!(text.contains("(location: "), "got: {text}");
                assert!(text.contains("agents/math/AGENT.md"), "got: {text}");
                assert!(text.contains("agents/reviewer.md"), "got: {text}");
            }
            _ => panic!("Expected Continue with Text, got {result:?}"),
        }
    }

    #[tokio::test]
    async fn workspace_agents_handler_passes_through_without_workspace() {
        let handler = WorkspaceAgentsPromptHandler::new();
        // No tool_context state at all → PassThrough.
        let result = handler.handle(agents_hook_ctx(None)).await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough without workspace, got {result:?}"
        );
    }

    #[tokio::test]
    async fn workspace_agents_handler_passes_through_on_empty_dir() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("agents")).unwrap();

        let handler = WorkspaceAgentsPromptHandler::new();
        let result = handler
            .handle(agents_hook_ctx(Some(&temp.path().to_string_lossy())))
            .await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough for empty agents dir, got {result:?}"
        );
    }

    #[tokio::test]
    async fn workspace_agents_handler_rescans_on_dir_mtime_change() {
        let temp = TempDir::new().unwrap();
        let agents_dir = temp.path().join("agents");
        std::fs::create_dir(&agents_dir).unwrap();
        create_test_agent(&agents_dir, "math", "Math operations");

        let handler = WorkspaceAgentsPromptHandler::new();
        let ws = temp.path().to_string_lossy().to_string();

        // First call scans and caches.
        let first = handler.handle(agents_hook_ctx(Some(&ws))).await;
        match &first {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(text.contains("math"), "got: {text}");
                assert!(!text.contains("reviewer"), "got: {text}");
            }
            _ => panic!("Expected Continue with Text, got {first:?}"),
        }

        // Adding an agent bumps the `agents/` dir mtime → the next call
        // must re-scan rather than serve the cached catalog.
        create_test_agent_flat(&agents_dir, "reviewer", "Reviews code");

        let second = handler.handle(agents_hook_ctx(Some(&ws))).await;
        match &second {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(text.contains("math"), "got: {text}");
                assert!(text.contains("reviewer"), "got: {text}");
            }
            _ => panic!("Expected Continue with Text, got {second:?}"),
        }
    }
}
