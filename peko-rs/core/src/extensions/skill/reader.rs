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
}

#[async_trait]
impl SkillRuntime for WorkspaceSkillRuntime {
    fn resolve_skill(&self, name: &str) -> Option<SkillEntry> {
        let skill_md = self.skills_dir.join(name).join("SKILL.md");
        if !skill_md.is_file() {
            return None;
        }
        Some(SkillEntry {
            name: name.to_string(),
            path: skill_md,
            extension_id: None,
        })
    }

    fn list_skills(&self) -> Vec<String> {
        let read = match std::fs::read_dir(&self.skills_dir) {
            Ok(it) => it,
            Err(_) => return Vec::new(),
        };
        let mut names: Vec<String> = read
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if !path.is_dir() {
                    return None;
                }
                if !path.join("SKILL.md").is_file() {
                    return None;
                }
                entry.file_name().to_str().map(str::to_string)
            })
            .collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
