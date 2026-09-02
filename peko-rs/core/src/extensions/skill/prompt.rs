//! Workspace-scanning prompt handler for the `skills` system-prompt
//! section (Part B: dynamic per-turn workspace catalog).
//!
//! Registered **once** per core (see `principal/context.rs`). At invoke
//! time the handler resolves the workspace from the hook context's
//! `ToolRuntimeContext`, scans `<workspace>/skills/<name>/SKILL.md`,
//! parses each skill's frontmatter, and renders one catalog line per
//! skill: `- {name}: {description} (skills/{name}/SKILL.md)`.
//!
//! Presence in the workspace = visible (ADR-047): there is deliberately
//! **no** capability or active-extension filter. The skill `name` is the
//! directory name — the same key `WorkspaceSkillRuntime::resolve_skill`
//! uses — so every rendered line is invocable via the `Skill` tool.
//!
//! The scan result is cached in a `Mutex` keyed on the `skills/`
//! directory's mtime — each call `stat`s the directory (cheap) and only
//! re-reads the SKILL.md files when the mtime changed. That keeps the
//! handler well within the renderer's 2-second hook timeout.

use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use tracing::warn;

use crate::extensions::framework::core::{HookContext, HookHandler, HookPoint};
use crate::extensions::framework::types::{HookOutput, HookResult, ToolRuntimeContext};
use crate::tools::builtin::skill::{parse_yaml_frontmatter_typed, SkillFrontmatter};

/// Default priority for the skills-catalog prompt section.
pub const SKILL_CATALOG_HOOK_PRIORITY: i32 = 90;

/// Hard cap on the rendered skills catalog. Keeps a pathological
/// workspace (hundreds of skills) from blowing up the system prompt;
/// on overflow whole lines are truncated from the end and a pointer to
/// the on-disk directory is appended instead.
const SKILLS_CATALOG_MAX_BYTES: usize = 8 * 1024;

/// Workspace-scanning handler for the `skills` prompt section.
///
/// See the module doc for the scanning/caching contract. A missing
/// workspace, missing `skills/` dir, or empty catalog yields
/// [`HookResult::PassThrough`] so the section is stripped from the
/// prompt.
#[derive(Debug, Default)]
pub struct WorkspaceSkillsPromptHandler {
    cache: Mutex<Option<((SystemTime, usize), String)>>,
}

impl WorkspaceSkillsPromptHandler {
    /// Create a new workspace-scanning skills handler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the skills catalog for `workspace`, using the mtime-keyed
    /// cache. Returns `None` when there is nothing to render.
    ///
    /// The cache key is `(dir_mtime, immediate_child_count)`. Mtime alone
    /// is unreliable on Windows NTFS for fast back-to-back subdir
    /// creations (the parent dir's mtime can read back the same value
    /// before the metadata update has been flushed); counting children
    /// catches added/removed subdirs even when mtime is stale.
    fn render_catalog(&self, workspace: &str) -> Option<String> {
        let skills_dir = Path::new(workspace).join("skills");
        let metadata = std::fs::metadata(&skills_dir).ok()?;
        let mtime = metadata.modified().ok()?;
        let child_count = std::fs::read_dir(&skills_dir).ok()?.count();
        let key = (mtime, child_count);

        {
            let cache = self.cache.lock().expect("skills catalog cache poisoned");
            if let Some((cached_key, text)) = &*cache {
                if *cached_key == key {
                    return (!text.is_empty()).then(|| text.clone());
                }
            }
        }

        let text = scan_skills_dir(&skills_dir, workspace);

        let mut cache = self.cache.lock().expect("skills catalog cache poisoned");
        *cache = Some((key, text.clone()));

        (!text.is_empty()).then_some(text)
    }
}

#[async_trait]
impl HookHandler for WorkspaceSkillsPromptHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        let workspace = ctx
            .get_state::<ToolRuntimeContext>("tool_context")
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
            section: "skills".to_string(),
            priority: SKILL_CATALOG_HOOK_PRIORITY,
        }
    }

    fn priority(&self) -> i32 {
        SKILL_CATALOG_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        "WorkspaceSkillsPromptHandler".to_string()
    }
}

/// Scan `<workspace>/skills/<name>/SKILL.md` and render the catalog,
/// capped at [`SKILLS_CATALOG_MAX_BYTES`]. Skills whose frontmatter
/// fails to parse are skipped with a warning.
fn scan_skills_dir(skills_dir: &Path, workspace: &str) -> String {
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "Failed to read skills directory {}: {e}",
                skills_dir.display()
            );
            return String::new();
        }
    };

    let mut lines = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let skill_md = dir.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let content = match std::fs::read_to_string(&skill_md) {
            Ok(content) => content,
            Err(e) => {
                warn!("Failed to read {}: {e}; skipping skill", skill_md.display());
                continue;
            }
        };
        match parse_yaml_frontmatter_typed::<SkillFrontmatter>(&content) {
            Ok((fm, _body)) => {
                lines.push(format!(
                    "- {name}: {} (skills/{name}/SKILL.md)",
                    fm.description
                ));
            }
            Err(e) => {
                warn!(
                    "Failed to parse frontmatter in {}: {e}; skipping skill",
                    skill_md.display()
                );
            }
        }
    }
    // Deterministic ordering — directory iteration order is
    // platform-dependent.
    lines.sort();

    let notice = format!("(more skills in {workspace}/skills/ — list the directory to see all)");
    let mut out = String::new();
    let mut iter = lines.iter().peekable();
    while let Some(line) = iter.next() {
        // Reserve room for the truncation notice whenever more lines
        // remain, so the total stays under the cap even on overflow.
        let reserve = if iter.peek().is_some() {
            notice.len() + 1
        } else {
            0
        };
        if !out.is_empty() && out.len() + line.len() + 1 + reserve > SKILLS_CATALOG_MAX_BYTES {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    if iter.peek().is_some() {
        out.push('\n');
        out.push_str(&notice);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::core::ExtensionServices;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_skill(skills_dir: &Path, name: &str, description: &str) {
        let skill_dir = skills_dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )
        .unwrap();
    }

    /// Build a `PromptSystemSection { section: "skills" }` hook context,
    /// optionally carrying a workspace in the `tool_context` state.
    fn skills_hook_ctx(workspace: Option<&str>) -> HookContext {
        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "skills".to_string(),
                priority: SKILL_CATALOG_HOOK_PRIORITY,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        if let Some(ws) = workspace {
            ctx.set_state(
                "tool_context",
                ToolRuntimeContext::new()
                    .with_workspace(ws)
                    .with_principal_id("test-principal"),
            );
        }
        ctx
    }

    fn handle_text(result: HookResult) -> Option<String> {
        match result {
            HookResult::Continue(HookOutput::Text(text)) => Some(text),
            _ => None,
        }
    }

    #[tokio::test]
    async fn workspace_skills_handler_renders_catalog() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();
        make_skill(&skills_dir, "docker", "Docker ops");
        make_skill(&skills_dir, "git", "Git workflow");
        // Unparseable frontmatter → skipped (does not break the catalog).
        let bad_dir = skills_dir.join("broken");
        std::fs::create_dir(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("SKILL.md"), "no frontmatter here").unwrap();

        let handler = WorkspaceSkillsPromptHandler::new();
        let text = handle_text(
            handler
                .handle(skills_hook_ctx(Some(&temp.path().to_string_lossy())))
                .await,
        )
        .expect("expected catalog text");

        assert!(
            text.contains("- docker: Docker ops (skills/docker/SKILL.md)"),
            "got: {text}"
        );
        assert!(
            text.contains("- git: Git workflow (skills/git/SKILL.md)"),
            "got: {text}"
        );
        assert!(!text.contains("broken"), "got: {text}");
    }

    #[tokio::test]
    async fn workspace_skills_handler_passes_through_without_workspace() {
        let handler = WorkspaceSkillsPromptHandler::new();
        let result = handler.handle(skills_hook_ctx(None)).await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough without workspace, got {result:?}"
        );
    }

    #[tokio::test]
    async fn workspace_skills_handler_passes_through_on_empty_dir() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("skills")).unwrap();

        let handler = WorkspaceSkillsPromptHandler::new();
        let result = handler
            .handle(skills_hook_ctx(Some(&temp.path().to_string_lossy())))
            .await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough for empty skills dir, got {result:?}"
        );
    }

    #[tokio::test]
    async fn workspace_skills_handler_rescans_on_dir_mtime_change() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();
        make_skill(&skills_dir, "docker", "Docker ops");

        let handler = WorkspaceSkillsPromptHandler::new();
        let ws = temp.path().to_string_lossy().to_string();

        // First call scans and caches.
        let first = handle_text(handler.handle(skills_hook_ctx(Some(&ws))).await)
            .expect("expected catalog text");
        assert!(first.contains("docker"), "got: {first}");
        assert!(!first.contains("git"), "got: {first}");

        // Adding a skill bumps the `skills/` dir mtime → the next call
        // must re-scan rather than serve the cached catalog.
        make_skill(&skills_dir, "git", "Git workflow");

        let second = handle_text(handler.handle(skills_hook_ctx(Some(&ws))).await)
            .expect("expected catalog text");
        assert!(second.contains("docker"), "got: {second}");
        assert!(second.contains("git"), "got: {second}");
    }

    #[test]
    fn scan_truncates_catalog_at_byte_cap() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();
        // ~200 skills × ~90 bytes each ≈ 18 KB of catalog — well over
        // the 8 KB cap.
        for i in 0..200 {
            make_skill(
                &skills_dir,
                &format!("skill-{i:03}"),
                "A description long enough to fill the catalog quickly",
            );
        }

        let ws = temp.path().to_string_lossy().to_string();
        let out = scan_skills_dir(&skills_dir, &ws);

        assert!(out.len() <= SKILLS_CATALOG_MAX_BYTES, "len: {}", out.len());
        assert!(
            out.contains("(more skills in "),
            "expected truncation notice, got: {out}"
        );
        assert!(out.contains("list the directory to see all"), "got: {out}");
        // Whole-line truncation: the first (sorted) skill survived.
        assert!(out.contains("- skill-000:"), "got: {out}");
    }

    #[test]
    fn scan_under_cap_has_no_truncation_notice() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();
        make_skill(&skills_dir, "docker", "Docker ops");

        let ws = temp.path().to_string_lossy().to_string();
        let out = scan_skills_dir(&skills_dir, &ws);

        assert_eq!(out, "- docker: Docker ops (skills/docker/SKILL.md)");
    }
}
