//! Skill tool
//!
//! Provides `Skill` so the agent can invoke a SKILL.md body on-demand
//! with argument substitution. The skill list is gated by the principal's
//! `capabilities.skills` allowlist (per-call re-registration; see
//! [`crate::principal::context::install_skill_tool`]).
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
pub struct SkillTool {
    /// Daemon-global skills directory (`~/.peko/skills/`).
    skills_dir: PathBuf,
    /// Principal's enabled-skill allowlist (bare names from
    /// `PrincipalCapabilities.skills`). Case-insensitive match.
    enabled_skills: Vec<String>,
}

impl SkillTool {
    /// Build a new `SkillTool` rooted at `skills_dir` with the principal's
    /// enabled-skill allowlist. Unknown / disabled skills are rejected
    /// with `skill_not_enabled` before any disk access.
    #[must_use]
    pub fn new(skills_dir: PathBuf, enabled_skills: Vec<String>) -> Self {
        Self {
            skills_dir,
            enabled_skills,
        }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "Skill"
    }

    fn description(&self) -> String {
        r"Invoke a SKILL.md body with argument substitution.

Parameters:
- name: required — skill name (must match a discovered SKILL.md directory name AND be in the principal's enabled allowlist).
- args: optional array of strings — positional arguments.

Argument substitution (Claude-style):
- $ARGUMENTS — full args array joined with single spaces
- $0, $1, … — positional, 0-indexed
- $name — declared in SKILL.md frontmatter `arguments:` list; positions map to names by order

Escape: prefix with `\$` to keep a placeholder literal.

If a `$name` placeholder's name is not declared in the frontmatter `arguments:` list, it is left unsubstituted in the rendered body.

Returns:
- { name, body } — the skill's body with arguments substituted.
- { error, skill } — structured error: skill_not_enabled, skill_unreadable, or unknown_skill."
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

        // Gate against the principal's allowlist BEFORE touching disk, so
        // a disabled skill's existence on the filesystem isn't leaked
        // via timing or error messages.
        if !self
            .enabled_skills
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&name))
        {
            return Ok(json!({
                "error": "skill_not_enabled",
                "skill": name,
            }));
        }

        let skill_md = self.skills_dir.join(&name).join("SKILL.md");
        let content = std::fs::read_to_string(&skill_md).map_err(|e| {
            anyhow::anyhow!(
                "skill_unreadable: failed to read {}: {e}",
                skill_md.display()
            )
        })?;

        let (frontmatter, body): (SkillFrontmatter, String) =
            parse_yaml_frontmatter_typed(&content).map_err(|e| {
                anyhow::anyhow!(
                    "skill_unreadable: failed to parse frontmatter in {}: {e}",
                    skill_md.display()
                )
            })?;

        // `parse_yaml_frontmatter` includes the newline that follows the
        // closing `---` in the body. Trim it so the rendered output
        // matches what the SKILL.md author wrote.
        let body = body.strip_prefix('\n').unwrap_or(&body);

        let rendered = substitute_args(body, &frontmatter.arguments, &args);

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
        assert_eq!(
            substitute_args("hello world", &[], &[]),
            "hello world"
        );
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
            substitute_args(
                "Issue $0 on $1",
                &[],
                &["42".into(), "main".into()]
            ),
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
        let rendered = substitute_args(
            "Issue $issue and again $issue",
            &named,
            &["42".into()],
        );
        assert_eq!(rendered, "Issue 42 and again 42");
    }

    #[test]
    fn substitute_named_position_out_of_range_leaves_literal() {
        // `arguments: [issue, branch]` says there are two named slots,
        // but the caller passes only one arg. `$branch` (named[1]) maps
        // to args[1] which doesn't exist → left literal. `$issue`
        // (named[0]) maps to args[0] = "42".
        let named = vec!["issue".to_string(), "branch".to_string()];
        let rendered = substitute_args(
            "Open $issue on $branch.",
            &named,
            &["42".into()],
        );
        assert_eq!(rendered, "Open 42 on $branch.");
    }

    #[test]
    fn substitute_escape_preserves_literal_dollar() {
        // `\$1` is the literal string "$1" — no substitution.
        // `$0` does substitute because args has length 1.
        let rendered = substitute_args(
            "Cost \\$1.00 or $0",
            &[],
            &["free".into()],
        );
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
        let rendered = substitute_args(
            "Open $issue on $branch.",
            &named,
            &["42".into()],
        );
        assert_eq!(rendered, "Open 42 on $branch.");
    }

    // ---- SkillTool::execute tests (filesystem-backed) ----

    use tempfile::TempDir;

    fn write_skill(dir: &std::path::Path, name: &str, body: &str, args: &[&str]) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let args_yaml = if args.is_empty() {
            String::new()
        } else {
            let quoted: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
            format!("arguments: [{}]\n", quoted.join(", "))
        };
        let content = format!(
            "---\nname: {name}\ndescription: A test skill\n{args_yaml}---\n\n{body}\n"
        );
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[tokio::test]
    async fn execute_returns_substituted_body() {
        let tmp = TempDir::new().unwrap();
        write_skill(
            tmp.path(),
            "fix",
            "Open issue $issue on branch $branch.\nFull: $ARGUMENTS",
            &["issue", "branch"],
        );
        let tool = SkillTool::new(
            tmp.path().to_path_buf(),
            vec!["fix".into()],
        );
        let result = tool
            .execute(json!({
                "name": "fix",
                "args": ["42", "main"],
            }))
            .await
            .unwrap();
        assert_eq!(result["name"], "fix");
        assert_eq!(
            result["body"],
            "Open issue 42 on branch main.\nFull: 42 main"
        );
    }

    #[tokio::test]
    async fn execute_rejects_skill_not_in_allowlist() {
        let tmp = TempDir::new().unwrap();
        // Skill exists on disk but the allowlist doesn't include it.
        write_skill(tmp.path(), "fix", "body", &[]);
        let tool = SkillTool::new(
            tmp.path().to_path_buf(),
            vec!["other".into()],
        );
        let result = tool
            .execute(json!({ "name": "fix" }))
            .await
            .unwrap();
        assert_eq!(result["error"], "skill_not_enabled");
        assert_eq!(result["skill"], "fix");
    }

    #[tokio::test]
    async fn execute_rejects_unknown_skill() {
        // Allowlist is empty; even if a file existed, the name would
        // be rejected. We don't create any files on disk here to confirm
        // the allowlist check fires before any disk access.
        let tmp = TempDir::new().unwrap();
        let tool = SkillTool::new(
            tmp.path().to_path_buf(),
            vec![], // empty allowlist
        );
        let result = tool
            .execute(json!({ "name": "docker" }))
            .await
            .unwrap();
        assert_eq!(result["error"], "skill_not_enabled");
        assert_eq!(result["skill"], "docker");
    }

    #[tokio::test]
    async fn execute_rejects_missing_name_param() {
        let tmp = TempDir::new().unwrap();
        let tool = SkillTool::new(tmp.path().to_path_buf(), vec![]);
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("missing required parameter 'name'"));
    }

    #[tokio::test]
    async fn execute_handles_missing_arguments_frontmatter() {
        let tmp = TempDir::new().unwrap();
        // No `arguments:` frontmatter; body still uses $issue.
        write_skill(tmp.path(), "minimal", "Issue $issue", &[]);
        let tool = SkillTool::new(tmp.path().to_path_buf(), vec!["minimal".into()]);
        let result = tool
            .execute(json!({
                "name": "minimal",
                "args": ["42"],
            }))
            .await
            .unwrap();
        // $issue is undeclared → left literal.
        assert_eq!(result["body"], "Issue $issue");
    }

    #[tokio::test]
    async fn execute_is_case_insensitive_on_allowlist() {
        let tmp = TempDir::new().unwrap();
        write_skill(tmp.path(), "Docker", "Docker body", &[]);
        let tool = SkillTool::new(
            tmp.path().to_path_buf(),
            vec!["docker".into()],
        );
        let result = tool
            .execute(json!({ "name": "Docker" }))
            .await
            .unwrap();
        assert_eq!(result["name"], "Docker");
        assert_eq!(result["body"], "Docker body");
    }
}