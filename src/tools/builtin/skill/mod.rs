//! Skill tool
//!
//! Provides `Skill` so the agent can invoke a SKILL.md body on-demand
//! with argument substitution. The skill list is gated by the principal's
//! `capabilities.skills` allowlist, resolved at handle time via the global
//! [`SkillStateRegistry`](crate::principal::SkillStateRegistry).
//!
//! Argument substitution matches Claude Code's skill syntax:
//! - `$ARGUMENTS` — the full args array joined with single spaces
//! - `$0`, `$1`, … `$N` — positional, 0-indexed
//! - `$name` — declared in the SKILL.md frontmatter `arguments:` list;
//!   names map to positions in order. A `$name` whose name is not in the
//!   list is left unsubstituted (Claude-compatible behavior).
//!
//! Escape: `\$` is left literal (so `\$issue` in the body renders as
//! `$issue` rather than substituting).
//!
//! Dynamic context: inline `` !`command` `` and fenced `` ```! `` blocks
//! in the body are resolved by [`preprocess::preprocess_dynamic_context`]
//! before argument substitution, so skills can author live-state-aware
//! bodies (git status, env vars, process lists, …) without forcing the
//! LLM to call `Bash` first. See `preprocess` for the shell runner,
//! the glob allowlist, and the failure-handling rules.

pub mod preprocess;

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use crate::extensions::framework::adapters::parsing::parse_yaml_frontmatter_typed;
use crate::extensions::skill::SkillFrontmatter;
use crate::tools::core::traits::Tool;

/// Sentinel used to escape `\$` placeholders during substitution.
const ESCAPE_SENTINEL: &str = "\x00ESCAPED_DOLLAR\x00";

/// Scan `body` for the largest `$N` (where N is a sequence of digits)
/// that appears as a positional placeholder. Used to bound the
/// high-to-low positional-substitution loop so that e.g. `$10` is
/// handled even when the caller only passed 2 args.
///
/// Must be called on `body` *after* the escape pass has stripped `\$`,
/// otherwise it could mistake a literal `\$10` for a positional.
fn scan_max_positional_index(body: &str) -> usize {
    let bytes = body.as_bytes();
    let mut max_idx = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if let Ok(n) = body[i + 1..j].parse::<usize>() {
                if n > max_idx {
                    max_idx = n;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    max_idx
}

/// Tool for invoking a SKILL.md body with argument substitution.
///
/// The `Skill` tool is a singleton registered once on the daemon-global
/// `ExtensionCore`. Per-principal allowlist and workspace state are resolved
/// at handle time via the global [`SkillStateRegistry`] using the
/// `principal_id` carried in `ToolContext`. This avoids the previous
/// per-message re-registration race (P2 audit issue #2).
pub struct SkillTool {
    /// Daemon-global skills directory (`~/.peko/skills/`).
    ///
    /// Set on first use so tests can construct a `SkillTool` without a
    /// real data directory and then point it at a temp dir via
    /// [`Self::with_skills_dir_for_test`].
    skills_dir: std::sync::OnceLock<PathBuf>,
}

impl SkillTool {
    /// Build the singleton `SkillTool`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            skills_dir: std::sync::OnceLock::new(),
        }
    }

    /// Override the skills directory for a single test instance.
    ///
    /// # Panics
    /// Panics if the skills directory has already been set.
    #[cfg(test)]
    fn with_skills_dir_for_test(self, dir: PathBuf) -> Self {
        self.skills_dir
            .set(dir)
            .expect("skills_dir not already set");
        self
    }

    fn skills_dir(&self) -> &PathBuf {
        self.skills_dir.get_or_init(|| {
            crate::common::paths::PathResolver::new()
                .skills_dir()
                .clone()
        })
    }
}

impl Default for SkillTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "Skill"
    }

    fn description(&self) -> String {
        r#"Invoke a SKILL.md body with argument substitution.

Parameters:
- name: required — skill name (must match a discovered SKILL.md directory name AND be in the principal's enabled allowlist).
- args: optional array of strings — positional arguments.

Argument substitution (Claude-style):
- $ARGUMENTS — full args array joined with single spaces
- $0, $1, … — positional, 0-indexed
- $name — declared in SKILL.md frontmatter `arguments:` list; positions map to names by order

Escape: prefix with `\$` to keep a placeholder literal.

If a `$name` placeholder's name is not declared in the frontmatter `arguments:` list, it is left unsubstituted in the rendered body.

Dynamic context (Claude-style):
- Inline: `!` + `command` + `` ` `` — runs `command` and inlines its stdout. Only recognized when `!` is at the start of a line or preceded by ASCII whitespace.
- Fenced: ` ```! ` … ` ``` ` — same idea, multiline; the entire fence is replaced by the command output.
- Single pass: substituted output is not re-scanned.
- Failure mode: on non-zero exit, stdout is inlined verbatim and a `stderr: <stderr>` line is appended. On timeout, `stderr: command timed out after 5000 ms` is appended.
- Frontmatter knobs:
  - `shell: bash` (default; only supported value today)
  - `allowed-tools: ["git *", "pwd", ...]` — glob allowlist. Empty list = all commands allowed. Glob is anchored to the full trimmed command.

Returns:
- { name, body } — the skill's body with dynamic context resolved, then arguments substituted.
- { error, skill } — structured error: skill_not_enabled, skill_unreadable, or unknown_skill."#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name (must match a discovered SKILL.md directory name)"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Positional arguments. Maps to $0..$N and $ARGUMENTS in the body."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Backward-compatible test entrypoint: run with an empty ToolContext.
        // Production always routes through execute_with_context, which carries
        // the principal_id needed to resolve per-principal skill state.
        let ctx = crate::tools::ToolContext::default_for_tool("Skill");
        self.execute_with_context(params, &ctx).await
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &crate::tools::ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required parameter 'name'"))?
            .to_string();

        let args: Vec<String> = params
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Resolve per-principal state from the global registry. Fail closed:
        // no registry entry means the skill is not enabled for this caller.
        let principal_id = ctx
            .principal_id
            .as_ref()
            .map(|pid| crate::principal::PrincipalId(pid.clone()));
        let Some(pid) = principal_id else {
            return Ok(json!({
                "error": "skill_not_enabled",
                "skill": name,
            }));
        };

        let state = crate::principal::SkillStateRegistry::global()
            .get(&pid)
            .await;
        let Some(state) = state else {
            return Ok(json!({
                "error": "skill_not_enabled",
                "skill": name,
            }));
        };

        if !state.is_enabled(&name) {
            return Ok(json!({
                "error": "skill_not_enabled",
                "skill": name,
            }));
        }

        let skill_md = self.skills_dir().join(&name).join("SKILL.md");
        let content = std::fs::read_to_string(&skill_md).map_err(|e| {
            anyhow::anyhow!("skill_unreadable: failed to read SKILL.md for skill {name}: {e}")
        })?;

        let (frontmatter, body): (SkillFrontmatter, String) =
            parse_yaml_frontmatter_typed(&content).map_err(|e| {
                anyhow::anyhow!(
                "skill_unreadable: failed to parse frontmatter in SKILL.md for skill {name}: {e}"
            )
            })?;

        // `parse_yaml_frontmatter` includes the newline that follows the
        // closing `---` in the body. Trim it so the rendered output
        // matches what the SKILL.md author wrote.
        let body = body.strip_prefix('\n').unwrap_or(&body);

        // Resolve inline `` !`cmd` `` and fenced `` ```! `` blocks against
        // the principal's workspace before argument substitution runs.
        let body =
            preprocess::preprocess_dynamic_context(body, &frontmatter, &state.workspace).await?;

        let rendered = substitute_args(&body, &frontmatter.arguments, &args);

        Ok(json!({
            "name": frontmatter.name,
            "body": rendered,
        }))
    }
}

/// Substitute Claude-style placeholders in `body`:
/// 1. `\$` is preserved as a literal `$` (escape rule).
/// 2. `$name` — declared names from the frontmatter `arguments:` list.
///    Names map to positional indices by their order in the list.
/// 3. `$0`, `$1`, … `$N` — positional, 0-indexed. Substitution runs
///    high-to-low so `$10` isn't eaten by `$1`.
/// 4. `$ARGUMENTS` — full args joined with single spaces.
///
/// Undeclared `$name` placeholders are left unsubstituted verbatim.
fn substitute_args(body: &str, named: &[String], args: &[String]) -> String {
    // Escape pass: replace every \$ with a sentinel so the substitution
    // passes below don't touch it. Restore at the end.
    let mut out = body.replace("\\$", ESCAPE_SENTINEL);

    // Named placeholders. Iterate by index so we can map name → args[i].
    // When the named index is past args.len(), leave the placeholder
    // literal (Claude-compatible: the LLM sees the unsubstituted $name
    // and can re-invoke with the right arg).
    for (i, name) in named.iter().enumerate() {
        let placeholder = format!("${name}");
        if let Some(value) = args.get(i) {
            out = out.replace(&placeholder, value);
        }
    }

    // Positional placeholders ($0..$N where N is the highest index
    // referenced in the body). Iterate high-to-low so e.g. $10 isn't
    // matched greedily by $1. Positions beyond args.len() are left
    // literal (the LLM will see the unsubstituted placeholder).
    let max_positional = scan_max_positional_index(&out);
    for i in (0..=max_positional).rev() {
        // Replace `${i}` only when it is NOT followed by another digit —
        // otherwise `$1` would corrupt `$10`. The negative-lookahead
        // is implemented by walking the string and replacing exact matches.
        let needle = format!("${i}");
        let mut rebuilt = String::with_capacity(out.len());
        let mut cursor = 0;
        while cursor < out.len() {
            // Match `${i}` only if the placeholder fits AND is followed by
            // a non-digit (or end-of-string). This prevents `$1` from
            // matching the first two chars of `$10`.
            let end = cursor + needle.len();
            if end <= out.len() && out.get(cursor..end) == Some(needle.as_str()) {
                let next_is_digit = out[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit());
                if !next_is_digit {
                    if let Some(value) = args.get(i) {
                        rebuilt.push_str(value);
                    } else {
                        rebuilt.push_str(&needle);
                    }
                    cursor = end;
                    continue;
                }
            }
            // Skip one char (handles multi-byte UTF-8 in body content).
            let ch = out[cursor..].chars().next().unwrap();
            rebuilt.push(ch);
            cursor += ch.len_utf8();
        }
        out = rebuilt;
    }

    // $ARGUMENTS last so it never collides with positional or named.
    let joined = args.join(" ");
    out = out.replace("$ARGUMENTS", &joined);

    // Restore escaped dollars.
    out.replace(ESCAPE_SENTINEL, "$")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_no_args_no_placeholders() {
        assert_eq!(substitute_args("hello world", &[], &[]), "hello world");
    }

    #[test]
    fn substitute_arguments_full() {
        assert_eq!(
            substitute_args("Full: $ARGUMENTS", &[], &["a".into(), "b".into()]),
            "Full: a b"
        );
    }

    #[test]
    fn substitute_positional() {
        assert_eq!(
            substitute_args("Issue $0 on $1", &[], &["42".into(), "main".into()]),
            "Issue 42 on main"
        );
    }

    #[test]
    fn substitute_positional_high_to_low_avoids_greedy_match() {
        // $10 must not become "<value of $1>0". With args of length 11,
        // $10 should map to args[10] and $1 to args[1].
        let args: Vec<String> = (0..11).map(|i| format!("v{i}")).collect();
        let rendered = substitute_args("$10 then $1", &[], &args);
        assert_eq!(rendered, "v10 then v1");
        // When the user only passes 2 args, $10 should be left literal
        // (no positional at index 10) and $1 should map to args[1].
        let rendered = substitute_args("$10 then $1", &[], &["a".into(), "b".into()]);
        assert_eq!(rendered, "$10 then b");
    }

    #[test]
    fn substitute_named_by_position() {
        // `arguments: [issue, branch]` → $issue = args[0], $branch = args[1].
        let named = vec!["issue".to_string(), "branch".to_string()];
        let rendered = substitute_args(
            "Open $issue on $branch.",
            &named,
            &["42".into(), "main".into()],
        );
        assert_eq!(rendered, "Open 42 on main.");
    }

    #[test]
    fn substitute_undeclared_name_left_literal() {
        // `$issue` is not in the named list → left unsubstituted.
        // The body has only $issue and $0; named=["issue"] but with one
        // arg passed, so $issue maps to args[0] = "42".
        let named = vec!["issue".to_string()];
        let rendered = substitute_args("Issue $issue and again $issue", &named, &["42".into()]);
        assert_eq!(rendered, "Issue 42 and again 42");
    }

    #[test]
    fn substitute_named_position_out_of_range_leaves_literal() {
        // `arguments: [issue, branch]` says there are two named slots,
        // but the caller passes only one arg. `$branch` (named[1]) maps
        // to args[1] which doesn't exist → left literal. `$issue`
        // (named[0]) maps to args[0] = "42".
        let named = vec!["issue".to_string(), "branch".to_string()];
        let rendered = substitute_args("Open $issue on $branch.", &named, &["42".into()]);
        assert_eq!(rendered, "Open 42 on $branch.");
    }

    #[test]
    fn substitute_escape_preserves_literal_dollar() {
        // `\$1` is the literal string "$1" — no substitution.
        // `$0` does substitute because args has length 1.
        let rendered = substitute_args("Cost \\$1.00 or $0", &[], &["free".into()]);
        assert_eq!(rendered, "Cost $1.00 or free");
    }

    #[test]
    fn substitute_escape_preserves_named_and_arguments() {
        let named = vec!["price".to_string()];
        let rendered = substitute_args(
            "Cost \\$price, actual $price, full: $ARGUMENTS",
            &named,
            &["9.99".into()],
        );
        assert_eq!(rendered, "Cost $price, actual 9.99, full: 9.99");
    }

    #[test]
    fn substitute_missing_arg_leaves_named_literal() {
        // Only one arg provided but two named slots — the second
        // ($branch, named[1]) has no args[1] to map to, so it is
        // left literal (Claude-compatible).
        let named = vec!["issue".to_string(), "branch".to_string()];
        let rendered = substitute_args("Open $issue on $branch.", &named, &["42".into()]);
        assert_eq!(rendered, "Open 42 on $branch.");
    }

    // ---- SkillTool::execute tests (filesystem-backed) ----

    use crate::principal::{PrincipalId, SkillState, SkillStateRegistry};
    use crate::tools::ToolContext;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::TempDir;

    static TEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn next_test_pid() -> PrincipalId {
        PrincipalId(format!(
            "prin_test_{}",
            TEST_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn test_ctx(pid: &PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("hook_run", "hook", "Skill").with_principal_id(pid.0.clone())
    }

    async fn register_test_state(pid: &PrincipalId, allowlist: Vec<&str>, workspace: PathBuf) {
        let state = SkillState::new(allowlist.into_iter().map(String::from).collect(), workspace);
        SkillStateRegistry::global()
            .register(pid.clone(), state)
            .await;
    }

    async fn cleanup_test_state(pid: &PrincipalId) {
        SkillStateRegistry::global().unregister(pid).await;
    }

    fn write_skill(dir: &std::path::Path, name: &str, body: &str, args: &[&str]) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let args_yaml = if args.is_empty() {
            String::new()
        } else {
            let quoted: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
            format!("arguments: [{}]\n", quoted.join(", "))
        };
        let content =
            format!("---\nname: {name}\ndescription: A test skill\n{args_yaml}---\n\n{body}\n");
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    fn write_skill_with_frontmatter(
        dir: &std::path::Path,
        name: &str,
        frontmatter_extra: &str,
        body: &str,
    ) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let content = format!(
            "---\nname: {name}\ndescription: A test skill\n{frontmatter_extra}---\n\n{body}\n"
        );
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[tokio::test]
    async fn execute_returns_substituted_body() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        write_skill(
            tmp.path(),
            "fix",
            "Open issue $issue on branch $branch.\nFull: $ARGUMENTS",
            &["issue", "branch"],
        );
        register_test_state(&pid, vec!["fix"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(
                json!({
                    "name": "fix",
                    "args": ["42", "main"],
                }),
                &test_ctx(&pid),
            )
            .await
            .unwrap();
        assert_eq!(result["name"], "fix");
        assert_eq!(
            result["body"],
            "Open issue 42 on branch main.\nFull: 42 main"
        );
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_rejects_skill_not_in_allowlist() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        // Skill exists on disk but the allowlist doesn't include it.
        write_skill(tmp.path(), "fix", "body", &[]);
        register_test_state(&pid, vec!["other"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(json!({ "name": "fix" }), &test_ctx(&pid))
            .await
            .unwrap();
        assert_eq!(result["error"], "skill_not_enabled");
        assert_eq!(result["skill"], "fix");
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_rejects_unknown_skill() {
        // Allowlist is empty; even if a file existed, the name would
        // be rejected. We don't create any files on disk here to confirm
        // the allowlist check fires before any disk access.
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        register_test_state(&pid, vec![], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(json!({ "name": "docker" }), &test_ctx(&pid))
            .await
            .unwrap();
        assert_eq!(result["error"], "skill_not_enabled");
        assert_eq!(result["skill"], "docker");
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_rejects_missing_name_param() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        register_test_state(&pid, vec![], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("missing required parameter 'name'"));
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_handles_missing_arguments_frontmatter() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        // No `arguments:` frontmatter; body still uses $issue.
        write_skill(tmp.path(), "minimal", "Issue $issue", &[]);
        register_test_state(&pid, vec!["minimal"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(
                json!({
                    "name": "minimal",
                    "args": ["42"],
                }),
                &test_ctx(&pid),
            )
            .await
            .unwrap();
        // $issue is undeclared → left literal.
        assert_eq!(result["body"], "Issue $issue");
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_is_case_insensitive_on_allowlist() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "Docker", "Docker body", &[]);
        register_test_state(&pid, vec!["docker"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(json!({ "name": "Docker" }), &test_ctx(&pid))
            .await
            .unwrap();
        assert_eq!(result["name"], "Docker");
        assert_eq!(result["body"], "Docker body");
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_runs_dynamic_context_inline_form() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "live", "CWD: !`pwd`", &[]);
        register_test_state(&pid, vec!["live"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(json!({ "name": "live" }), &test_ctx(&pid))
            .await
            .unwrap();
        // `pwd` resolves to the workspace dir; the exact path varies by
        // platform, but the body should no longer contain the
        // `!`pwd`` placeholder.
        let body = result["body"].as_str().unwrap();
        assert!(
            !body.contains("!`pwd`"),
            "placeholder should be gone, got: {body}"
        );
        assert!(body.starts_with("CWD: "));
        assert!(body.len() > "CWD: ".len());
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_runs_dynamic_context_fenced_form() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        // Opener on its own line per the plan's exact-match rule.
        let body_str = "Intro\n```!\necho alpha\n```\nOutro";
        write_skill(tmp.path(), "fence", body_str, &[]);
        register_test_state(&pid, vec!["fence"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(json!({ "name": "fence" }), &test_ctx(&pid))
            .await
            .unwrap();
        let body = result["body"].as_str().unwrap();
        assert!(
            body.contains("alpha"),
            "fenced output should contain echo'd text, got: {body}"
        );
        assert!(!body.contains("```"), "fence markers should be gone");
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_blocks_command_not_in_allowed_tools() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        write_skill_with_frontmatter(
            tmp.path(),
            "guarded",
            "allowed-tools:\n  - \"echo *\"\n",
            "Got: !`ls /`",
        );
        register_test_state(&pid, vec!["guarded"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(json!({ "name": "guarded" }), &test_ctx(&pid))
            .await
            .unwrap();
        let body = result["body"].as_str().unwrap();
        // `ls /` is not in the allowlist → placeholder is left literal
        // and the blocked marker is prepended.
        assert!(body.contains("[shell blocked: command not in allowed-tools]"));
        assert!(body.contains("!`ls /`"));
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_allows_command_in_allowed_tools_glob() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        write_skill_with_frontmatter(
            tmp.path(),
            "guarded",
            "allowed-tools:\n  - \"echo *\"\n",
            "Got: !`echo hello`",
        );
        register_test_state(&pid, vec!["guarded"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(json!({ "name": "guarded" }), &test_ctx(&pid))
            .await
            .unwrap();
        assert_eq!(result["body"], "Got: hello\n");
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_fail_closed_without_principal_id() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "fix", "body", &[]);
        register_test_state(&pid, vec!["fix"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let ctx = ToolContext::for_hook_run("hook_run", "hook", "Skill");
        let result = tool
            .execute_with_context(json!({ "name": "fix" }), &ctx)
            .await
            .unwrap();
        assert_eq!(result["error"], "skill_not_enabled");
        cleanup_test_state(&pid).await;
    }

    #[tokio::test]
    async fn execute_fail_closed_when_principal_not_registered() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "fix", "body", &[]);
        // No state registered for `pid`.
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let result = tool
            .execute_with_context(json!({ "name": "fix" }), &test_ctx(&pid))
            .await
            .unwrap();
        assert_eq!(result["error"], "skill_not_enabled");
    }

    #[tokio::test]
    async fn execute_error_redacts_disk_path() {
        let pid = next_test_pid();
        let tmp = TempDir::new().unwrap();
        register_test_state(&pid, vec!["missing"], tmp.path().to_path_buf()).await;
        let tool = SkillTool::new().with_skills_dir_for_test(tmp.path().to_path_buf());
        let err = tool
            .execute_with_context(json!({ "name": "missing" }), &test_ctx(&pid))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("skill_unreadable"));
        assert!(msg.contains("SKILL.md for skill missing"));
        assert!(!msg.contains(tmp.path().to_string_lossy().as_ref()));
        cleanup_test_state(&pid).await;
    }
}
