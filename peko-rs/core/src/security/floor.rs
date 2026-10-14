//! The fixed trust floor (ADR-078 D6): what no run may do below a given
//! trust, whatever its policy says. Everything here is a pure function of
//! the stamp, the tool, and the path; the dispatcher applies it to every
//! call.

use std::path::{Component, Path, PathBuf};

use peko_tools_core::ActingFor;

/// Built-in tools an untrusted run never gets: the shell (the master key
/// every other rule depends on) and the owner's MCP administration.
pub const UNTRUSTED_DENIED_TOOLS: &[&str] = &["Bash", "MCP"];

/// Workspace files only an owner run may write. Everything an agent's
/// prompt is built from, plus the policy itself.
pub const PROTECTED_FILES: &[&str] = &[
    "principal.toml",
    "POLICY.md",
    "kb/MEMORY.md",
    "kb/index.md",
    "kb/CONVENTIONS.md",
];

/// Workspace trees only an owner run may write: the constitution (what
/// runs and what every agent is told) and the notes injected into
/// specific agents' prompts.
pub const PROTECTED_DIRS: &[&str] = &[
    "roles",
    "skills",
    "hooks",
    "mcp",
    "workflows",
    "plugins",
    "kb/roles",
    "kb/groups",
];

/// Where proposals land. The runtime writes here; no run may.
pub const INBOX_DIR: &str = "kb/inbox";

/// Why `stamp` may not call `tool_name`, if it may not. `mcp_untrusted_ok`
/// is `Some` for an MCP method: the owner's declaration on its server.
#[must_use]
pub fn tool_refusal(
    stamp: &ActingFor,
    tool_name: &str,
    mcp_untrusted_ok: Option<bool>,
) -> Option<String> {
    if !stamp.is_untrusted() {
        return None;
    }
    let denied = UNTRUSTED_DENIED_TOOLS.contains(&tool_name) || mcp_untrusted_ok == Some(false);
    denied.then(|| {
        format!(
            "`{tool_name}` is not available while acting for {} (untrusted). The owner can \
             permit this actor as `trusted`, or ask for the result on their behalf.",
            stamp.actor
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
///   and no write into the proposal inbox.
/// - Non-owner runs: no read of another principal's workspace, and a
///   write to a protected file becomes a proposal.
/// - Untrusted runs: nothing outside the principal's own workspace.
#[must_use]
pub fn check_path(
    stamp: &ActingFor,
    workspace: Option<&Path>,
    path: &Path,
    write: bool,
) -> PathDecision {
    let target = resolve(path);
    let workspace = workspace.map(resolve);
    if let Some(workspace) = &workspace {
        let principals_root = workspace
            .parent()
            .filter(|root| root.file_name().is_some_and(|name| name == "principals"));
        if let Some(root) = principals_root {
            let foreign = target.starts_with(root) && !target.starts_with(workspace);
            if foreign && (write || !stamp.is_owner()) {
                return PathDecision::Deny(format!(
                    "{} belongs to another principal",
                    target.display()
                ));
            }
        }
    }
    if stamp.is_untrusted() {
        let inside = workspace
            .as_deref()
            .is_some_and(|workspace| target.starts_with(workspace));
        if !inside {
            return PathDecision::Deny(format!(
                "{} is outside this principal's workspace; while acting for {} (untrusted) \
                 only workspace files are reachable",
                target.display(),
                stamp.actor
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

/// Glob patterns an untrusted run may not use: absolute, home-relative,
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
        }
    }

    struct Tree {
        _dir: tempfile::TempDir,
        mine: PathBuf,
        other: PathBuf,
    }

    fn tree() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("config/principals");
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
    fn untrusted_runs_lose_the_shell_and_unmarked_mcp() {
        let low = stamp(Trust::Untrusted);
        assert!(tool_refusal(&low, "Bash", None).is_some());
        assert!(tool_refusal(&low, "MCP", None).is_some());
        assert!(tool_refusal(&low, "mcp__mail__send", Some(false)).is_some());
        assert!(tool_refusal(&low, "mcp__docs__search", Some(true)).is_none());
        assert!(tool_refusal(&low, "Read", None).is_none());
        assert!(tool_refusal(&stamp(Trust::Trusted), "Bash", None).is_none());
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
