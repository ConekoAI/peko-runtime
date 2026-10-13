//! Workspace-backed `SkillRuntime` (ADR-047 §2.4).
//!
//! Replaces the old `SkillCatalogRuntime` (which wrapped the global
//! `SkillCatalog` populated by the extension framework) with a
//! runtime that reads directly from the principal's workspace at
//! `<workspace>/skills/<name>/SKILL.md`. No registry, no adapter,
//! no catalog mutation — every `resolve_skill(name)` is a direct
//! filesystem check against the principal's own skills directory.
//!
//! ADR-047 §2.4 deletes the extension framework as a registration
//! discipline. Skills are files inside the principal's workspace;
//! the runtime's job is to point the `SkillTool` at them. The
//! principal is responsible for installation.
//!
//! **v1 limitation:** the runtime does not enumerate skills
//! eagerly — every `list_skills()` call walks the directory. That's
//! the correct trade-off for a workspace-resident registry: the
//! directory is the source of truth, and caching adds a layer that
//! has to be invalidated on every install / uninstall. Add a
//! `notify`-backed cache in a follow-up if list-time disk I/O
//! becomes hot.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::tools::builtin::skill::{SkillEntry, SkillRuntime};

/// Skill definitions must be ordinary workspace files. Linked roots or
/// definitions make discovery depend on another workspace and cannot be
/// transported faithfully in a principal snapshot.
pub(crate) fn is_skill_directory(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Per-principal skill lookup backed by `<workspace>/skills/`.
pub struct WorkspaceSkillRuntime {
    skills_dir: PathBuf,
}

impl WorkspaceSkillRuntime {
    /// Build a runtime that resolves skills from `skills_dir`.
    ///
    /// Production callers pass `<principal_shared>/skills/`. The
    /// runtime does not require the directory to exist; a missing
    /// directory resolves to no skills.
    #[must_use]
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }

    /// The directory the runtime reads from.
    #[must_use]
    pub fn skills_dir(&self) -> &std::path::Path {
        &self.skills_dir
    }

    fn has_ordinary_root(&self) -> bool {
        is_skill_directory(&self.skills_dir)
            && self
                .skills_dir
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .is_none_or(is_skill_directory)
    }
}

#[async_trait]
impl SkillRuntime for WorkspaceSkillRuntime {
    fn resolve_skill(&self, name: &str) -> Option<SkillEntry> {
        // `name` is joined onto `skills_dir` directly, so it must be a
        // single plain path component — anything else (`..`, `a/b`,
        // `a\b`, absolute paths, empty) would resolve outside the
        // skills directory. Invalid names behave like a nonexistent
        // skill (an unknown-skill error at the tool layer), not a new error
        // surface.
        let mut components = std::path::Path::new(name).components();
        if !matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        ) || name.contains(['/', '\\'])
        {
            return None;
        }
        let skill_dir = self.skills_dir.join(name);
        let skill_md = skill_dir.join("SKILL.md");
        if !self.has_ordinary_root()
            || !is_skill_directory(&skill_dir)
            || !std::fs::symlink_metadata(&skill_md).is_ok_and(|meta| meta.is_file())
        {
            return None;
        }
        Some(SkillEntry {
            name: name.to_string(),
            path: skill_md,
        })
    }

    fn list_skills(&self) -> Vec<String> {
        if !self.has_ordinary_root() {
            return Vec::new();
        }
        let read = match std::fs::read_dir(&self.skills_dir) {
            Ok(it) => it,
            Err(_) => return Vec::new(),
        };
        let mut names: Vec<String> = read
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                self.resolve_skill(&name).map(|_| name)
            })
            .collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn linked_roots_directories_and_definitions_are_not_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        make_skill(outside.path(), "external", "Other principal");
        let external = outside.path().join("external");
        std::os::unix::fs::symlink(&external, tmp.path().join("linked")).unwrap();
        std::fs::create_dir(tmp.path().join("definition")).unwrap();
        std::os::unix::fs::symlink(
            external.join("SKILL.md"),
            tmp.path().join("definition/SKILL.md"),
        )
        .unwrap();
        let runtime = WorkspaceSkillRuntime::new(tmp.path().to_path_buf());
        assert!(runtime.list_skills().is_empty());
        assert!(runtime.resolve_skill("linked").is_none());
        assert!(runtime.resolve_skill("definition").is_none());
        let linked_root = tmp.path().join("skills");
        std::os::unix::fs::symlink(outside.path(), &linked_root).unwrap();
        let runtime = WorkspaceSkillRuntime::new(linked_root);
        assert!(runtime.list_skills().is_empty());
        assert!(runtime.resolve_skill("external").is_none());
    }

    fn make_skill(dir: &std::path::Path, id: &str, body: &str) {
        let skill_dir = dir.join(id);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {id}\ndescription: {body}\n---\n\n# {id}\n\nbody\n"),
        )
        .unwrap();
    }

    #[test]
    fn resolve_existing_skill() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "docker", "Docker ops");
        let rt = WorkspaceSkillRuntime::new(tmp.path().to_path_buf());
        let entry = rt.resolve_skill("docker").expect("docker should resolve");
        assert_eq!(entry.name, "docker");
        assert!(entry.path.ends_with("docker/SKILL.md"));
    }

    #[test]
    fn resolve_missing_skill_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = WorkspaceSkillRuntime::new(tmp.path().to_path_buf());
        assert!(rt.resolve_skill("nope").is_none());
    }

    #[test]
    fn resolve_non_directory_entry_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        // Stray file at the top level — must not resolve as a skill.
        std::fs::write(tmp.path().join("stray"), b"x").unwrap();
        let rt = WorkspaceSkillRuntime::new(tmp.path().to_path_buf());
        assert!(rt.resolve_skill("stray").is_none());
    }

    #[test]
    fn list_skills_returns_sorted_skills_only() {
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "docker", "Docker ops");
        make_skill(tmp.path(), "git", "Git workflow");
        // Stray directory without SKILL.md — excluded.
        std::fs::create_dir_all(tmp.path().join("incomplete")).unwrap();
        // Stray file — excluded.
        std::fs::write(tmp.path().join("readme.md"), b"x").unwrap();

        let rt = WorkspaceSkillRuntime::new(tmp.path().to_path_buf());
        assert_eq!(rt.list_skills(), vec!["docker", "git"]);
    }

    #[test]
    fn missing_skills_dir_returns_empty() {
        let rt = WorkspaceSkillRuntime::new(std::path::PathBuf::from("/no/such/path/here"));
        assert!(rt.list_skills().is_empty());
        assert!(rt.resolve_skill("anything").is_none());
    }

    #[test]
    fn resolve_rejects_path_traversal_names() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        // A real SKILL.md OUTSIDE the skills dir — the traversal target.
        make_skill(tmp.path(), "secret", "should never resolve");
        // A legit skill inside the skills dir.
        make_skill(&skills_dir, "docker", "Docker ops");

        let rt = WorkspaceSkillRuntime::new(skills_dir);

        // The traversal target exists on disk; it must still not resolve.
        assert!(rt.resolve_skill("../secret").is_none());
        // Other multi-component / non-plain-component shapes.
        for bad in [
            "..",
            ".",
            "",
            "nested/skill",
            "nested\\skill",
            "/etc",
            "./docker",
            "docker/",
            "docker/../secret",
        ] {
            assert!(rt.resolve_skill(bad).is_none(), "should reject {bad:?}");
        }

        // Normal single-component names still resolve.
        assert!(rt.resolve_skill("docker").is_some());
    }

    #[test]
    fn list_skills_never_produces_rejected_names() {
        // Every name `list_skills` can return is a real directory entry,
        // hence a single plain component — so it always round-trips
        // through `resolve_skill`'s traversal guard.
        let tmp = tempfile::tempdir().unwrap();
        make_skill(tmp.path(), "docker", "Docker ops");
        let rt = WorkspaceSkillRuntime::new(tmp.path().to_path_buf());
        for name in rt.list_skills() {
            assert!(rt.resolve_skill(&name).is_some(), "{name} should resolve");
        }
    }
}
