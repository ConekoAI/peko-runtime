//! Workspace-scanning prompt handler for the `skills` prompt section
//! (Part B: dynamic per-turn workspace catalog). The section rides the
//! tail `<runtime-context>` message, not the frozen system prompt.
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
//! The scan result is cached in a `Mutex` keyed on a per-file
//! `(mtime, len)` fingerprint of every `<name>/SKILL.md` — each call
//! `stat`s the scanned files (a handful; cheap) and only re-reads them
//! when the fingerprint changed. Unlike the previous dir-mtime key,
//! in-place edits to an existing SKILL.md invalidate the catalog on
//! the next iteration (ADR-052 D2). That keeps the handler well within
//! the renderer's 2-second hook timeout.

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

/// Cache key: per-file `(relative_path, mtime, len)` stats for every
/// scanned file in the catalog dir, sorted by path for determinism.
/// File-level stats (not dir mtime) so in-place content edits
/// invalidate the cache (ADR-052 D2).
type DirFingerprint = Vec<(String, SystemTime, u64)>;

/// Workspace-scanning handler for the `skills` prompt section.
///
/// See the module doc for the scanning/caching contract. A missing
/// workspace, missing `skills/` dir, or empty catalog yields
/// [`HookResult::PassThrough`] so the section is stripped from the
/// prompt.
#[derive(Debug, Default)]
pub struct WorkspaceSkillsPromptHandler {
    cache: Mutex<Option<(DirFingerprint, String)>>,
}

impl WorkspaceSkillsPromptHandler {
    /// Create a new workspace-scanning skills handler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the skills catalog for `workspace`, using the
    /// fingerprint-keyed cache. Returns `None` when there is nothing
    /// to render.
    ///
    /// The cache key folds each scanned `<name>/SKILL.md` file's
    /// `(mtime, len)` in, so in-place content edits invalidate the
    /// catalog on the next call — the previous `(dir_mtime,
    /// child_count)` key only caught added/removed entries (dir mtime
    /// doesn't move on content writes).
    fn render_catalog(&self, workspace: &str) -> Option<String> {
        let skills_dir = Path::new(workspace).join("skills");
        let key = skills_dir_fingerprint(&skills_dir)?;

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

/// Fingerprint every `<name>/SKILL.md` under `skills_dir` as
/// `(relative_path, mtime, len)` entries, sorted by path. `None` when
/// the dir doesn't exist. Files whose metadata can't be read are
/// skipped, mirroring the scanner's skip-unreadable behavior.
fn skills_dir_fingerprint(skills_dir: &Path) -> Option<DirFingerprint> {
    let mut stats = Vec::new();
    for entry in std::fs::read_dir(skills_dir).ok()?.flatten() {
        let skill_md = entry.path().join("SKILL.md");
        let Ok(meta) = std::fs::metadata(&skill_md) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        let rel = skill_md
            .strip_prefix(skills_dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        stats.push((rel, mtime, meta.len()));
    }
    stats.sort();
    Some(stats)
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

    /// ADR-052 D2: an in-place CONTENT edit to an existing SKILL.md
    /// (same filename, no dir entry added/removed, so the `skills/`
    /// dir mtime is untouched) must invalidate the catalog on the next
    /// call. The previous `(dir_mtime, child_count)` key served the
    /// stale catalog here.
    #[tokio::test]
    async fn workspace_skills_handler_rescans_on_in_place_content_edit() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();
        make_skill(&skills_dir, "docker", "Docker ops");

        let handler = WorkspaceSkillsPromptHandler::new();
        let ws = temp.path().to_string_lossy().to_string();

        let first = handle_text(handler.handle(skills_hook_ctx(Some(&ws))).await)
            .expect("expected catalog text");
        assert!(first.contains("Docker ops"), "got: {first}");

        // Rewrite the same file with a longer description — len
        // changes; dir mtime does not.
        make_skill(&skills_dir, "docker", "Docker ops, compose, and swarm");

        let second = handle_text(handler.handle(skills_hook_ctx(Some(&ws))).await)
            .expect("expected updated catalog text");
        assert!(
            second.contains("Docker ops, compose, and swarm"),
            "got: {second}"
        );
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
