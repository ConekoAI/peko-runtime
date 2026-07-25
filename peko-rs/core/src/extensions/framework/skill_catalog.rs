//! Global skill location catalog.
//!
//! The builtin `Skill` tool resolves skill bodies at handle time. It cannot
//! rely on a single hard-coded directory because skills may be installed via
//! the extension framework (under `~/.peko/data/extensions/`) or live in the
//! legacy `~/.peko/skills/` directory. This catalog is populated by
//! `ExtensionStore` whenever skills are loaded and provides a single,
//! read-only lookup table from skill name to canonical `SKILL.md` path.

use crate::types::ExtensionId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Entry for a single discovered skill.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Skill name (from SKILL.md frontmatter).
    pub name: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub path: PathBuf,
    /// Optional owning extension id, for cleanup on uninstall.
    pub extension_id: Option<ExtensionId>,
}

/// Process-wide catalog mapping skill names to their SKILL.md locations.
#[derive(Debug, Default)]
pub struct SkillCatalog {
    entries: Mutex<HashMap<String, SkillEntry>>,
}

impl SkillCatalog {
    /// Get the global singleton catalog.
    pub fn global() -> &'static Self {
        static CATALOG: OnceLock<SkillCatalog> = OnceLock::new();
        CATALOG.get_or_init(SkillCatalog::default)
    }

    /// Register (or overwrite) a skill entry.
    pub fn register(
        &self,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        extension_id: Option<ExtensionId>,
    ) {
        let name = name.into();
        let entry = SkillEntry {
            name: name.clone(),
            path: path.into(),
            extension_id,
        };
        let mut entries = self.entries.lock().expect("SkillCatalog mutex poisoned");
        entries.insert(name, entry);
    }

    /// Remove a skill entry by name. Idempotent.
    pub fn unregister(&self, name: &str) {
        let mut entries = self.entries.lock().expect("SkillCatalog mutex poisoned");
        entries.remove(name);
    }

    /// Remove every skill owned by the given extension. Used on uninstall.
    pub fn unregister_by_extension(&self, extension_id: &ExtensionId) {
        let mut entries = self.entries.lock().expect("SkillCatalog mutex poisoned");
        entries.retain(|_, entry| {
            entry
                .extension_id
                .as_ref()
                .map(|id| id != extension_id)
                .unwrap_or(true)
        });
    }

    /// Look up a skill by name.
    pub fn resolve(&self, name: &str) -> Option<SkillEntry> {
        let entries = self.entries.lock().expect("SkillCatalog mutex poisoned");
        entries.get(name).cloned()
    }

    /// Return all registered skill names, sorted.
    pub fn list(&self) -> Vec<String> {
        let entries = self.entries.lock().expect("SkillCatalog mutex poisoned");
        let mut names: Vec<String> = entries.keys().cloned().collect();
        names.sort();
        names
    }

    /// Clear the catalog. Called by `ExtensionStore::load_all` before a
    /// full rescan so reloads do not accumulate stale entries.
    pub fn clear(&self) {
        let mut entries = self.entries.lock().expect("SkillCatalog mutex poisoned");
        entries.clear();
    }

    /// Register a skill from a path without an owning extension.
    /// Convenience helper for tests and legacy-path bootstrapping.
    #[cfg(test)]
    pub fn register_path(&self, name: impl Into<String>, path: impl AsRef<std::path::Path>) {
        self.register(name.into(), path.as_ref().to_path_buf(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_catalog() -> SkillCatalog {
        SkillCatalog::default()
    }

    #[test]
    fn register_resolve_unregister_round_trip() {
        let catalog = fresh_catalog();
        catalog.register("docker", PathBuf::from("/skills/docker/SKILL.md"), None);

        let entry = catalog.resolve("docker").expect("registered skill");
        assert_eq!(entry.name, "docker");
        assert_eq!(entry.path, PathBuf::from("/skills/docker/SKILL.md"));

        catalog.unregister("docker");
        assert!(catalog.resolve("docker").is_none());
    }

    #[test]
    fn unregister_by_extension_removes_only_owned() {
        let catalog = fresh_catalog();
        catalog.register(
            "docker",
            PathBuf::from("/ext/docker/SKILL.md"),
            Some(ExtensionId("ext1".to_string())),
        );
        catalog.register("bash", PathBuf::from("/skills/bash/SKILL.md"), None);

        catalog.unregister_by_extension(&ExtensionId("ext1".to_string()));
        assert!(catalog.resolve("docker").is_none());
        assert!(catalog.resolve("bash").is_some());
    }

    #[test]
    fn list_returns_sorted_names() {
        let catalog = fresh_catalog();
        catalog.register("zebra", PathBuf::from("/z/SKILL.md"), None);
        catalog.register("alpha", PathBuf::from("/a/SKILL.md"), None);
        assert_eq!(catalog.list(), vec!["alpha", "zebra"]);
    }
}
