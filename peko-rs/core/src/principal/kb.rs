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
//!   set (`MEMORY.md`, `index.md`) and the three cold conventions
//!   (`people/`, `groups/`, `agents/`), each seeded as a small
//!   README/convention doc. Create-if-missing ONLY — an existing file
//!   is never touched; the principal owns its kb from the moment it
//!   exists.
//! - [`migrate_legacy_memory`] — the one-time ADR-054-boot-pass move:
//!   a pre-ADR-055 `<workspace>/MEMORY.md` is MOVED to
//!   `<workspace>/kb/MEMORY.md` so the contract change never orphans a
//!   principal's memory. Idempotent; no-op when nothing to move.
//!
//! ## What the runtime does NOT do
//!
//! - It never re-seeds a deliberately removed file. Absence renders
//!   absent (ADR-050 presence = visibility); deleting `kb/index.md`
//!   removes the hot map section, and that is a valid state.
//! - It never reads cold kb content wholesale. Only the pinned hot
//!   set (`kb/MEMORY.md`, `kb/index.md`) and the two targeted scope
//!   notes reach the prompt (ADR-055 D2/D8: `kb/groups/<channel>.md`
//!   for the run's triggering channel, `kb/agents/<name>.md` for the
//!   named agent); everything else is read-on-demand via tools,
//!   discovered through the hot index.
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
pub const KB_README_MD: &str = "README.md";
pub const PEOPLE_README_MD: &str = "people/README.md";
pub const GROUPS_README_MD: &str = "groups/README.md";
pub const AGENTS_README_MD: &str = "agents/README.md";

const MEMORY_BODY: &str = r#"# Long-term memory

This file is your hot memory: it rides in every prompt, every turn,
for every agent you run. Keep it curated — beliefs, commitments,
preferences, standing decisions. It is NOT a log and NOT a dump:
everything here costs tokens on every turn.

Rules of the house (ADR-055):

- Revise in place. Never append history; update the statement.
- Anything that ages out of relevance gets deleted, not archived —
  your sessions hold the raw history, not this file.
- Everything else you want to persist lives in this `kb/` tree; keep
  `index.md` pointing at it.
"#;

const INDEX_BODY: &str = r#"# Knowledge base index

This file is the map of your `kb/` tree — and like `MEMORY.md`, it
rides in every prompt. One line per area: where it lives and what it
holds. When you add, move, or retire a subtree, update this map in
the same breath.

Revision rules for the whole tree (ADR-055):

- Principal-authored files: revise in place, no append-only history.
- Imported material: replace on refresh; your notes ABOUT an import
  are revised, the import itself is swapped.
- Nothing here is compacted away — that word belongs to sessions.
- Every opaque artifact (image, xlsx, db, …) gets a one-paragraph
  `.md` shadow next to it saying what it is and how to use it.

You own this tree. Restructure it as you see fit — just keep this
index honest, or delete it if you outgrow it.
"#;

const KB_README_BODY: &str = r#"# kb/ — the persistent tree

Everything you know durably lives here: this directory is packaged
with you (Shared tier) and travels when you move. Sessions are your
raw history (Local tier, never packaged); this tree is what you chose
to keep.

Layout at creation: `MEMORY.md` (hot memory), `index.md` (the map),
`people/`, `groups/`, `agents/`. Everything beyond that is yours to
shape — `refs/`, `journal/`, `projects/`, `imports/`, datasets,
whatever your work needs. Only the two hot files ride in every
prompt; the rest is looked up through the index, except the
targeted scope notes (ADR-055 D8).
"#;

const PEOPLE_README_BODY: &str = r#"# people/

One file per person you relate to: `<handle>.md` or `<did>.md`.
Notes, preferences, standing context — anything you'd want to know
at the start of a conversation with them. Revise in place.

This directory is COLD (ADR-055 D2): nothing here is injected
automatically. Your `index.md` — which rides in every prompt — is
what tells your future self this directory exists; look files up
with Read/Glob when a conversation calls for them. Name files after
the person's recognizable handle so lookups are obvious.
"#;

const GROUPS_README_BODY: &str = r#"# groups/

One file per group you participate in: `<channel-or-group-id>.md`.
Conventions of the room, who's in it, what it's about, what you
committed to there. Revise in place.

Mostly cold (ADR-055 D2): files here are NOT cataloged into every
prompt. One targeted exception (D8): when a run's triggering channel
matches a file name here, that file is injected into the bound
agent's prompt for that run. Name files after the channel/group id
as the runtime knows it, so the match happens.
"#;

const AGENTS_README_BODY: &str = r#"# agents/

One file per NAMED agent that deserves durable memory of its own:
`<agent-name>.md`. Standing context for that agent — its remit,
what it has learned across runs, commitments it holds. Revise in
place.

Cold for everyone else, hot for its owner (ADR-055 D8): when a run
starts for agent `<name>` and this directory holds `<name>.md`,
that file is injected into that agent's prompt. Ephemeral, unnamed
spawns get no note — their learnings flow back into the principal's
kb through the spawn result.
"#;

/// Seed the ADR-055 kb scaffold under `workspace`.
///
/// Creates `kb/`, `kb/people/`, `kb/groups/`, `kb/agents/` and writes
/// the six convention files — each ONLY when the file does not
/// already exist (an existing file is never overwritten; create-once
/// semantics, matching `/tmp` + `/trash` seeding). Safe to call
/// repeatedly.
///
/// Returns the relative paths (from `workspace`) of files actually
/// created, sorted — empty when the scaffold already existed.
pub fn seed_kb_scaffold(workspace: &Path) -> Result<Vec<String>> {
    let kb = workspace.join(KB_DIR);
    std::fs::create_dir_all(kb.join("people"))?;
    std::fs::create_dir_all(kb.join("groups"))?;
    std::fs::create_dir_all(kb.join("agents"))?;

    let mut created = Vec::new();
    for (relative, body) in [
        (MEMORY_MD, MEMORY_BODY),
        (INDEX_MD, INDEX_BODY),
        (KB_README_MD, KB_README_BODY),
        (PEOPLE_README_MD, PEOPLE_README_BODY),
        (GROUPS_README_MD, GROUPS_README_BODY),
        (AGENTS_README_MD, AGENTS_README_BODY),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn seeds_all_six_files_and_directories() {
        let ws = temp_workspace();
        let created = seed_kb_scaffold(ws.path()).unwrap();
        assert_eq!(
            created,
            vec![
                "kb/MEMORY.md",
                "kb/README.md",
                "kb/agents/README.md",
                "kb/groups/README.md",
                "kb/index.md",
                "kb/people/README.md",
            ]
        );
        for relative in [
            "kb",
            "kb/people",
            "kb/groups",
            "kb/agents",
            "kb/MEMORY.md",
            "kb/index.md",
            "kb/README.md",
            "kb/people/README.md",
            "kb/groups/README.md",
            "kb/agents/README.md",
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
        assert_eq!(created.len(), 5, "only the five missing files seed");
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
}
