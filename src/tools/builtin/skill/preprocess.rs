//! Dynamic context preprocessor for SKILL.md bodies.
//!
//! Resolves inline `` !`cmd` `` and fenced `` ```! `` blocks by running
//! the named commands and inlining their output. The pass runs once
//! over the body — substituted output is not re-scanned for further
//! placeholders. Argument substitution (`$ARGUMENTS`, `$0`..`$N`,
//! `$name`) runs *after* preprocessing, so the substituted output can
//! contain `$`-style placeholders that will be expanded in the
//! second pass.
//!
//! See [`preprocess_dynamic_context`] for the full algorithm and the
//! failure-handling rules.

use std::path::Path;

use anyhow::Result;
use glob::Pattern as GlobPattern;
use tokio::process::Command;

use crate::extensions::skill::SkillFrontmatter;

/// Per-call timeout for injected commands (ms). Shorter than
/// `BashTool`'s no-default because these land inline in the body,
/// which the LLM has to read in full.
const SHELL_TIMEOUT_MS: u64 = 5_000;

/// Per-stream cap (stdout / stderr) for a single injected command.
const MAX_OUTPUT_BYTES: usize = 30_000;

/// Result of running an injected command.
struct ShellOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
}

/// Run a shell command, blocking, with a per-call timeout and
/// per-stream output cap. The command runs in `working_dir`.
///
/// On timeout, the child process is killed (via tokio's process drop
/// semantics); no partial stdout is captured. The caller surfaces
/// this via a `command timed out after N ms` stderr line.
async fn run_shell_blocking(
    command: &str,
    working_dir: &Path,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> Result<ShellOutput> {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    cmd.current_dir(working_dir);

    let output = match tokio::time::timeout(
        tokio::time::Duration::from_millis(timeout_ms),
        cmd.output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(anyhow::anyhow!("failed to execute shell: {e}")),
        Err(_) => {
            return Ok(ShellOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: -1,
                timed_out: true,
            });
        }
    };

    let stdout = truncate_bytes(&output.stdout, max_output_bytes);
    let stderr = truncate_bytes(&output.stderr, max_output_bytes);
    let exit_code = output.status.code().unwrap_or(-1);

    Ok(ShellOutput {
        stdout,
        stderr,
        exit_code,
        timed_out: false,
    })
}

/// Truncate `bytes` to at most `max` bytes, then append `...(truncated)`.
/// Always cuts on a UTF-8 char boundary so the result is valid UTF-8.
fn truncate_bytes(bytes: &[u8], max: usize) -> String {
    if bytes.len() <= max {
        return String::from_utf8_lossy(bytes).to_string();
    }
    let end = (0..=max)
        .rev()
        .find(|&i| std::str::from_utf8(&bytes[..i]).is_ok())
        .unwrap_or(0);
    let mut s = String::from_utf8_lossy(&bytes[..end]).to_string();
    s.push_str("...(truncated)");
    s
}

/// Format a `ShellOutput` for inline substitution. See module docs.
///
/// When a stderr line is appended, at most one trailing newline is
/// stripped from `stdout` so the separator newline doesn't produce a
/// blank line in the rendered body. Inner newlines are preserved.
fn format_output(out: &ShellOutput, timeout_ms: u64) -> String {
    let stderr_line = if out.timed_out {
        format!("command timed out after {timeout_ms} ms")
    } else if out.exit_code != 0 {
        let trimmed = out.stderr.trim();
        if !trimmed.is_empty() {
            trimmed.to_string()
        } else {
            format!("exit code {}", out.exit_code)
        }
    } else {
        String::new()
    };

    if stderr_line.is_empty() {
        out.stdout.clone()
    } else if out.stdout.is_empty() {
        format!("stderr: {stderr_line}")
    } else {
        let stdout = out.stdout.strip_suffix('\n').unwrap_or(&out.stdout);
        format!("{stdout}\nstderr: {stderr_line}")
    }
}

/// True when `command` matches at least one glob in the
/// `allowed_tools` allowlist. Empty list = all commands allowed.
fn is_command_allowed(frontmatter: &SkillFrontmatter, command: &str) -> bool {
    if frontmatter.allowed_tools.is_empty() {
        return true;
    }
    let trimmed = command.trim();
    frontmatter.allowed_tools.iter().any(|pattern| {
        GlobPattern::new(pattern).is_ok_and(|p| p.matches(trimmed))
    })
}

/// Resolve inline `` !`cmd` `` and fenced `` ```! `` blocks in
/// `body` by running the named commands in `workspace_dir` and
/// inlining their output. Single pass — substituted output is not
/// re-scanned for further placeholders.
///
/// The preprocessor runs in two phases for clarity:
/// 1. Fenced blocks first (multi-line commands).
/// 2. Inline blocks second (single-line commands).
///
/// Phases are independent; the output of phase 1 is the input of
/// phase 2. Neither output is re-scanned.
///
/// ## Failure handling
/// - Command exits 0: stdout inlined verbatim.
/// - Command exits non-zero: stdout inlined verbatim, then a
///   `stderr: <stderr>` line (or `stderr: exit code N` if stderr is
///   empty) is appended.
/// - Command times out: empty body + `stderr: command timed out
///   after 5000 ms`.
/// - Command not in `allowed-tools` glob allowlist (when the list is
///   non-empty): placeholder is left literal and a
///   `[shell blocked: command not in allowed-tools]` marker is
///   prepended.
/// - `shell:` frontmatter set to anything other than `"bash"`: same
///   treatment, with a shell-specific marker.
pub(crate) async fn preprocess_dynamic_context(
    body: &str,
    frontmatter: &SkillFrontmatter,
    workspace_dir: &Path,
) -> Result<String> {
    let after_fenced = resolve_fenced(body, frontmatter, workspace_dir).await?;
    resolve_inline(&after_fenced, frontmatter, workspace_dir).await
}

/// Resolve fenced `` ```! `` … `` ``` `` blocks in `body`. Each block
/// is replaced with the output of running its (multi-line) command.
///
/// A line equals the opener iff `line.trim_end() == "```!"` (no
/// leading whitespace allowed; trailing whitespace OK). The closer is
/// `line.trim_end() == "```"`. An unclosed fence is left literal.
async fn resolve_fenced(
    body: &str,
    frontmatter: &SkillFrontmatter,
    workspace_dir: &Path,
) -> Result<String> {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut out_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut fence_buffer: Vec<String> = Vec::new();

    for line in lines {
        let trimmed = line.trim_end();

        if fence_buffer.is_empty() && trimmed == "```!" {
            // Opener. Start collecting the command.
            fence_buffer.push(line.to_string());
            continue;
        }

        if !fence_buffer.is_empty() {
            if trimmed == "```" {
                // Closer. The first buffer element is the opener
                // line; the rest is the command (possibly empty).
                let command = if fence_buffer.len() > 1 {
                    fence_buffer[1..].join("\n")
                } else {
                    String::new()
                };
                let closer_line = line.to_string();
                let result = run_or_blocked_fenced(
                    &command,
                    frontmatter,
                    workspace_dir,
                    &fence_buffer,
                    &closer_line,
                )
                .await?;
                out_lines.push(result);
                fence_buffer.clear();
                continue;
            }
            // Inside the fence — accumulate the command line.
            fence_buffer.push(line.to_string());
            continue;
        }

        // Outside any fence — copy verbatim.
        out_lines.push(line.to_string());
    }

    // Unclosed fence: emit the buffered lines literally so the
    // author can see what was left unprocessed.
    if !fence_buffer.is_empty() {
        for line in &fence_buffer {
            out_lines.push(line.clone());
        }
    }

    Ok(out_lines.join("\n"))
}

/// Run the command in `command` (the body of a fenced block) and
/// return the inline form. When blocked (shell / allowed-tools), the
/// output is `[shell blocked: ...]` followed by the original fence
/// (opener + command lines + closer) so the LLM sees the full context.
async fn run_or_blocked_fenced(
    command: &str,
    frontmatter: &SkillFrontmatter,
    workspace_dir: &Path,
    fence_buffer: &[String],
    closer_line: &str,
) -> Result<String> {
    if let Some(shell) = frontmatter.shell.as_deref() {
        if shell != "bash" {
            let marker = format!(
                "[shell blocked: shell \"{shell}\" not supported, only \"bash\"]"
            );
            let original = fence_buffer.join("\n") + "\n" + closer_line;
            return Ok(format!("{marker}{original}"));
        }
    }
    if !is_command_allowed(frontmatter, command) {
        let marker = "[shell blocked: command not in allowed-tools]";
        let original = fence_buffer.join("\n") + "\n" + closer_line;
        return Ok(format!("{marker}{original}"));
    }
    let out = run_shell_blocking(command, workspace_dir, SHELL_TIMEOUT_MS, MAX_OUTPUT_BYTES).await?;
    Ok(format_output(&out, SHELL_TIMEOUT_MS))
}

/// Resolve inline `` !`cmd` `` placeholders. A `!` is a candidate
/// only if it's at byte offset 0 of `body` or preceded by ASCII
/// whitespace. The placeholder is replaced by the command's output.
async fn resolve_inline(
    body: &str,
    frontmatter: &SkillFrontmatter,
    workspace_dir: &Path,
) -> Result<String> {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;

    while i < bytes.len() {
        let ch = body[i..].chars().next().unwrap();
        let ch_len = ch.len_utf8();

        let anchor_ok = ch == '!'
            && (i == 0 || bytes[i - 1].is_ascii_whitespace());

        if anchor_ok {
            let after_bang = &body[i + ch_len..];
            if let Some((cmd, end_offset)) = parse_inline_command(after_bang) {
                // The full placeholder is `!` + `` `cmd` `` =
                // `ch_len` (1) + `end_offset` bytes.
                let placeholder_len = ch_len + end_offset;
                let placeholder = &body[i..i + placeholder_len];

                // Shell selector: only "bash" is supported today.
                if let Some(shell) = frontmatter.shell.as_deref() {
                    if shell != "bash" {
                        let marker = format!(
                            "[shell blocked: shell \"{shell}\" not supported, only \"bash\"]"
                        );
                        out.push_str(&marker);
                        out.push_str(placeholder);
                        i += placeholder_len;
                        continue;
                    }
                }

                // Allowed-tools glob allowlist.
                if !is_command_allowed(frontmatter, &cmd) {
                    let marker = "[shell blocked: command not in allowed-tools]";
                    out.push_str(marker);
                    out.push_str(placeholder);
                    i += placeholder_len;
                    continue;
                }

                let out_run = run_shell_blocking(
                    &cmd,
                    workspace_dir,
                    SHELL_TIMEOUT_MS,
                    MAX_OUTPUT_BYTES,
                )
                .await?;
                let formatted = format_output(&out_run, SHELL_TIMEOUT_MS);
                out.push_str(&formatted);
                i += placeholder_len;
                continue;
            }
            // No closing backtick → the `!` is literal.
        }

        out.push(ch);
        i += ch_len;
    }

    Ok(out)
}

/// Parse `` `cmd` `` from the start of `s`. Returns the command
/// (between the backticks) and the total bytes consumed (including
/// both backticks).
///
/// Inline commands may not contain newlines. A trailing backslash
/// escapes the next character for the *parser* (so `\`` is preserved
/// in the command as `\`` — the backslash is sent to the shell which
/// decides what to do with it).
fn parse_inline_command(s: &str) -> Option<(String, usize)> {
    if !s.starts_with('`') {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 1; // skip opening backtick
    let mut cmd = String::new();
    let mut escaped = false;

    while i < bytes.len() {
        let ch = s[i..].chars().next()?;
        let ch_len = ch.len_utf8();

        if escaped {
            cmd.push(ch);
            escaped = false;
            i += ch_len;
            continue;
        }
        if ch == '\\' {
            cmd.push(ch);
            escaped = true;
            i += ch_len;
            continue;
        }
        if ch == '`' {
            return Some((cmd, i + ch_len));
        }
        if ch == '\n' {
            // Inline form is single-line by design.
            return None;
        }
        cmd.push(ch);
        i += ch_len;
    }
    // No closing backtick.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::skill::SkillFrontmatter;
    use std::path::PathBuf;

    fn empty_frontmatter() -> SkillFrontmatter {
        SkillFrontmatter {
            name: "test".to_string(),
            description: "test".to_string(),
            tags: vec![],
            author: None,
            arguments: vec![],
            shell: None,
            allowed_tools: vec![],
        }
    }

    fn tmp_workspace() -> TempDir {
        TempDir::new().unwrap()
    }

    use tempfile::TempDir;

    // ---- truncate_bytes ----

    #[test]
    fn truncate_bytes_under_limit_unchanged() {
        let s = truncate_bytes(b"hello", 100);
        assert_eq!(s, "hello");
    }

    #[test]
    fn truncate_bytes_at_limit_cuts_and_marks() {
        let s = truncate_bytes(b"hello world", 5);
        assert_eq!(s, "hello...(truncated)");
    }

    #[test]
    fn truncate_bytes_cuts_on_utf8_boundary() {
        // "héllo" is 6 bytes (é = 2 bytes). With max=3 the largest
        // valid char boundary ≤ 3 is byte 3 (after 'é'). Cutting
        // mid-é would corrupt the UTF-8 — the implementation must
        // back off to byte 3.
        let s = truncate_bytes("héllo".as_bytes(), 3);
        assert_eq!(s, "hé...(truncated)");
        // And with max=2 (mid-é), the implementation must back off
        // to byte 1 ('h' only).
        let s = truncate_bytes("héllo".as_bytes(), 2);
        assert_eq!(s, "h...(truncated)");
    }

    // ---- format_output ----

    #[test]
    fn format_output_success_returns_stdout() {
        let out = ShellOutput {
            stdout: "hi\n".to_string(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
        };
        assert_eq!(format_output(&out, 5000), "hi\n");
    }

    #[test]
    fn format_output_nonzero_appends_stderr() {
        let out = ShellOutput {
            stdout: "partial".to_string(),
            stderr: "boom\n".to_string(),
            exit_code: 1,
            timed_out: false,
        };
        assert_eq!(format_output(&out, 5000), "partial\nstderr: boom");
    }

    #[test]
    fn format_output_nonzero_empty_stderr_uses_exit_code() {
        let out = ShellOutput {
            stdout: "partial".to_string(),
            stderr: String::new(),
            exit_code: 42,
            timed_out: false,
        };
        assert_eq!(format_output(&out, 5000), "partial\nstderr: exit code 42");
    }

    #[test]
    fn format_output_timeout_no_stdout() {
        let out = ShellOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: -1,
            timed_out: true,
        };
        assert_eq!(format_output(&out, 5000), "stderr: command timed out after 5000 ms");
    }

    // ---- is_command_allowed ----

    #[test]
    fn is_command_allowed_empty_list_allows_all() {
        let fm = empty_frontmatter();
        assert!(is_command_allowed(&fm, "rm -rf /"));
    }

    #[test]
    fn is_command_allowed_glob_match() {
        let mut fm = empty_frontmatter();
        fm.allowed_tools = vec!["git *".to_string(), "ls *".to_string()];
        assert!(is_command_allowed(&fm, "git status"));
        assert!(is_command_allowed(&fm, "ls /tmp"));
        assert!(!is_command_allowed(&fm, "rm -rf /"));
    }

    #[test]
    fn is_command_allowed_anchored_match() {
        let mut fm = empty_frontmatter();
        fm.allowed_tools = vec!["pwd".to_string()];
        assert!(is_command_allowed(&fm, "pwd"));
        // "pwd;" should NOT match a `pwd` pattern (anchored).
        assert!(!is_command_allowed(&fm, "pwd; rm -rf /"));
    }

    // ---- parse_inline_command ----

    #[test]
    fn parse_inline_command_basic() {
        let (cmd, end) = parse_inline_command("`echo hi`rest").unwrap();
        assert_eq!(cmd, "echo hi");
        assert_eq!(end, "`echo hi`".len());
    }

    #[test]
    fn parse_inline_command_no_opening_backtick() {
        assert!(parse_inline_command("echo hi`").is_none());
    }

    #[test]
    fn parse_inline_command_no_closing_backtick() {
        assert!(parse_inline_command("`echo hi").is_none());
    }

    #[test]
    fn parse_inline_command_newline_is_rejected() {
        assert!(parse_inline_command("`echo\nhi`").is_none());
    }

    #[test]
    fn parse_inline_command_escaped_backtick_preserved() {
        // `echo \`hi\`` — backslashes are preserved; the escaped
        // backticks are part of the command string.
        let (cmd, _) = parse_inline_command("`echo \\`hi\\``").unwrap();
        assert_eq!(cmd, "echo \\`hi\\`");
    }

    #[test]
    fn parse_inline_command_empty() {
        let (cmd, end) = parse_inline_command("``").unwrap();
        assert_eq!(cmd, "");
        assert_eq!(end, 2);
    }

    // ---- preprocess_dynamic_context: inline form ----

    #[tokio::test]
    async fn preprocess_inline_basic_substitution() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "CWD: !`pwd`";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(!out.contains("!`pwd`"));
        assert!(out.starts_with("CWD: "));
    }

    #[tokio::test]
    async fn preprocess_inline_preserves_surrounding_text() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "before\n!`echo middle`\nafter";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert_eq!(out, "before\nmiddle\n\nafter");
    }

    #[tokio::test]
    async fn preprocess_inline_anchor_rule_blocks_equals_prefix() {
        // `KEY=!`echo hi`` — the `!` is preceded by `=`, so it must
        // be left literal; the command must not run.
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "KEY=!`echo hi`";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert_eq!(out, "KEY=!`echo hi`");
    }

    #[tokio::test]
    async fn preprocess_inline_anchor_rule_at_start_of_line() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "!`echo start` rest";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("start"), "got: {out}");
        assert!(!out.contains("!`"));
    }

    #[tokio::test]
    async fn preprocess_inline_anchor_rule_after_whitespace() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "word !`echo afterspace` end";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("afterspace"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_inline_anchor_rule_after_tab() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "word\t!`echo aftertab` end";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("aftertab"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_inline_unclosed_backtick_left_literal() {
        // `!`echo hi` (no closing backtick) — leave literal.
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        // Body has an opening backtick after `!` but no closing one.
        let body = "no close: !`echo hi";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        // The unclosed `!` is left as a literal; the rest of the body
        // is unchanged.
        assert!(out.contains("!`echo hi"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_inline_failure_appends_stderr() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "result: !`sh -c 'echo out; echo bad 1>&2; exit 7'`";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        // stdout inlined verbatim, then stderr line.
        assert!(out.starts_with("result: out\nstderr: bad"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_inline_command_not_in_allowlist_blocked() {
        let mut fm = empty_frontmatter();
        fm.allowed_tools = vec!["echo *".to_string()];
        let ws = tmp_workspace();
        let body = "Got: !`ls /`";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("[shell blocked: command not in allowed-tools]"));
        assert!(out.contains("!`ls /`"));
    }

    #[tokio::test]
    async fn preprocess_inline_command_in_allowlist_runs() {
        let mut fm = empty_frontmatter();
        fm.allowed_tools = vec!["echo *".to_string()];
        let ws = tmp_workspace();
        let body = "Got: !`echo hello`";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert_eq!(out, "Got: hello\n");
    }

    #[tokio::test]
    async fn preprocess_inline_unknown_shell_blocked() {
        let mut fm = empty_frontmatter();
        fm.shell = Some("powershell".to_string());
        let ws = tmp_workspace();
        let body = "Got: !`echo hi`";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("[shell blocked: shell \"powershell\" not supported"));
        assert!(out.contains("!`echo hi`"));
    }

    #[tokio::test]
    async fn preprocess_inline_bash_shell_explicit_runs() {
        let mut fm = empty_frontmatter();
        fm.shell = Some("bash".to_string());
        let ws = tmp_workspace();
        let body = "Got: !`echo hi`";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("hi"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_inline_single_pass_output_not_rescanned() {
        // A command whose stdout contains a `!`cmd`` placeholder —
        // that text must NOT be re-scanned. The shell receives
        // `echo '!\`printf INNER\`'` and prints `!\`printf INNER\``
        // literally (single quotes preserve the backslashes).
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "!`echo '!\\`printf INNER\\`'`";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        // The inner placeholder syntax survives in the output —
        // proof the preprocessor did not re-scan. If it had, the
        // inner would have been replaced with the result of
        // `printf INNER` (which is "INNER" with no newline).
        // `echo` appends a trailing newline, hence the `\n`.
        assert_eq!(out, "!\\`printf INNER\\`\n");
        // And the inner result is NOT a standalone "INNER" string.
        assert_ne!(out.trim(), "INNER");
    }

    // ---- preprocess_dynamic_context: fenced form ----

    #[tokio::test]
    async fn preprocess_fenced_basic_substitution() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        // The opener is on its own line per the plan's exact-match rule.
        let body = "Intro\n```!\necho alpha\n```\nOutro";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("alpha"), "got: {out}");
        assert!(!out.contains("```"), "fence markers should be gone, got: {out}");
    }

    #[tokio::test]
    async fn preprocess_fenced_multiline_command() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "```!\nprintf 'one\\ntwo'\n```";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("one"), "got: {out}");
        assert!(out.contains("two"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_fenced_unclosed_left_literal() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "before\n```!\necho hi\nstill no closer";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        // Unclosed fence: opener and command lines are preserved
        // literally.
        assert!(out.contains("```!"));
        assert!(out.contains("echo hi"));
    }

    #[tokio::test]
    async fn preprocess_fenced_blocked_by_allowed_tools() {
        let mut fm = empty_frontmatter();
        fm.allowed_tools = vec!["echo *".to_string()];
        let ws = tmp_workspace();
        let body = "Intro\n```!\nls /\n```\nOutro";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(
            out.contains("[shell blocked: command not in allowed-tools]"),
            "got: {out}"
        );
        // The whole fence is left literal so the LLM sees what was blocked.
        assert!(out.contains("```!"));
        assert!(out.contains("ls /"));
        assert!(out.contains("Intro"));
        assert!(out.contains("Outro"));
    }

    #[tokio::test]
    async fn preprocess_fenced_failure_appends_stderr() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "```!\nsh -c 'echo out; echo bad 1>&2; exit 3'\n```";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("out"));
        assert!(out.contains("stderr: bad"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_does_not_touch_regular_code_fences() {
        // A normal code fence like ```bash should not trigger
        // dynamic-context resolution.
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "Intro\n```bash\necho should-not-run\n```\nOutro";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.contains("echo should-not-run"));
        assert!(out.contains("```bash"));
        assert!(out.contains("```"));
    }

    #[tokio::test]
    async fn preprocess_inline_works_inside_text_with_other_content() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "Header\n\n!`echo middle`\n\nFooter";
        let out =
            preprocess_dynamic_context(body, &fm, ws.path()).await.unwrap();
        assert!(out.starts_with("Header\n\nmiddle"), "got: {out}");
        assert!(out.ends_with("Footer"), "got: {out}");
    }

    // ---- run_shell_blocking smoke test (uses real shell) ----

    #[tokio::test]
    async fn run_shell_blocking_echo() {
        let ws = tmp_workspace();
        let out = run_shell_blocking("echo hi", ws.path(), 5_000, 100).await.unwrap();
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.exit_code, 0);
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn run_shell_blocking_nonzero_captures_stderr() {
        let ws = tmp_workspace();
        let out = run_shell_blocking("sh -c 'echo err 1>&2; exit 9'", ws.path(), 5_000, 100)
            .await
            .unwrap();
        assert_eq!(out.exit_code, 9);
        assert!(out.stderr.contains("err"));
    }

    #[tokio::test]
    async fn run_shell_blocking_runs_in_workspace() {
        let ws = tmp_workspace();
        // `pwd` should resolve to the workspace path.
        let out = run_shell_blocking("pwd", ws.path(), 5_000, 100).await.unwrap();
        let pwd = out.stdout.trim();
        // Resolve symlinks / canonicalize to handle macOS /private/var
        // vs /var, /tmp vs /private/tmp.
        let canonical_ws = ws.path().canonicalize().unwrap_or_else(|_| ws.path().to_path_buf());
        let canonical_pwd = PathBuf::from(pwd).canonicalize().unwrap_or_else(|_| PathBuf::from(pwd));
        assert_eq!(canonical_pwd, canonical_ws);
    }
}
