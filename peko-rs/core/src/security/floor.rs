//! The fixed trust floor (ADR-078 D6): what no run may do below a given
//! trust, whatever its policy says. Everything here is a pure function of
//! the stamp, the tool, and the path; the dispatcher applies it to every
//! call.

use crate::common::paths::is_pekos_dir_name;
use std::path::{Component, Path, PathBuf};

use peko_tools_core::ActingFor;

/// Built-in tools an untrusted run never gets: the owner's MCP
/// administration (the shell is owner-only, [`OWNER_ONLY_TOOLS`]).
pub const UNTRUSTED_DENIED_TOOLS: &[&str] = &["MCP"];

/// Built-in tools only an owner run gets. `Plan` reads and writes every
/// plan the principal holds, and plans have no compartment, so a run
/// acting for anyone else would reach the owner's. `Bash` runs as the OS
/// user, so no path rule holds it: it would read every conversation the
/// runtime keeps, its keys, and its settings.
pub const OWNER_ONLY_TOOLS: &[&str] = &["Plan", "Bash"];

/// `Channel` actions only the owner decides: `create` / `invite` change
/// who can reach the principal and whom it can reach (ADR-082 D6);
/// `role` changes the instructions it follows in a conversation (ADR-087
/// D6); `admin` changes who may manage a group (ADR-092 D1); `join` puts
/// the principal in a group someone else runs (ADR-098 D2). An untrusted run never gets them; a trusted run's call waits for
/// the owner ([`crate::security::judge::owner_decides`]).
pub const CHANNEL_OWNER_ACTIONS: &[&str] = &[
    "create", "invite", "role", "admin", "register", "link", "join",
];

/// The `Channel` owner action `params` call, if any.
#[must_use]
pub fn channel_owner_action<'a>(tool_name: &str, params: &'a serde_json::Value) -> Option<&'a str> {
    if tool_name != "Channel" {
        return None;
    }
    params["action"]
        .as_str()
        .filter(|a| CHANNEL_OWNER_ACTIONS.contains(a))
}

/// The longest brief an `Agent` `as_owner` call may hand the owner
/// (ADR-100 D1), in characters: the owner reads all of it.
pub const OWNER_TASK_BRIEF_MAX: usize = 4_000;

/// Whether `params` hand a task to the owner (`Agent` `as_owner`,
/// ADR-100).
#[must_use]
pub fn is_owner_task(tool_name: &str, params: &serde_json::Value) -> bool {
    tool_name == "Agent" && params["as_owner"].as_bool() == Some(true)
}

/// Why `stamp` may not hand this task to the owner, if it may not
/// (ADR-100 D1). Only a run the owner started in a conversation others
/// share may: the owner's own runs need no hand-off, and no one else's
/// run may fill the owner's queue.
#[must_use]
pub fn owner_task_refusal(
    stamp: &ActingFor,
    tool_name: &str,
    params: &serde_json::Value,
) -> Option<String> {
    if !is_owner_task(tool_name, params) {
        return None;
    }
    if stamp.is_owner() {
        return Some(
            "You already act for your owner with their full access here; call Agent without \
             as_owner."
                .into(),
        );
    }
    if !stamp.owner_request {
        return Some(format!(
            "Only a turn your owner started in this conversation can hand work to them \
             (as_owner); this run acts for {}.",
            shown_actor(&stamp.actor)
        ));
    }
    if params["action"].as_str().is_some_and(|a| a != "new") {
        return Some("as_owner works only with action \"new\".".into());
    }
    let path = params["path"].as_str().unwrap_or_default();
    if path.contains('/') {
        return Some(
            "With as_owner, `path` is a bare slug naming the task (no `/`); the task runs in \
             your owner's own sessions."
                .into(),
        );
    }
    let length = params["prompt"].as_str().map_or(0, |p| p.chars().count());
    if length > OWNER_TASK_BRIEF_MAX {
        return Some(format!(
            "The brief is {length} characters; your owner reads all of it, so keep it under \
             {OWNER_TASK_BRIEF_MAX}."
        ));
    }
    None
}

/// Why `stamp` may not make this particular call, if it may not: the
/// per-action part of the floor, beside the per-tool [`tool_refusal`].
#[must_use]
pub fn action_refusal(
    stamp: &ActingFor,
    tool_name: &str,
    params: &serde_json::Value,
) -> Option<String> {
    if !stamp.is_untrusted() {
        return None;
    }
    let what = match channel_owner_action(tool_name, params)? {
        "role" => "Choosing the role for a conversation",
        "admin" => "Choosing a group's admins",
        "join" => "Joining a group",
        _ => "Creating groups and inviting people",
    };
    Some(format!(
        "{what} is not available while acting for {} (untrusted). Ask the owner to set it up.",
        shown_actor(&stamp.actor)
    ))
}

/// An actor address as a refusal message shows it: a peko's wire form
/// `principal:<route>` reads as `peko:<route>` (models quote these
/// messages to people).
fn shown_actor(actor: &str) -> String {
    match actor.strip_prefix("principal:") {
        Some(rest) => format!("peko:{rest}"),
        None => actor.to_string(),
    }
}

/// Workspace files only an owner run may write. Everything an agent's
/// prompt is built from, plus the policy itself.
pub const PROTECTED_FILES: &[&str] = &[
    "peko.toml",
    // Legacy name, kept until the rename to `peko.toml` has happened.
    "principal.toml",
    "POLICY.md",
    "kb/MEMORY.md",
    "kb/index.md",
    "kb/CONVENTIONS.md",
];

/// Workspace trees only an owner run may write: the constitution (what
/// runs and what every agent is told), the notes injected into specific
/// agents' prompts, and the dream notebook whose lessons the nightly pass
/// promotes into `kb/CONVENTIONS.md` (ADR-080 D7).
pub const PROTECTED_DIRS: &[&str] = &[
    "roles",
    "skills",
    "hooks",
    "mcp",
    "workflows",
    "plugins",
    "kb/roles",
    "kb/groups",
    "kb/dreams",
];

/// Where proposals land. The runtime writes here; no run may.
pub const INBOX_DIR: &str = "kb/inbox";

/// The Peko home a peko's workspace sits in: `<home>/pekos/<name>` (or the
/// pre-0.1.0 `<home>/principals/<name>`).
fn peko_home(workspace: &Path) -> Option<&Path> {
    let root = workspace.parent()?;
    if !is_pekos_dir_name(root.file_name()?) {
        return None;
    }
    root.parent()
}

/// Whether `path` lies in some principal's runtime-local tree
/// (`…/pekos/<name>/local/`, or the legacy `…/principals/<name>/local/`):
/// its sessions, ledger, plans, memory index, and schedules. `path` must
/// already be [`resolve`]d.
#[must_use]
pub fn in_principal_local(path: &Path) -> bool {
    let parts: Vec<_> = path.components().map(Component::as_os_str).collect();
    parts
        .windows(3)
        .any(|w| is_pekos_dir_name(w[0]) && w[2] == "local")
}

/// Whether `path` is the runtime's own state, which only owner runs reach:
/// anything in the Peko home outside `workspace` (every conversation's
/// channel log, the runtime's identity and vault, its settings) and any
/// principal's runtime-local tree, wherever the data directory is.
/// Both arguments must already be [`resolve`]d.
#[must_use]
pub fn is_runtime_state(workspace: Option<&Path>, path: &Path) -> bool {
    if in_principal_local(path) {
        return true;
    }
    let Some(workspace) = workspace else {
        return false;
    };
    peko_home(workspace).is_some_and(|home| path.starts_with(home) && !path.starts_with(workspace))
}

/// Whether `path` is in a DM between two people, the requests to start
/// one, or a person's block list (ADR-095 D6). No run reaches these, the
/// owner's included: they are people's own conversations, and no peko is
/// in them. `Bash` runs as the OS user and can still read the files.
/// `path` must already be [`resolve`]d.
#[must_use]
pub fn in_people_only(path: &Path) -> bool {
    if path.components().any(|c| c.as_os_str() == "dm_requests")
        || path
            .file_name()
            .is_some_and(|name| name == "blocked_people.json")
    {
        return true;
    }
    path.ancestors()
        .find(|dir| {
            dir.parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "channels")
        })
        .is_some_and(is_person_dm_dir)
}

/// Whether `dir` holds a DM between two people: kind `dm`, both
/// participants persons.
fn is_person_dm_dir(dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read(dir.join("meta.json")) else {
        return false;
    };
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let people = meta["participants"].as_array().map_or(0, |all| {
        all.iter()
            .filter(|p| p.as_str().is_some_and(|p| p.starts_with("user:")))
            .count()
    });
    meta["kind"] == "dm" && people == 2
}

/// Whether a search from `root` would read a DM between two people
/// (ADR-095 D6): `root` is in one, or holds a channels directory that has
/// one. `candidates` are where channel directories live.
fn search_reaches_people(root: &Path, candidates: &[PathBuf]) -> bool {
    if in_people_only(root) {
        return true;
    }
    candidates
        .iter()
        .filter(|channels| channels.starts_with(root))
        .any(|channels| {
            std::fs::read_dir(channels).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|entry| is_person_dm_dir(&entry.path()))
            })
        })
}

/// Why a non-owner run may not search from `root`, if it may not: a
/// recursive `Glob` / `Grep` from an ancestor of the runtime's state would
/// read it. `data_dir` is the runtime's data directory (sessions live
/// there when it is outside the Peko home).
#[must_use]
pub fn search_root_refusal(
    stamp: &ActingFor,
    workspace: Option<&Path>,
    data_dir: Option<&Path>,
    root: &Path,
) -> Option<String> {
    let root = resolve(root);
    let workspace = workspace.map(resolve);
    let home = workspace
        .as_deref()
        .and_then(peko_home)
        .map(Path::to_path_buf);
    // ADR-095 D6: people's own conversations, for every run.
    let candidates: Vec<PathBuf> = home
        .iter()
        .map(|h| h.join("runtime/channels"))
        .chain(
            data_dir
                .map(resolve)
                .into_iter()
                .flat_map(|d| [d.join("runtime/channels"), d.join("channels")]),
        )
        .collect();
    if search_reaches_people(&root, &candidates) {
        return Some(format!(
            "a search from {} would read conversations between people, which no peko \
             reads; search a narrower folder",
            root.display()
        ));
    }
    if stamp.is_owner() {
        return None;
    }
    let reaches = |state: &Path| state.starts_with(&root);
    let covers_state = home.as_deref().is_some_and(reaches)
        || data_dir.map(resolve).as_deref().is_some_and(reaches);
    covers_state.then(|| {
        format!(
            "a search from {} would read the runtime's conversations and keys; while acting \
             for {} ({}) search inside the workspace or a project folder",
            root.display(),
            shown_actor(&stamp.actor),
            stamp.trust.as_str()
        )
    })
}

/// Whether `path` lies in some principal's tool ledger
/// (`…/pekos/<name>/local/ledger/`, ADR-079 D5). Only the
/// dispatcher writes there. `path` must already be [`resolve`]d.
#[must_use]
pub fn in_tool_ledger(path: &Path) -> bool {
    let parts: Vec<_> = path.components().map(Component::as_os_str).collect();
    parts
        .windows(4)
        .any(|w| is_pekos_dir_name(w[0]) && w[2] == "local" && w[3] == "ledger")
}

/// Why `stamp` may not call `tool_name`, if it may not. `mcp_untrusted_ok`
/// is `Some` for an MCP method: the owner's declaration on its server.
#[must_use]
pub fn tool_refusal(
    stamp: &ActingFor,
    tool_name: &str,
    mcp_untrusted_ok: Option<bool>,
) -> Option<String> {
    if tool_name == "Bash" && super::memory_gate::is_memory_run(stamp) {
        return Some(
            "`Bash` is not available in the memory-maintenance run (ADR-080): it reads \
             other people's conversations, so it writes memory only through the file tools."
                .into(),
        );
    }
    if stamp.is_owner() {
        return None;
    }
    if tool_name == "Bash" {
        return Some(format!(
            "`Bash` is not available while acting for {} ({}): only work done for the owner \
             gets the shell. Use the file tools inside the workspace, or ask the owner for the \
             result.",
            shown_actor(&stamp.actor),
            stamp.trust.as_str()
        ));
    }
    if OWNER_ONLY_TOOLS.contains(&tool_name) {
        return Some(format!(
            "`{tool_name}` holds the owner's internal work and is not available while acting \
             for {} ({}). Track this conversation's work in your replies or with `Task`.",
            shown_actor(&stamp.actor),
            stamp.trust.as_str()
        ));
    }
    if !stamp.is_untrusted() {
        return None;
    }
    let denied = UNTRUSTED_DENIED_TOOLS.contains(&tool_name) || mcp_untrusted_ok == Some(false);
    denied.then(|| {
        format!(
            "`{tool_name}` is not available while acting for {} (untrusted). The owner can \
             permit this actor as `trusted`, or ask for the result on their behalf.",
            shown_actor(&stamp.actor)
        )
    })
}

/// `path` with `.` and `..` resolved lexically, then the deepest existing
/// ancestor canonicalized (following symlinks), so a link planted inside
/// the workspace cannot point a check at one place and a write at another.
#[must_use]
pub fn resolve(path: &Path) -> PathBuf {
    let mut lexical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other),
        }
    }
    let mut existing = lexical.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            let mut out = canonical;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => return lexical,
        }
    }
}

/// `path` relative to `workspace` when it is one of the protected files or
/// inside a protected tree. Both arguments must already be [`resolve`]d.
#[must_use]
pub fn protected_relative(workspace: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(workspace).ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    let inside = |dir: &str| relative == dir || relative.starts_with(&format!("{dir}/"));
    let protected = PROTECTED_FILES.contains(&relative.as_str())
        || PROTECTED_DIRS.iter().any(|dir| inside(dir))
        || inside(INBOX_DIR);
    protected.then_some(relative)
}

/// What the floor does with a file-tool call on one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathDecision {
    Allow,
    /// A non-owner write to a protected file: filed for the owner instead.
    Propose {
        relative: String,
    },
    Deny(String),
}

/// The floor's decision for `stamp` touching `path` (ADR-078 D6).
///
/// - Every run: no write into another principal's workspace (ADR-066 D9),
///   into the proposal inbox, or into any tool ledger (ADR-079).
/// - Non-owner runs: no read of another principal's workspace or of the
///   runtime's own state ([`is_runtime_state`]: conversation logs,
///   sessions, keys, settings), and a write to a protected file becomes a
///   proposal.
/// - Untrusted runs: nothing outside the principal's own workspace.
#[must_use]
pub fn check_path(
    stamp: &ActingFor,
    workspace: Option<&Path>,
    path: &Path,
    write: bool,
) -> PathDecision {
    let target = resolve(path);
    if in_people_only(&target) {
        return PathDecision::Deny(format!(
            "{} is a conversation between people (or their requests or blocks); no peko \
             reads or writes it",
            target.display()
        ));
    }
    if write && in_tool_ledger(&target) {
        return PathDecision::Deny(format!(
            "{} is a tool ledger; only the runtime writes it",
            target.display()
        ));
    }
    let workspace = workspace.map(resolve);
    if let Some(workspace) = &workspace {
        let principals_root = workspace
            .parent()
            .filter(|root| root.file_name().is_some_and(is_pekos_dir_name));
        if let Some(root) = principals_root {
            let foreign = target.starts_with(root) && !target.starts_with(workspace);
            if foreign && (write || !stamp.is_owner()) {
                return PathDecision::Deny(format!("{} belongs to another peko", target.display()));
            }
        }
    }
    if !stamp.is_owner() && is_runtime_state(workspace.as_deref(), &target) {
        return PathDecision::Deny(format!(
            "{} is the runtime's own state (conversations, keys, settings); only work \
             done for the owner reaches it",
            target.display()
        ));
    }
    if stamp.is_untrusted() {
        let inside = workspace
            .as_deref()
            .is_some_and(|workspace| target.starts_with(workspace));
        if !inside {
            return PathDecision::Deny(format!(
                "{} is outside this peko's workspace; while acting for {} (untrusted) \
                 only workspace files are reachable",
                target.display(),
                shown_actor(&stamp.actor)
            ));
        }
    }
    if !write {
        return PathDecision::Allow;
    }
    let Some(workspace) = &workspace else {
        return PathDecision::Allow;
    };
    match protected_relative(workspace, &target) {
        Some(relative)
            if relative == INBOX_DIR || relative.starts_with(&format!("{INBOX_DIR}/")) =>
        {
            PathDecision::Deny(format!(
                "{relative} is the proposal inbox; proposals are filed by the runtime"
            ))
        }
        Some(relative) if !stamp.is_owner() => PathDecision::Propose { relative },
        _ => PathDecision::Allow,
    }
}

/// The path a file-tool call targets, and whether it writes. `None` for
/// tools the path floor does not cover.
#[must_use]
pub fn file_tool_target(tool_name: &str, params: &serde_json::Value) -> Option<(PathBuf, bool)> {
    let (key, write) = match tool_name {
        "Read" => ("file_path", false),
        "Write" | "Edit" => ("file_path", true),
        "Glob" | "Grep" => ("path", false),
        _ => return None,
    };
    let raw = params.get(key).and_then(|v| v.as_str())?;
    Some((crate::tools::builtin::paths::expand_tilde(raw), write))
}

/// Glob patterns a non-owner run may not use: absolute, home-relative,
/// or climbing out of their base.
#[must_use]
pub fn glob_pattern_escapes(pattern: &str) -> bool {
    pattern.starts_with('/')
        || pattern.starts_with('~')
        || Path::new(pattern)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_tools_core::{Compartment, Origin, Trust};

    fn stamp(trust: Trust) -> ActingFor {
        ActingFor {
            actor: "user:hub:x".into(),
            name: None,
            trust,
            compartment: Compartment::Conversation {
                channel: "dm_x".into(),
            },
            origin: Origin::Ingress { channel: None },
            owner_request: false,
        }
    }

    /// ADR-100 D1: only the owner's own turn in a shared room hands a
    /// task to them, as a new task with a short-enough brief.
    #[test]
    fn only_the_owners_capped_turn_hands_a_task_to_them() {
        let task = |extra: serde_json::Value| {
            let mut params = serde_json::json!({
                "prompt": "Run cargo test in ~/repo and report failures",
                "path": "run-tests",
                "as_owner": true,
            });
            params
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            params
        };
        let mut capped = stamp(Trust::Untrusted);
        capped.owner_request = true;
        assert!(owner_task_refusal(&capped, "Agent", &task(serde_json::json!({}))).is_none());
        let mut capped_trusted = stamp(Trust::Trusted);
        capped_trusted.owner_request = true;
        assert!(
            owner_task_refusal(&capped_trusted, "Agent", &task(serde_json::json!({}))).is_none()
        );

        let guest = owner_task_refusal(
            &stamp(Trust::Untrusted),
            "Agent",
            &task(serde_json::json!({})),
        )
        .expect("a guest's turn is refused");
        assert!(guest.contains("Only a turn your owner started"), "{guest}");
        let owner = owner_task_refusal(&stamp(Trust::Owner), "Agent", &task(serde_json::json!({})))
            .expect("the owner's own run needs no hand-off");
        assert!(owner.contains("without as_owner"), "{owner}");
        assert!(owner_task_refusal(
            &capped,
            "Agent",
            &task(serde_json::json!({"action": "resume"}))
        )
        .is_some());
        assert!(owner_task_refusal(
            &capped,
            "Agent",
            &task(serde_json::json!({"path": "sess:/elsewhere"}))
        )
        .is_some());
        let long = "x".repeat(OWNER_TASK_BRIEF_MAX + 1);
        assert!(
            owner_task_refusal(&capped, "Agent", &task(serde_json::json!({"prompt": long})))
                .is_some()
        );
        // Without the flag it is an ordinary call.
        let plain = serde_json::json!({"prompt": "x", "path": "y"});
        assert!(owner_task_refusal(&stamp(Trust::Untrusted), "Agent", &plain).is_none());
        assert!(!is_owner_task("Bash", &task(serde_json::json!({}))));
    }

    struct Tree {
        _dir: tempfile::TempDir,
        mine: PathBuf,
        other: PathBuf,
    }

    fn tree() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("config/pekos");
        let mine = root.join("mine");
        let other = root.join("other");
        std::fs::create_dir_all(mine.join("kb")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        Tree {
            mine: resolve(&mine),
            other: resolve(&other),
            _dir: dir,
        }
    }

    #[test]
    fn both_folder_names_are_runtime_state() {
        for folder in ["pekos", "principals"] {
            let base = PathBuf::from("/home/u/.peko");
            let local = base.join(folder).join("ada/local/sessions/x.jsonl");
            let ledger = base.join(folder).join("ada/local/ledger/0.jsonl");
            assert!(in_principal_local(&local), "{folder}");
            assert!(in_tool_ledger(&ledger), "{folder}");
            assert_eq!(
                peko_home(&base.join(folder).join("ada")),
                Some(base.as_path()),
                "{folder}"
            );
        }
        assert!(!in_principal_local(Path::new("/home/u/notes/ada/local/x")));
    }

    #[test]
    fn untrusted_runs_lose_unmarked_mcp() {
        let low = stamp(Trust::Untrusted);
        assert!(tool_refusal(&low, "MCP", None).is_some());
        assert!(tool_refusal(&low, "mcp__mail__send", Some(false)).is_some());
        assert!(tool_refusal(&low, "mcp__docs__search", Some(true)).is_none());
        assert!(tool_refusal(&low, "Read", None).is_none());
        assert!(tool_refusal(&stamp(Trust::Trusted), "MCP", None).is_none());
    }

    #[test]
    fn only_owner_runs_get_the_shell() {
        for trust in [Trust::Untrusted, Trust::Trusted] {
            let reason = tool_refusal(&stamp(trust), "Bash", None).expect("refused");
            assert!(reason.contains("owner"), "{reason}");
        }
        assert!(tool_refusal(&stamp(Trust::Owner), "Bash", None).is_none());
        assert!(tool_refusal(&ActingFor::runtime(), "Bash", None).is_none());
    }

    /// ADR-095 D6: a DM between two people, the requests to start one,
    /// and the block list are out of every run's reach, the owner's too;
    /// a DM with a peko stays the owner's.
    #[test]
    fn no_run_reaches_conversations_between_people() {
        let t = tree();
        let home = t.mine.parent().unwrap().parent().unwrap().to_path_buf();
        let channels = home.join("runtime/channels");
        for (dir, participants) in [
            ("dm_people", vec!["user:local:local", "user:hub:bob"]),
            ("dm_peko", vec!["user:local:local", "principal:hub:p"]),
        ] {
            std::fs::create_dir_all(channels.join(dir)).unwrap();
            std::fs::write(
                channels.join(dir).join("meta.json"),
                serde_json::json!({"kind": "dm", "participants": participants}).to_string(),
            )
            .unwrap();
        }
        let owner = stamp(Trust::Owner);
        for path in [
            channels.join("dm_people/events.jsonl"),
            channels.join("dm_people"),
            home.join("runtime/dm_requests/dm_x.json"),
            home.join("runtime/blocked_people.json"),
        ] {
            for write in [false, true] {
                assert!(
                    matches!(
                        check_path(&owner, Some(&t.mine), &path, write),
                        PathDecision::Deny(_)
                    ),
                    "{} write={write}",
                    path.display()
                );
            }
        }
        assert!(matches!(
            check_path(
                &owner,
                Some(&t.mine),
                &channels.join("dm_peko/events.jsonl"),
                false
            ),
            PathDecision::Allow
        ));
        for root in [&home, &channels, &channels.join("dm_people")] {
            assert!(
                search_root_refusal(&owner, Some(&t.mine), None, root).is_some(),
                "{}",
                root.display()
            );
        }
        assert!(
            search_root_refusal(&owner, Some(&t.mine), None, &channels.join("dm_peko")).is_none()
        );
    }

    /// The runtime's conversations, keys and settings are the owner's:
    /// a trusted run reads its workspace and the owner's projects, not them.
    #[test]
    fn only_owner_runs_reach_the_runtimes_own_state() {
        let t = tree();
        let home = t.mine.parent().unwrap().parent().unwrap().to_path_buf();
        let dir = tempfile::tempdir().unwrap();
        let data = resolve(dir.path());
        let project = data.join("projects/site/README.md");
        let state = [
            home.join("runtime/channels/dm_abc/events.jsonl"),
            home.join("runtime/identity.toml"),
            home.join("vault.enc"),
            home.join("pekos/channels/chan_x/events.jsonl"),
            data.join("pekos/mine/local/sessions/s.jsonl"),
            data.join("pekos/other/local/sessions/s.jsonl"),
            t.mine.join("local/sessions/s.jsonl"),
        ];
        for trust in [Trust::Untrusted, Trust::Trusted] {
            for path in &state {
                for write in [false, true] {
                    assert!(
                        matches!(
                            check_path(&stamp(trust), Some(&t.mine), path, write),
                            PathDecision::Deny(_)
                        ),
                        "{trust:?} {} write={write}",
                        path.display()
                    );
                }
            }
        }
        for path in &state[..3] {
            assert_eq!(
                check_path(&stamp(Trust::Owner), Some(&t.mine), path, false),
                PathDecision::Allow
            );
        }
        assert_eq!(
            check_path(
                &stamp(Trust::Trusted),
                Some(&t.mine),
                &t.mine.join("notes.md"),
                false
            ),
            PathDecision::Allow
        );
        assert_eq!(
            check_path(&stamp(Trust::Trusted), Some(&t.mine), &project, false),
            PathDecision::Allow,
            "a trusted run still reads the owner's projects"
        );
    }

    #[test]
    fn non_owner_searches_may_not_start_above_the_runtimes_state() {
        let t = tree();
        let home = t.mine.parent().unwrap().parent().unwrap().to_path_buf();
        let dir = tempfile::tempdir().unwrap();
        let data = resolve(&dir.path().join("Application Support/peko"));
        let trusted = stamp(Trust::Trusted);
        // The filesystem root holding the state: `/`, or on Windows the
        // drive (`/` there is the current drive's root, maybe another one).
        let fs_root = home.ancestors().last().unwrap().to_path_buf();
        for root in [home.clone(), home.parent().unwrap().to_path_buf(), fs_root] {
            assert!(
                search_root_refusal(&trusted, Some(&t.mine), Some(&data), &root).is_some(),
                "{}",
                root.display()
            );
        }
        assert!(search_root_refusal(&trusted, Some(&t.mine), Some(&data), dir.path()).is_some());
        assert!(search_root_refusal(&trusted, Some(&t.mine), Some(&data), &t.mine).is_none());
        assert!(
            search_root_refusal(&trusted, Some(&t.mine), Some(&data), &data.join("x")).is_none()
        );
        assert!(
            search_root_refusal(&stamp(Trust::Owner), Some(&t.mine), Some(&data), &home).is_none()
        );
    }

    #[test]
    fn only_owner_runs_get_plans() {
        for trust in [Trust::Untrusted, Trust::Trusted] {
            let reason = tool_refusal(&stamp(trust), "Plan", None).expect("refused");
            assert!(reason.contains(trust.as_str()), "{reason}");
        }
        assert!(tool_refusal(&stamp(Trust::Owner), "Plan", None).is_none());
        assert!(tool_refusal(&ActingFor::runtime(), "Plan", None).is_none());
        assert!(tool_refusal(&ActingFor::unattributed(), "Plan", None).is_some());
    }

    #[test]
    fn no_run_writes_a_tool_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("data/pekos/mine/local/ledger");
        std::fs::create_dir_all(&ledger).unwrap();
        let file = ledger.join("ledger-2026-10-14.jsonl");
        for trust in [Trust::Owner, Trust::Trusted, Trust::Untrusted] {
            let decision = check_path(&stamp(trust), None, &file, true);
            assert!(
                matches!(decision, PathDecision::Deny(ref reason) if reason.contains("tool ledger")),
                "{trust:?}: {decision:?}"
            );
        }
        let sneaky = dir
            .path()
            .join("data/pekos/mine/local/cache/../ledger/x.jsonl");
        assert!(matches!(
            check_path(&stamp(Trust::Owner), None, &sneaky, true),
            PathDecision::Deny(_)
        ));
        assert_eq!(
            check_path(&stamp(Trust::Owner), None, &file, false),
            PathDecision::Allow,
            "reading stays open to owner runs"
        );
        assert!(!in_tool_ledger(Path::new("/w/ledger/notes.md")));
    }

    #[test]
    fn untrusted_runs_are_confined_to_the_workspace() {
        let t = tree();
        let low = stamp(Trust::Untrusted);
        assert_eq!(
            check_path(&low, Some(&t.mine), &t.mine.join("notes.md"), false),
            PathDecision::Allow
        );
        let escape = t.mine.join("../../../etc/passwd");
        assert!(matches!(
            check_path(&low, Some(&t.mine), &escape, false),
            PathDecision::Deny(_)
        ));
        assert!(matches!(
            check_path(&low, None, &t.mine.join("notes.md"), false),
            PathDecision::Deny(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_carry_a_check_out_of_the_workspace() {
        let t = tree();
        std::os::unix::fs::symlink(&t.other, t.mine.join("link")).unwrap();
        let low = stamp(Trust::Untrusted);
        assert!(matches!(
            check_path(&low, Some(&t.mine), &t.mine.join("link/secret"), false),
            PathDecision::Deny(_)
        ));
    }

    #[test]
    fn protected_writes_become_proposals_below_owner() {
        let t = tree();
        let memory = t.mine.join("kb/MEMORY.md");
        for trust in [Trust::Untrusted, Trust::Trusted] {
            assert_eq!(
                check_path(&stamp(trust), Some(&t.mine), &memory, true),
                PathDecision::Propose {
                    relative: "kb/MEMORY.md".into()
                }
            );
        }
        assert_eq!(
            check_path(&stamp(Trust::Owner), Some(&t.mine), &memory, true),
            PathDecision::Allow
        );
        assert_eq!(
            check_path(
                &stamp(Trust::Trusted),
                Some(&t.mine),
                &t.mine.join("hooks/x/hook.toml"),
                true
            ),
            PathDecision::Propose {
                relative: "hooks/x/hook.toml".into()
            }
        );
        assert!(matches!(
            check_path(
                &stamp(Trust::Owner),
                Some(&t.mine),
                &t.mine.join("kb/inbox/x.md"),
                true
            ),
            PathDecision::Deny(_)
        ));
    }

    #[test]
    fn no_run_writes_another_principals_workspace() {
        let t = tree();
        let target = t.other.join("kb/MEMORY.md");
        assert!(matches!(
            check_path(&stamp(Trust::Owner), Some(&t.mine), &target, true),
            PathDecision::Deny(_)
        ));
        assert_eq!(
            check_path(&stamp(Trust::Owner), Some(&t.mine), &target, false),
            PathDecision::Allow
        );
        assert!(matches!(
            check_path(&stamp(Trust::Trusted), Some(&t.mine), &target, false),
            PathDecision::Deny(_)
        ));
    }

    #[test]
    fn escaping_glob_patterns_are_detected() {
        assert!(glob_pattern_escapes("../**/*"));
        assert!(glob_pattern_escapes("/etc/*"));
        assert!(glob_pattern_escapes("~/.ssh/*"));
        assert!(!glob_pattern_escapes("kb/**/*.md"));
    }
}
