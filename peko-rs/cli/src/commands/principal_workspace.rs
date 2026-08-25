//! Per-principal workspace plugin manager (ADR-047 §5 Phase 5).
//!
//! Workspace-resident tools / hooks / skills / MCP servers live under
//! `<workspace>/{tools,hooks,skills,mcp}/<id>/`. The commands in this
//! module — `peko principal {tool,hook,skill,mcp} list/install/remove` —
//! operate directly on those directories:
// - `list` walks the per-kind directory and prints one row per entry.
// - `install <path>` copies the source directory into `<workspace>/<kind>/<id>`.
// - `remove <id>` deletes `<workspace>/<kind>/<id>`.
//!
//! The runtime reads the same directories on principal boot (see
//! `peko-rs/core/src/extensions/{universal,workspace_hooks,skill,mcp}/workspace.rs`).
//! There is no global registry of "installed" plugins — what is on disk
//! is the truth. Removal is "rm -rf", install is "cp -r". ADR-046's
//! audit canary catches drift between successive daemon boots.
//!
//! The MCP `add/auth/remove` flavor predates Phase 5 (it was a Claude
//! Code-style workflow for managing remote SSE servers with OAuth).
//! After Phase 5, MCP servers follow the same `list/install/remove`
//! pattern as tools/hooks/skills; the OAuth credential flow lives in
//! the vault (see `peko vault`).

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The four kinds of workspace-resident plugin that the per-principal
/// subcommands can manage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Tool,
    Hook,
    Skill,
    Mcp,
}

impl PluginKind {
    /// Workspace subdirectory name.
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Tool => "tools",
            Self::Hook => "hooks",
            Self::Skill => "skills",
            Self::Mcp => "mcp",
        }
    }

    /// Singular human label used in CLI output.
    #[must_use]
    pub fn singular_label(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Hook => "hook",
            Self::Skill => "skill",
            Self::Mcp => "mcp server",
        }
    }
}

/// One row in a `list` output.
#[derive(Debug, Clone, Serialize)]
pub struct PluginEntry {
    pub id: String,
    pub kind: PluginKind,
    pub path: PathBuf,
    /// Whether the entry directory contains a recognized manifest
    /// (`tool.toml` / `hook.toml` / `SKILL.md` / `server.json`).
    pub has_manifest: bool,
}

/// Resolve `<workspace>/<kind>` for `kind`. Caller passes the workspace
/// root.
#[must_use]
pub fn kind_dir(workspace: &Path, kind: PluginKind) -> PathBuf {
    workspace.join(kind.dir_name())
}

/// List every plugin of `kind` currently installed in `workspace`.
/// Missing directories yield an empty list, not an error.
#[must_use]
pub fn list_kind(workspace: &Path, kind: PluginKind) -> Vec<PluginEntry> {
    let dir = kind_dir(workspace, kind);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        out.push(PluginEntry {
            id: id.clone(),
            kind,
            path: path.clone(),
            has_manifest: manifest_for(kind, &path).exists(),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Locate the canonical manifest filename for `kind` inside `dir`.
#[must_use]
pub fn manifest_for(kind: PluginKind, dir: &Path) -> PathBuf {
    match kind {
        PluginKind::Tool => dir.join("manifest.yaml"),
        PluginKind::Hook => dir.join("hook.toml"),
        PluginKind::Skill => dir.join("SKILL.md"),
        PluginKind::Mcp => dir.join("server.json"),
    }
}

/// Copy `source` into `<workspace>/<kind>/<id>`. The destination id is
/// the basename of `source` (last path component).
///
/// `source` may be a directory (most common) or a single manifest file
/// (less common — only the MCP `server.json` form). For directory
/// copies, the function refuses to copy into a pre-existing destination
/// unless `force` is true.
pub async fn install_kind(
    workspace: &Path,
    kind: PluginKind,
    source: &Path,
    force: bool,
) -> Result<PluginEntry> {
    if !source.exists() {
        bail!(
            "source path does not exist: {}",
            source.display()
        );
    }

    let id = source
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("source path has no usable basename: {}", source.display()))?
        .to_string();

    let dest = kind_dir(workspace, kind).join(&id);
    if dest.exists() {
        if !force {
            bail!(
                "{} `{}` is already installed at {}; pass --force to overwrite",
                kind.singular_label(),
                id,
                dest.display()
            );
        }
        std::fs::remove_dir_all(&dest).with_context(|| {
            format!(
                "failed to remove existing {} `{}` at {}",
                kind.singular_label(),
                id,
                dest.display()
            )
        })?;
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create {} dir {}",
                kind.singular_label(),
                parent.display()
            )
        })?;
    }

    copy_recursive(source, &dest).with_context(|| {
        format!(
            "failed to copy {} from {} to {}",
            kind.singular_label(),
            source.display(),
            dest.display()
        )
    })?;

    Ok(PluginEntry {
        id: id.clone(),
        kind,
        path: dest.clone(),
        has_manifest: manifest_for(kind, &dest).exists(),
    })
}

/// Remove `<workspace>/<kind>/<id>`. Missing entries are an error
/// unless `force` is true (then no-op success).
pub fn remove_kind(
    workspace: &Path,
    kind: PluginKind,
    id: &str,
    force: bool,
) -> Result<()> {
    let dest = kind_dir(workspace, kind).join(id);
    if !dest.exists() {
        if force {
            return Ok(());
        }
        bail!(
            "{} `{}` not installed (no {} dir at {})",
            kind.singular_label(),
            id,
            kind.dir_name(),
            dest.display()
        );
    }
    std::fs::remove_dir_all(&dest).with_context(|| {
        format!(
            "failed to remove {} `{}` at {}",
            kind.singular_label(),
            id,
            dest.display()
        )
    })?;
    Ok(())
}

fn copy_recursive(src: &Path, dest: &Path) -> Result<()> {
    let src_meta = std::fs::metadata(src)
        .with_context(|| format!("stat source: {}", src.display()))?;
    if src_meta.is_dir() {
        std::fs::create_dir_all(dest)
            .with_context(|| format!("create dir: {}", dest.display()))?;
        for entry in std::fs::read_dir(src)
            .with_context(|| format!("read_dir: {}", src.display()))?
        {
            let entry = entry?;
            let entry_src = entry.path();
            let entry_name = entry.file_name();
            let entry_dest = dest.join(&entry_name);
            // Skip the hidden-noise files that ship with every
            // workspace plugin (`.DS_Store`, `.pekoignore`).
            if let Some(name) = entry_name.to_str() {
                if name == ".DS_Store" || name == ".pekoignore" {
                    continue;
                }
            }
            if entry_dest.exists() {
                std::fs::remove_dir_all(&entry_dest).ok();
                std::fs::remove_file(&entry_dest).ok();
            }
            copy_recursive(&entry_src, &entry_dest)?;
        }
    } else {
        std::fs::copy(src, dest)
            .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn list_missing_kind_dir_is_empty_not_error() {
        let ws = workspace();
        assert!(list_kind(ws.path(), PluginKind::Tool).is_empty());
        assert!(list_kind(ws.path(), PluginKind::Hook).is_empty());
        assert!(list_kind(ws.path(), PluginKind::Skill).is_empty());
        assert!(list_kind(ws.path(), PluginKind::Mcp).is_empty());
    }

    #[test]
    fn list_picks_up_directories_only() {
        let ws = workspace();
        std::fs::create_dir_all(ws.path().join("hooks/greet")).unwrap();
        std::fs::write(ws.path().join("hooks/loose-file"), "x").unwrap();
        let hooks = list_kind(ws.path(), PluginKind::Hook);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].id, "greet");
        assert!(!hooks[0].has_manifest);
    }

    #[test]
    fn list_detects_manifest_presence() {
        let ws = workspace();
        let tool_dir = ws.path().join("tools/foo");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(tool_dir.join("manifest.yaml"), "name: foo\n").unwrap();
        let tools = list_kind(ws.path(), PluginKind::Tool);
        assert_eq!(tools.len(), 1);
        assert!(tools[0].has_manifest);
    }

    #[test]
    fn list_sorts_alphabetically() {
        let ws = workspace();
        for id in ["zebra", "alpha", "mango"] {
            std::fs::create_dir_all(ws.path().join("skills").join(id)).unwrap();
        }
        let skills = list_kind(ws.path(), PluginKind::Skill);
        let ids: Vec<&str> = skills.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "mango", "zebra"]);
    }

    #[tokio::test]
    async fn install_copies_directory_into_workspace() {
        let ws = workspace();
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("server.json"), "{}").unwrap();
        std::fs::create_dir(src.path().join("nested")).unwrap();
        std::fs::write(src.path().join("nested").join("cap.json"), "[]").unwrap();

        let entry = install_kind(
            ws.path(),
            PluginKind::Mcp,
            src.path(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(entry.id, src.path().file_name().unwrap().to_str().unwrap());
        assert!(entry.path.join("server.json").exists());
        assert!(entry.path.join("nested").join("cap.json").exists());
        assert!(entry.has_manifest);
    }

    #[tokio::test]
    async fn install_refuses_existing_without_force_flag() {
        let ws = workspace();
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("manifest.yaml"), "x").unwrap();

        install_kind(ws.path(), PluginKind::Tool, src.path(), false)
            .await
            .unwrap();

        let err = install_kind(ws.path(), PluginKind::Tool, src.path(), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already installed"));
    }

    #[tokio::test]
    async fn install_with_force_overwrites() {
        let ws = workspace();
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("manifest.yaml"), "v1").unwrap();
        install_kind(ws.path(), PluginKind::Tool, src.path(), false)
            .await
            .unwrap();
        std::fs::write(src.path().join("manifest.yaml"), "v2").unwrap();
        let entry = install_kind(ws.path(), PluginKind::Tool, src.path(), true)
            .await
            .unwrap();
        let body = std::fs::read_to_string(entry.path.join("manifest.yaml")).unwrap();
        assert_eq!(body, "v2");
    }

    #[tokio::test]
    async fn install_rejects_missing_source() {
        let ws = workspace();
        let err = install_kind(
            ws.path(),
            PluginKind::Tool,
            Path::new("/definitely/not/here"),
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn remove_kind_succeeds_then_idempotent_with_force() {
        let ws = workspace();
        let tool_dir = ws.path().join("tools/lonely");
        std::fs::create_dir_all(&tool_dir).unwrap();
        remove_kind(ws.path(), PluginKind::Tool, "lonely", false).unwrap();
        assert!(!tool_dir.exists());
        remove_kind(ws.path(), PluginKind::Tool, "lonely", true).unwrap();
    }

    #[test]
    fn remove_kind_missing_is_error_without_force() {
        let ws = workspace();
        let err = remove_kind(ws.path(), PluginKind::Tool, "absent", false)
            .unwrap_err();
        assert!(err.to_string().contains("not installed"));
    }
}