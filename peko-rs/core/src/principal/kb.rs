//! The principal knowledge base scaffold (ADR-055).
//!
//! A principal's persistent knowledge — long-term memory, notes about
//! people and groups, reference material, daily logs, datasets — lives
//! in ONE tree: `<workspace>/kb/` (Shared tier, packaged). There is no
//! separate `memory/` concept and no compaction: files are revised in
//! place (principal-authored) or replaced on refresh (imports).
//!
//! This module owns two create-once primitives:
//!
//! - [`seed_kb_scaffold`] — the P0 floor: `kb/` plus its pinned hot
//!   set (`MEMORY.md`, `index.md`, `CONVENTIONS.md`) and the three
//!   cold conventions (`people/`, `groups/`, `roles/`), each seeded as
//!   a small README/convention doc. Create-if-missing ONLY — an
//!   existing file is never touched; the principal owns its kb from
//!   the moment it exists.
//! - [`migrate_legacy_memory`] — the one-time ADR-054-boot-pass move:
//!   a pre-ADR-055 `<workspace>/MEMORY.md` is MOVED to
//!   `<workspace>/kb/MEMORY.md` so the contract change never orphans a
//!   principal's memory. Idempotent; no-op when nothing to move.
//! - [`migrate_legacy_roles_dir`] — the one-time rename move:
//!   a pre-rename `kb/agents/` directory is MOVED to `kb/roles/` so
//!   the D8 note directory pairs with the role-file terminology
//!   (ADR-052 D3). Idempotent; no-op when nothing to move.
//!
//! ## What the runtime does NOT do
//!
//! - It never re-seeds a deliberately removed file. Absence renders
//!   absent (ADR-050 presence = visibility); deleting `kb/index.md`
//!   removes the hot map section, and that is a valid state.
//! - It never reads cold kb content wholesale. Only the pinned hot
//!   set (`kb/MEMORY.md`, `kb/index.md`, `kb/CONVENTIONS.md`) and the
//!   two targeted scope notes reach the prompt (ADR-055 D2/D8:
//!   `kb/groups/<channel>.md` for the run's triggering channel,
//!   `kb/roles/<name>.md` for the named role); everything else is
//!   read-on-demand via tools, discovered through the hot index.
//! - It never seeds framework manuals into `kb/` (ADR-055 D6 —
//!   runtime truth stays with the runtime; pointer, not copy).

use anyhow::Result;
use std::path::Path;

/// Directory (relative to the principal workspace) holding the
/// principal's persistent knowledge base. Mirrors
/// `peko_engine`'s `KB_DIR` (the engine crate is the prompt-side
/// consumer; this is the provisioning-side twin).
pub const KB_DIR: &str = "kb";

/// The kb files the P0 scaffold seeds. Each is written only when
/// absent — see [`seed_kb_scaffold`].
pub const MEMORY_MD: &str = "MEMORY.md";
pub const INDEX_MD: &str = "index.md";
pub const CONVENTIONS_MD: &str = "CONVENTIONS.md";
pub const KB_README_MD: &str = "README.md";
pub const PEOPLE_README_MD: &str = "people/README.md";
pub const GROUPS_README_MD: &str = "groups/README.md";
pub const ROLES_README_MD: &str = "roles/README.md";

const MEMORY_BODY: &str = include_str!("../resources/kb/MEMORY.md");
const INDEX_BODY: &str = include_str!("../resources/kb/index.md");
const CONVENTIONS_BODY: &str = include_str!("../resources/kb/CONVENTIONS.md");
const KB_README_BODY: &str = include_str!("../resources/kb/README.md");
const PEOPLE_README_BODY: &str = include_str!("../resources/kb/people/README.md");
const GROUPS_README_BODY: &str = include_str!("../resources/kb/groups/README.md");
const ROLES_README_BODY: &str = include_str!("../resources/kb/roles/README.md");

/// Seed the ADR-055 kb scaffold under `workspace`.
///
/// Creates `kb/`, `kb/people/`, `kb/groups/`, `kb/roles/`, `kb/journal/`
/// and writes the seven convention files — each ONLY when the file does
/// not already exist (an existing file is never overwritten; create-once
/// semantics, matching `/tmp` + `/trash` seeding). Safe to call
/// repeatedly. Default bodies live in `resources/kb/` (not inline) —
/// they are a starting floor, not a render-time fallback.
///
/// Returns the relative paths (from `workspace`) of files actually
/// created, sorted — empty when the scaffold already existed.
pub fn seed_kb_scaffold(workspace: &Path) -> Result<Vec<String>> {
    let kb = workspace.join(KB_DIR);
    std::fs::create_dir_all(kb.join("people"))?;
    std::fs::create_dir_all(kb.join("groups"))?;
    std::fs::create_dir_all(kb.join("roles"))?;
    std::fs::create_dir_all(kb.join("journal"))?;

    let mut created = Vec::new();
    for (relative, body) in [
        (MEMORY_MD, MEMORY_BODY),
        (INDEX_MD, INDEX_BODY),
        (CONVENTIONS_MD, CONVENTIONS_BODY),
        (KB_README_MD, KB_README_BODY),
        (PEOPLE_README_MD, PEOPLE_README_BODY),
        (GROUPS_README_MD, GROUPS_README_BODY),
        (ROLES_README_MD, ROLES_README_BODY),
    ] {
        let path = kb.join(relative);
        if path.exists() {
            continue;
        }
        std::fs::write(&path, body)?;
        created.push(format!("{KB_DIR}/{relative}"));
    }
    created.sort();
    Ok(created)
}

/// Seed the birth entry in the principal's journal (provision-time).
///
/// Writes `kb/journal/YYYY-MM-DD.md` (UTC date) with a first line
/// noting the peko's birth — the first entry of the append-only daily
/// journal. Create-if-missing: if the day's file somehow already
/// exists it is left untouched. Returns the relative path of the file
/// when created, `None` when it already existed.
pub fn seed_journal_birth_entry(
    workspace: &Path,
    principal_name: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<String>> {
    let journal_dir = workspace.join(KB_DIR).join("journal");
    std::fs::create_dir_all(&journal_dir)?;
    let date = now.format("%Y-%m-%d");
    let relative = format!("{KB_DIR}/journal/{date}.md");
    let path = workspace.join(&relative);
    if path.exists() {
        return Ok(None);
    }
    let time = now.format("%Y-%m-%d %H:%M UTC");
    let body = format!(
        "# {date}\n\n\
         - Born: provisioned as principal `{principal_name}` at {time}. \
         This journal is append-only — one file per day, newest entries \
         at the end; never rewrite past entries.\n"
    );
    std::fs::write(&path, body)?;
    Ok(Some(relative))
}

/// One-time migration of a pre-ADR-055 principal: move the legacy
/// `<workspace>/MEMORY.md` into `<workspace>/kb/MEMORY.md`.
///
/// Runs in the ADR-054 boot pass, so every existing principal keeps
/// its memory through the contract change. Idempotent by construction:
/// no legacy file, or an existing `kb/MEMORY.md`, means no-op. The
/// move (not copy) is deliberate — leaving the legacy file behind
/// would fork the principal's memory into two sources of truth.
///
/// Returns `true` when the migration moved a file.
pub fn migrate_legacy_memory(workspace: &Path) -> Result<bool> {
    let legacy = workspace.join(MEMORY_MD);
    let target_dir = workspace.join(KB_DIR);
    let target = target_dir.join(MEMORY_MD);
    if !legacy.exists() || target.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(&target_dir)?;
    std::fs::rename(&legacy, &target)?;
    Ok(true)
}

/// One-time migration of a pre-rename principal: MOVE the legacy
/// `kb/agents/` directory to `kb/roles/` (2026-09-28 rename — the
/// D8 note pairs with the T1 role file, so the directory name follows
/// the role terminology). Idempotent: no legacy dir, or an existing
/// `kb/roles/`, means no-op — an existing `kb/roles/` wins and the
/// legacy dir is left in place (a silent destructive merge is worse
/// than a visible leftover, matching `migrate_legacy_memory`).
///
/// Returns `true` when the migration moved the directory.
pub fn migrate_legacy_roles_dir(workspace: &Path) -> Result<bool> {
    let legacy = workspace.join(KB_DIR).join("agents");
    let target = workspace.join(KB_DIR).join("roles");
    if !legacy.is_dir() || target.exists() {
        return Ok(false);
    }
    std::fs::rename(&legacy, &target)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn seeds_all_seven_files_and_directories() {
        let ws = temp_workspace();
        let created = seed_kb_scaffold(ws.path()).unwrap();
        assert_eq!(
            created,
            vec![
                "kb/CONVENTIONS.md",
                "kb/MEMORY.md",
                "kb/README.md",
                "kb/groups/README.md",
                "kb/index.md",
                "kb/people/README.md",
                "kb/roles/README.md",
            ]
        );
        for relative in [
            "kb",
            "kb/people",
            "kb/groups",
            "kb/roles",
            "kb/MEMORY.md",
            "kb/index.md",
            "kb/CONVENTIONS.md",
            "kb/README.md",
            "kb/people/README.md",
            "kb/groups/README.md",
            "kb/roles/README.md",
        ] {
            assert!(ws.path().join(relative).exists(), "{relative} must exist");
        }
    }

    #[test]
    fn repeated_seed_is_a_noop() {
        let ws = temp_workspace();
        assert!(!seed_kb_scaffold(ws.path()).unwrap().is_empty());
        assert!(seed_kb_scaffold(ws.path()).unwrap().is_empty());
    }

    /// The principal owns its kb from the moment it exists: a curated
    /// MEMORY.md is never clobbered by re-seeding, and its siblings
    /// still seed around it.
    #[test]
    fn existing_files_are_never_overwritten() {
        let ws = temp_workspace();
        std::fs::create_dir_all(ws.path().join("kb")).unwrap();
        std::fs::write(ws.path().join("kb").join("MEMORY.md"), "curated").unwrap();

        let created = seed_kb_scaffold(ws.path()).unwrap();
        assert_eq!(created.len(), 6, "only the six missing files seed");
        assert!(!created.contains(&"kb/MEMORY.md".to_string()));
        assert_eq!(
            std::fs::read_to_string(ws.path().join("kb").join("MEMORY.md")).unwrap(),
            "curated"
        );
    }

    #[test]
    fn migrates_legacy_root_memory_into_kb() {
        let ws = temp_workspace();
        std::fs::write(ws.path().join("MEMORY.md"), "old beliefs").unwrap();

        assert!(migrate_legacy_memory(ws.path()).unwrap());
        assert!(!ws.path().join("MEMORY.md").exists(), "move, not copy");
        assert_eq!(
            std::fs::read_to_string(ws.path().join("kb").join("MEMORY.md")).unwrap(),
            "old beliefs"
        );
    }

    #[test]
    fn migration_is_idempotent_and_noop_without_legacy() {
        let ws = temp_workspace();
        assert!(!migrate_legacy_memory(ws.path()).unwrap());

        std::fs::write(ws.path().join("MEMORY.md"), "old beliefs").unwrap();
        std::fs::create_dir_all(ws.path().join("kb")).unwrap();
        std::fs::write(ws.path().join("kb").join("MEMORY.md"), "new beliefs").unwrap();

        // Existing kb/MEMORY.md wins; the legacy file is NOT deleted
        // (a silent destructive merge is worse than a visible leftover).
        assert!(!migrate_legacy_memory(ws.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(ws.path().join("kb").join("MEMORY.md")).unwrap(),
            "new beliefs"
        );
        assert!(ws.path().join("MEMORY.md").exists());
    }

    #[test]
    fn migrates_legacy_agents_dir_into_roles() {
        let ws = temp_workspace();
        let agents = ws.path().join("kb").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("coder.md"), "remit notes").unwrap();

        assert!(migrate_legacy_roles_dir(ws.path()).unwrap());
        assert!(!agents.exists(), "move, not copy");
        assert_eq!(
            std::fs::read_to_string(ws.path().join("kb").join("roles").join("coder.md")).unwrap(),
            "remit notes"
        );
    }

    #[test]
    fn roles_dir_migration_is_idempotent_and_noop_without_legacy() {
        let ws = temp_workspace();
        assert!(!migrate_legacy_roles_dir(ws.path()).unwrap());

        // Existing kb/roles/ wins; the legacy dir is NOT deleted.
        let agents = ws.path().join("kb").join("agents");
        let roles = ws.path().join("kb").join("roles");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::create_dir_all(&roles).unwrap();

        assert!(!migrate_legacy_roles_dir(ws.path()).unwrap());
        assert!(agents.exists());
        assert!(roles.exists());
    }

    /// Provision-time journal birth entry: written once, in the UTC
    /// day file, noting the peko's birth.
    #[test]
    fn journal_birth_entry_seeds_once() {
        let ws = temp_workspace();
        let now = chrono::Utc::now();

        let created = seed_journal_birth_entry(ws.path(), "nova", now).unwrap();
        let rel = created.expect("birth entry created");
        assert_eq!(rel, format!("kb/journal/{}.md", now.format("%Y-%m-%d")));

        let body = std::fs::read_to_string(ws.path().join(&rel)).unwrap();
        assert!(body.contains("Born: provisioned as principal `nova`"));
        assert!(body.contains("append-only"));

        // Idempotent: the day's file already exists → untouched.
        assert!(seed_journal_birth_entry(ws.path(), "nova", now)
            .unwrap()
            .is_none());
        let body2 = std::fs::read_to_string(ws.path().join(&rel)).unwrap();
        assert_eq!(body, body2, "no duplicate birth line");
    }
}
