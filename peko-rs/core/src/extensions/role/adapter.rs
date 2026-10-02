//! Role adapter for the Extension system
//!
//! Discovers role files in the principal's workspace
//! (`<workspace>/roles/<id>.md` or `<workspace>/roles/<id>/ROLE.md`)
//! and renders them into the `roles` prompt section via the
//! workspace-scanning [`WorkspaceRolesPromptHandler`]. The engine
//! prompt renderer dispatches that hook on every iteration for the
//! tail `<runtime-context>` message (see
//! `peko-rs/engine/src/prompt/renderer.rs`), so roles added to the
//! workspace appear in the conversation on the next iteration.
//!
//! Terminology (ADR-064): a role is the TEMPLATE a live agent is
//! initiated from. "Agent" stays the word for the live actor of a
//! session; this module only ever handles roles.
//!
//! PR-C.4: `ExtensionTypeAdapter` trait impl + `AgentPromptHandlerFactory`
//! deleted. The trait impl was the framework-coupling path; both it
//! and the factory that wrapped `AgentPromptHandler` had zero callers
//! once `BuiltInAdapters` was gutted (PR-C.1).
//!
//! Part B (dynamic per-turn workspace catalog): the static per-role
//! `AgentPromptHandler` + `register_agents_with_core` registration was
//! replaced by the single scanning `WorkspaceRolesPromptHandler`,
//! which resolves the workspace from the hook context at invoke time
//! and re-scans `roles/` whenever a scanned file's `(mtime, len)`
//! changes (ADR-052 D2 — in-place edits included).
//! Presence in the workspace = visible (ADR-047) — no capability or
//! active-extension filter. The remaining surface is
//! [`RoleAdapter::discover_roles`] (also called from
//! `principal/manager.rs`) + the data types it produces.

/// Metadata parsed from a workspace role file.
#[derive(Debug, Clone)]
pub struct RoleMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
}
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use tracing::{debug, warn};

/// Role extension type identifier
pub const ROLE_EXTENSION_TYPE: &str = "role";

/// Default priority for role prompt injection
pub const ROLE_HOOK_PRIORITY: i32 = 90;

/// Role adapter for the Extension system
#[derive(Debug)]
pub struct RoleAdapter;

impl RoleAdapter {
    /// Create a new role adapter
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Discover roles from a directory.
    ///
    /// Supports two layouts:
    /// - Directory layout: `roles/<id>/ROLE.md`
    /// - Flat layout: `roles/<id>.md`
    ///
    /// The canonical role id is the directory name for directory layouts and
    /// the file stem for flat layouts. The frontmatter `name` is used only as
    /// the human-readable display name.
    pub fn discover_roles(&self, path: &Path) -> Vec<DiscoveredRole> {
        let mut roles = Vec::new();

        if !path.exists() {
            debug!("Roles directory does not exist: {:?}", path);
            return roles;
        }

        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read roles directory {:?}: {}", path, e);
                return roles;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                let role_md = path.join("ROLE.md");
                if role_md.exists() {
                    match self.parse_role_manifest(&role_md) {
                        Ok(manifest) => {
                            roles.push(DiscoveredRole {
                                manifest,
                                file_path: role_md,
                                base_dir: path,
                            });
                        }
                        Err(e) => {
                            warn!("Failed to parse role from {:?}: {}", role_md, e);
                        }
                    }
                }
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            {
                match self.parse_role_manifest(&path) {
                    Ok(manifest) => {
                        roles.push(DiscoveredRole {
                            manifest,
                            file_path: path.clone(),
                            base_dir: path
                                .parent()
                                .unwrap_or_else(|| Path::new("."))
                                .to_path_buf(),
                        });
                    }
                    Err(e) => {
                        warn!("Failed to parse role from {:?}: {}", path, e);
                    }
                }
            }
        }

        roles
    }

    /// Parse a ROLE.md file into role metadata.
    fn parse_role_manifest(&self, path: &Path) -> Result<RoleMetadata> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;

        let (meta, _body): (RoleFrontmatter, _) = parse_yaml_frontmatter_typed(&content)
            .with_context(|| format!("Failed to parse frontmatter in {path:?}"))?;

        if meta.name.is_empty() {
            anyhow::bail!("Role name cannot be empty");
        }
        if meta.description.is_empty() {
            anyhow::bail!("Role description cannot be empty");
        }

        let canonical_id = canonical_id_from_path(path);
        if canonical_id.is_empty() {
            anyhow::bail!("Role canonical id cannot be empty for {path:?}");
        }

        let manifest = RoleMetadata {
            id: canonical_id,
            name: meta.name,
            description: meta.description,
        };

        Ok(manifest)
    }
}

/// Derive the canonical role id from its on-disk path.
///
/// For the directory layout (`roles/<id>/ROLE.md`) the id is the directory
/// name. For the flat layout (`roles/<id>.md`) the id is the file stem.
fn canonical_id_from_path(path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();

    if file_name.eq_ignore_ascii_case("ROLE.md") {
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

/// Split a `---`-fenced YAML frontmatter block from its markdown body.
fn parse_yaml_frontmatter(content: &str) -> Result<(String, String)> {
    let mut lines = content.lines().peekable();
    match lines.next() {
        Some("---") => {}
        _ => anyhow::bail!("YAML frontmatter must start with ---"),
    }
    let mut frontmatter_lines = Vec::new();
    let mut found_end = false;
    for line in lines.by_ref() {
        if line == "---" {
            found_end = true;
            break;
        }
        frontmatter_lines.push(line);
    }
    if !found_end {
        anyhow::bail!("YAML frontmatter must end with ---");
    }
    let body = lines.collect::<Vec<_>>().join("\n");
    Ok((frontmatter_lines.join("\n"), body))
}

/// Parse the YAML frontmatter into `T`, returning `(metadata, body)`.
fn parse_yaml_frontmatter_typed<T: serde::de::DeserializeOwned>(
    content: &str,
) -> Result<(T, String)> {
    let (frontmatter, body) = parse_yaml_frontmatter(content)?;
    let metadata: T =
        serde_yaml::from_str(&frontmatter).context("Failed to parse YAML frontmatter")?;
    Ok((metadata, body))
}

impl Default for RoleAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// A discovered role before registration
#[derive(Debug, Clone)]
pub struct DiscoveredRole {
    /// Metadata parsed from the role file.
    pub manifest: RoleMetadata,
    /// Full path to ROLE.md
    pub file_path: PathBuf,
    /// Role base directory
    pub base_dir: PathBuf,
}

/// YAML frontmatter from ROLE.md
#[derive(Debug, Deserialize)]
struct RoleFrontmatter {
    name: String,
    description: String,
}

/// Workspace-scanning handler for the `roles` prompt section.
///
/// Registered **once** per core (see `principal/context.rs`), not per
/// role: at invoke time it reads the workspace from the hook context's
/// `ToolRuntimeContext`, scans `<workspace>/roles/` via
/// [`RoleAdapter::discover_roles`], and renders one line per role in
/// the format `- {name} (id: {id}): {description} (location: {path})`.
///
/// Presence in the workspace = visible (ADR-047): there is deliberately
/// **no** capability or active-extension filter. A missing workspace or
/// an empty `roles/` directory yields no section so
/// the section is stripped from the prompt.
///
/// The scan result is cached in a `Mutex` keyed on a per-file
/// `(mtime, len)` fingerprint of every scanned role file
/// (`<id>.md` / `<id>/ROLE.md`) — each call `stat`s the scanned files
/// (a handful; cheap) and only re-reads them when the fingerprint
/// changed. Unlike the previous dir-mtime key, in-place edits to an
/// existing role file invalidate the catalog on the next iteration
/// (ADR-052 D2). That keeps the handler well within the renderer's
/// 2-second hook timeout.
#[derive(Debug, Default)]
pub struct WorkspaceRolesPromptHandler {
    cache: Mutex<Option<(DirFingerprint, String)>>,
}

/// Cache key: per-file `(relative_path, mtime, len)` stats for every
/// scanned file in the catalog dir, sorted by path for determinism.
/// File-level stats (not dir mtime) so in-place content edits
/// invalidate the cache (ADR-052 D2).
type DirFingerprint = Vec<(String, SystemTime, u64)>;

impl WorkspaceRolesPromptHandler {
    /// Create a new workspace-scanning roles handler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the roles catalog for `workspace`, using the
    /// fingerprint-keyed cache. Returns `None` when there is nothing
    /// to render (no `roles/` dir, or no roles discovered).
    ///
    /// The cache key folds each scanned role file's `(mtime, len)`
    /// in, so in-place content edits invalidate the catalog on the
    /// next call — the previous `(dir_mtime, child_count)` key only
    /// caught added/removed entries (dir mtime doesn't move on content
    /// writes).
    fn render_catalog(&self, workspace: &str) -> Option<String> {
        let roles_dir = Path::new(workspace).join("roles");
        let key = roles_dir_fingerprint(&roles_dir)?;

        {
            let cache = self.cache.lock().expect("roles catalog cache poisoned");
            if let Some((cached_key, text)) = &*cache {
                if *cached_key == key {
                    return (!text.is_empty()).then(|| text.clone());
                }
            }
        }

        let roles = RoleAdapter::new().discover_roles(&roles_dir);
        let text = roles
            .iter()
            .map(|a| {
                // Normalize separators to `/` so the catalog renders
                // portably across platforms (Windows: `to_string_lossy()`
                // preserves `\`, which would break the substring
                // assertion in `workspace_roles_handler_renders_catalog`
                // and produce a less-readable location string for
                // users). Matches the format style used by the sibling
                // `WorkspaceSkillsPromptHandler` (skills/{name}/SKILL.md).
                let location = a.file_path.to_string_lossy().replace('\\', "/");
                format!(
                    "- {} (id: {}): {} (location: {})",
                    a.manifest.name, a.manifest.id, a.manifest.description, location
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut cache = self.cache.lock().expect("roles catalog cache poisoned");
        *cache = Some((key, text.clone()));

        (!text.is_empty()).then_some(text)
    }
}

/// Fingerprint every file [`RoleAdapter::discover_roles`] would scan
/// — flat `<id>.md` files and `<id>/ROLE.md` files — as
/// `(relative_path, mtime, len)` entries, sorted by path. `None` when
/// the dir doesn't exist. Files whose metadata can't be read are
/// skipped, mirroring the scanner's skip-unreadable behavior.
fn roles_dir_fingerprint(roles_dir: &Path) -> Option<DirFingerprint> {
    let mut stats = Vec::new();
    for entry in std::fs::read_dir(roles_dir).ok()?.flatten() {
        let path = entry.path();
        let file = if path.is_dir() {
            path.join("ROLE.md")
        } else {
            path
        };
        let is_markdown = file
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
        if !is_markdown {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&file) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        let rel = file
            .strip_prefix(roles_dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        stats.push((rel, mtime, meta.len()));
    }
    stats.sort();
    Some(stats)
}

#[async_trait]
impl crate::tools::prompt_sections::PromptSectionProvider for WorkspaceRolesPromptHandler {
    fn section(&self) -> &'static str {
        "roles"
    }

    fn priority(&self) -> i32 {
        ROLE_HOOK_PRIORITY
    }

    async fn render(
        &self,
        input: &crate::tools::prompt_sections::PromptSectionInput,
    ) -> Option<String> {
        let workspace = input.workspace.to_string_lossy().to_string();
        if workspace.is_empty() {
            return None;
        }
        self.render_catalog(&workspace)
    }
}

/// Helper to load roles from directory using the adapter
#[must_use]
pub fn load_roles_from_directory(path: &Path) -> Vec<DiscoveredRole> {
    let adapter = RoleAdapter::new();
    adapter.discover_roles(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::prompt_sections::PromptSectionProvider;

    use tempfile::TempDir;

    fn create_test_role(dir: &Path, name: &str, description: &str) -> PathBuf {
        let role_dir = dir.join(name);
        std::fs::create_dir(&role_dir).unwrap();

        let content = format!(
            r"---
name: {name}
description: {description}
color: '#ff0000'
---

# Test Role

This is a test role.
"
        );

        let role_md = role_dir.join("ROLE.md");
        std::fs::write(&role_md, content).unwrap();
        role_md
    }

    fn create_test_role_flat(dir: &Path, name: &str, description: &str) -> PathBuf {
        let content = format!(
            r"---
name: {name}
description: {description}
color: '#ff0000'
---

# Test Role

This is a test role.
"
        );

        let role_md = dir.join(format!("{name}.md"));
        std::fs::write(&role_md, content).unwrap();
        role_md
    }

    #[test]
    fn test_discover_roles() {
        let temp = TempDir::new().unwrap();

        create_test_role(temp.path(), "role1", "First role");
        create_test_role(temp.path(), "role2", "Second role");

        let adapter = RoleAdapter::new();
        let roles = adapter.discover_roles(temp.path());

        assert_eq!(roles.len(), 2);
        assert!(roles.iter().any(|a| a.manifest.id == "role1"));
        assert!(roles.iter().any(|a| a.manifest.id == "role2"));
    }

    #[test]
    fn test_discover_roles_flat_files() {
        let temp = TempDir::new().unwrap();

        create_test_role_flat(temp.path(), "role1", "First role");
        create_test_role_flat(temp.path(), "role2", "Second role");

        let adapter = RoleAdapter::new();
        let roles = adapter.discover_roles(temp.path());

        assert_eq!(roles.len(), 2);
        assert!(roles.iter().any(|a| a.manifest.id == "role1"));
        assert!(roles.iter().any(|a| a.manifest.id == "role2"));
        assert!(roles
            .iter()
            .any(|a| a.file_path == temp.path().join("role1.md")));
        assert!(roles
            .iter()
            .any(|a| a.file_path == temp.path().join("role2.md")));
    }

    #[test]
    fn test_discover_roles_mixed_layouts() {
        let temp = TempDir::new().unwrap();

        create_test_role(temp.path(), "dir-role", "Directory layout role");
        create_test_role_flat(temp.path(), "flat-role", "Flat layout role");

        let adapter = RoleAdapter::new();
        let roles = adapter.discover_roles(temp.path());

        assert_eq!(roles.len(), 2);
        assert!(roles.iter().any(|a| a.manifest.id == "dir-role"));
        assert!(roles.iter().any(|a| a.manifest.id == "flat-role"));
    }

    #[test]
    fn test_parse_role_manifest() {
        let temp = TempDir::new().unwrap();
        let role_md = create_test_role(temp.path(), "math", "Math operations");

        let adapter = RoleAdapter::new();
        let manifest = adapter.parse_role_manifest(&role_md).unwrap();

        assert_eq!(manifest.id, "math");
        assert_eq!(manifest.name, "math");
        assert_eq!(manifest.description, "Math operations");
    }

    #[test]
    fn test_parse_role_manifest_uses_canonical_id() {
        let temp = TempDir::new().unwrap();
        let role_dir = temp.path().join("senior-developer");
        std::fs::create_dir(&role_dir).unwrap();
        let role_md = role_dir.join("ROLE.md");
        std::fs::write(
            &role_md,
            r"---
name: Senior Developer
description: Premium implementation specialist
color: '#ff0000'
---

# Test Role
",
        )
        .unwrap();

        let adapter = RoleAdapter::new();
        let manifest = adapter.parse_role_manifest(&role_md).unwrap();

        assert_eq!(manifest.id, "senior-developer");
        assert_eq!(manifest.name, "Senior Developer");
        assert_eq!(manifest.description, "Premium implementation specialist");
    }

    /// Build the provider input for `workspace`.
    fn roles_input(workspace: &Path) -> crate::tools::prompt_sections::PromptSectionInput {
        crate::tools::prompt_sections::PromptSectionInput {
            principal_id: "test-principal".to_string(),
            workspace: workspace.to_path_buf(),
            session_id: String::new(),
            channel_port: None,
        }
    }

    #[tokio::test]
    async fn workspace_roles_handler_renders_catalog() {
        let temp = TempDir::new().unwrap();
        let roles_dir = temp.path().join("roles");
        std::fs::create_dir(&roles_dir).unwrap();
        create_test_role(&roles_dir, "math", "Math operations");
        create_test_role_flat(&roles_dir, "reviewer", "Reviews code");

        let handler = WorkspaceRolesPromptHandler::new();
        let text = handler
            .render(&roles_input(temp.path()))
            .await
            .expect("expected roles catalog text");
        assert!(
            text.contains("- math (id: math): Math operations"),
            "got: {text}"
        );
        assert!(
            text.contains("- reviewer (id: reviewer): Reviews code"),
            "got: {text}"
        );
        assert!(text.contains("(location: "), "got: {text}");
        assert!(text.contains("roles/math/ROLE.md"), "got: {text}");
        assert!(text.contains("roles/reviewer.md"), "got: {text}");
    }

    #[tokio::test]
    async fn workspace_roles_handler_passes_through_without_workspace() {
        let handler = WorkspaceRolesPromptHandler::new();
        let temp = TempDir::new().unwrap();
        // A workspace with no `roles/` dir → no section.
        let result = handler.render(&roles_input(temp.path())).await;
        assert!(result.is_none(), "Expected no section, got {result:?}");
    }

    #[tokio::test]
    async fn workspace_roles_handler_passes_through_on_empty_dir() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("roles")).unwrap();

        let handler = WorkspaceRolesPromptHandler::new();
        let result = handler.render(&roles_input(temp.path())).await;
        assert!(
            result.is_none(),
            "Expected no section for empty roles dir, got {result:?}"
        );
    }

    #[tokio::test]
    async fn workspace_roles_handler_rescans_on_dir_mtime_change() {
        let temp = TempDir::new().unwrap();
        let roles_dir = temp.path().join("roles");
        std::fs::create_dir(&roles_dir).unwrap();
        create_test_role(&roles_dir, "math", "Math operations");

        let handler = WorkspaceRolesPromptHandler::new();
        // First call scans and caches.
        let first = handler.render(&roles_input(temp.path())).await;
        match &first {
            Some(text) => {
                assert!(text.contains("math"), "got: {text}");
                assert!(!text.contains("reviewer"), "got: {text}");
            }
            None => panic!("Expected catalog text, got {first:?}"),
        }

        // Adding a role bumps the `roles/` dir mtime → the next call
        // must re-scan rather than serve the cached catalog.
        create_test_role_flat(&roles_dir, "reviewer", "Reviews code");

        let second = handler.render(&roles_input(temp.path())).await;
        match &second {
            Some(text) => {
                assert!(text.contains("math"), "got: {text}");
                assert!(text.contains("reviewer"), "got: {text}");
            }
            None => panic!("Expected catalog text, got {second:?}"),
        }
    }

    /// ADR-052 D2: an in-place CONTENT edit to an existing ROLE.md
    /// (same filename, no dir entry added/removed, so the `roles/`
    /// dir mtime is untouched) must invalidate the catalog on the next
    /// call. The previous `(dir_mtime, child_count)` key served the
    /// stale catalog here.
    #[tokio::test]
    async fn workspace_roles_handler_rescans_on_in_place_content_edit() {
        let temp = TempDir::new().unwrap();
        let roles_dir = temp.path().join("roles");
        std::fs::create_dir(&roles_dir).unwrap();
        let role_md = create_test_role(&roles_dir, "math", "Math operations");

        let handler = WorkspaceRolesPromptHandler::new();

        let first = handler.render(&roles_input(temp.path())).await;
        match &first {
            Some(text) => {
                assert!(text.contains("Math operations"), "got: {text}");
            }
            None => panic!("Expected catalog text, got {first:?}"),
        }

        // Rewrite the same file with a longer description — the file's
        // `(mtime, len)` changes; the dir mtime does not.
        let content = std::fs::read_to_string(&role_md)
            .unwrap()
            .replace("Math operations", "Math operations and symbolic algebra");
        std::fs::write(&role_md, content).unwrap();

        let second = handler.render(&roles_input(temp.path())).await;
        match &second {
            Some(text) => {
                assert!(
                    text.contains("Math operations and symbolic algebra"),
                    "got: {text}"
                );
            }
            None => panic!("Expected catalog text, got {second:?}"),
        }
    }
}
