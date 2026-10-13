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
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::{append_skill_text, MAX_SKILL_BODY_BYTES};
use crate::tools::builtin::skill::frontmatter::SkillFrontmatter;

/// Per-call timeout for injected commands (ms). Shorter than
/// `BashTool`'s no-default because these land inline in the body,
/// which the LLM has to read in full.
pub const SHELL_TIMEOUT_MS: u64 = 5_000;

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
/// On timeout, the command and everything it started are killed; no
/// partial stdout is captured. The caller surfaces this via a `command
/// timed out after N ms` stderr line.
async fn run_shell_blocking(
    command: &str,
    working_dir: &Path,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> Result<ShellOutput> {
    let mut cmd = shell_command(command)?;
    cmd.current_dir(working_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (mut child, tree) = crate::tools::builtin::process::spawn_in_own_group(&mut cmd)
        .map_err(|e| anyhow::anyhow!("failed to execute shell: {e}"))?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let output = match tokio::time::timeout(tokio::time::Duration::from_millis(timeout_ms), async {
        tokio::try_join!(
            child.wait(),
            read_capped_stream(stdout, max_output_bytes),
            read_capped_stream(stderr, max_output_bytes),
        )
    })
    .await
    {
        Ok(Ok(o)) => {
            tree.disarm();
            o
        }
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

    let (status, stdout, stderr) = output;
    let stdout = normalize_crlf(&stdout);
    let stderr = normalize_crlf(&stderr);
    let exit_code = status.code().unwrap_or(-1);

    Ok(ShellOutput {
        stdout,
        stderr,
        exit_code,
        timed_out: false,
    })
}

/// Drain both pipes concurrently to avoid deadlock, retaining only a bounded
/// prefix. The timeout still applies to a command producing endless output.
async fn read_capped_stream(
    mut stream: impl AsyncRead + Unpin,
    max: usize,
) -> std::io::Result<String> {
    let mut prefix = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let retained = count.min((max + 1).saturating_sub(prefix.len()));
        prefix.extend_from_slice(&buffer[..retained]);
    }
    Ok(truncate_bytes(&prefix, max))
}

/// Build a platform-appropriate shell invocation for `command`.
fn shell_command(command: &str) -> Result<Command> {
    #[cfg(windows)]
    let executable = windows_bash_path()?;
    #[cfg(not(windows))]
    let executable = std::path::PathBuf::from("bash");
    let mut cmd = Command::new(executable);
    cmd.arg("-c").arg(command);
    Ok(cmd)
}

/// Windows' System32 bash.exe launches WSL rather than a native shell. It
/// cannot honor native workspace paths and may have no distribution installed.
#[cfg(any(windows, test))]
fn is_wsl_launcher(path: &Path) -> bool {
    let path = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    [
        "/system32/bash.exe",
        "/syswow64/bash.exe",
        "/sysnative/bash.exe",
        "/windowsapps/bash.exe",
    ]
    .iter()
    .any(|suffix| path.ends_with(suffix))
}

#[cfg(any(windows, test))]
fn select_native_bash(
    path_candidates: impl IntoIterator<Item = std::path::PathBuf>,
    git_roots: impl IntoIterator<Item = std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    path_candidates
        .into_iter()
        .chain(
            git_roots
                .into_iter()
                .flat_map(|root| [root.join("bin/bash.exe"), root.join("usr/bin/bash.exe")]),
        )
        .find(|path| !is_wsl_launcher(path) && path.is_file())
}

#[cfg(any(windows, test))]
fn windows_bash_path() -> Result<std::path::PathBuf> {
    let path_candidates: Vec<_> = which::which_all("bash")
        .map(Iterator::collect)
        .unwrap_or_default();
    // Git commonly exposes only its cmd/ directory in PATH. Resolve its
    // installation root from git.exe, including mingw64/bin installations.
    let mut git_roots: Vec<_> = which::which_all("git")
        .into_iter()
        .flatten()
        .flat_map(|path| {
            path.ancestors()
                .skip(2)
                .take(3)
                .map(Path::to_path_buf)
                .collect::<Vec<_>>()
        })
        .collect();
    for (variable, suffix) in [
        ("ProgramFiles", "Git"),
        ("ProgramFiles(x86)", "Git"),
        ("LOCALAPPDATA", "Programs/Git"),
    ] {
        if let Some(root) = std::env::var_os(variable) {
            git_roots.push(std::path::PathBuf::from(root).join(suffix));
        }
    }
    select_native_bash(path_candidates, git_roots).ok_or_else(|| anyhow::anyhow!(
        "Skill dynamic context requires native Bash (for example Git for Windows); the WSL bash.exe launcher is not supported"
    ))
}

/// Normalize Windows CRLF line endings to LF so the preprocessor output
/// is platform-agnostic text.
fn normalize_crlf(s: &str) -> String {
    s.replace("\r\n", "\n")
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
    frontmatter
        .allowed_tools
        .iter()
        .any(|pattern| GlobPattern::new(pattern).is_ok_and(|p| p.matches(trimmed)))
}

/// Resolve inline `` !`cmd` `` and fenced `` ```! `` blocks in
/// `body` by running the named commands in `workspace_dir` and
/// inlining their output. Single pass — substituted output is not
/// re-scanned for further placeholders.
///
/// Walk the source in order. Resolve inline commands only in original lines
/// outside dynamic fences; fence output and blocked/unclosed fences remain
/// opaque to the inline parser.
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
pub async fn preprocess_dynamic_context(
    body: &str,
    frontmatter: &SkillFrontmatter,
    workspace_dir: &Path,
) -> Result<String> {
    resolve_fenced(body, frontmatter, workspace_dir).await
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
    let mut output = String::with_capacity(body.len().min(MAX_SKILL_BODY_BYTES));
    let mut first_line = true;
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
                append_line(&mut output, &mut first_line, &result)?;
                fence_buffer.clear();
                continue;
            }
            // Inside the fence — accumulate the command line.
            fence_buffer.push(line.to_string());
            continue;
        }

        // Only source lines are parsed. Never feed fence output to this pass.
        append_line(
            &mut output,
            &mut first_line,
            &resolve_inline(line, frontmatter, workspace_dir).await?,
        )?;
    }

    // Unclosed fence: emit the buffered lines literally so the
    // author can see what was left unprocessed.
    if !fence_buffer.is_empty() {
        for line in &fence_buffer {
            append_line(&mut output, &mut first_line, line)?;
        }
    }

    Ok(output)
}

fn append_line(output: &mut String, first: &mut bool, line: &str) -> Result<()> {
    if !*first {
        append_skill_text(output, "\n")?;
    }
    *first = false;
    append_skill_text(output, line)
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
            let marker = format!("[shell blocked: shell \"{shell}\" not supported, only \"bash\"]");
            let original = fence_buffer.join("\n") + "\n" + closer_line;
            return Ok(format!("{marker}{original}"));
        }
    }
    if !is_command_allowed(frontmatter, command) {
        let marker = "[shell blocked: command not in allowed-tools]";
        let original = fence_buffer.join("\n") + "\n" + closer_line;
        return Ok(format!("{marker}{original}"));
    }
    let out =
        run_shell_blocking(command, workspace_dir, SHELL_TIMEOUT_MS, MAX_OUTPUT_BYTES).await?;
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
    let mut out = String::with_capacity(body.len().min(MAX_SKILL_BODY_BYTES));
    let mut i = 0;

    while i < bytes.len() {
        let ch = body[i..].chars().next().unwrap();
        let ch_len = ch.len_utf8();

        let anchor_ok = ch == '!' && (i == 0 || bytes[i - 1].is_ascii_whitespace());

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
                        append_skill_text(&mut out, &marker)?;
                        append_skill_text(&mut out, placeholder)?;
                        i += placeholder_len;
                        continue;
                    }
                }

                // Allowed-tools glob allowlist.
                if !is_command_allowed(frontmatter, &cmd) {
                    let marker = "[shell blocked: command not in allowed-tools]";
                    append_skill_text(&mut out, marker)?;
                    append_skill_text(&mut out, placeholder)?;
                    i += placeholder_len;
                    continue;
                }

                let out_run =
                    run_shell_blocking(&cmd, workspace_dir, SHELL_TIMEOUT_MS, MAX_OUTPUT_BYTES)
                        .await?;
                let formatted = format_output(&out_run, SHELL_TIMEOUT_MS);
                append_skill_text(&mut out, &formatted)?;
                i += placeholder_len;
                continue;
            }
            // No closing backtick → the `!` is literal.
        }

        append_skill_text(&mut out, &body[i..i + ch_len])?;
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
    use std::path::PathBuf;

    #[tokio::test]
    async fn native_bash_environment_lookup_can_execute_bash() {
        let executable = windows_bash_path().unwrap();
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Command::new(executable)
                .arg("-c")
                .arg("[[ -n native ]] && printf native-bash")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"native-bash");
    }

    #[test]
    fn native_bash_selection_skips_wsl_and_finds_git_installation() {
        let temp = tempfile::tempdir().unwrap();
        let wsl = temp.path().join("Windows/System32/bash.exe");
        let git = temp.path().join("Program Files/Git");
        let native = git.join("bin/bash.exe");
        for path in [&wsl, &native] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }
        assert_eq!(
            select_native_bash([wsl.clone(), native.clone()], []),
            Some(native.clone())
        );
        assert_eq!(select_native_bash([wsl.clone()], [git]), Some(native));
        assert!(select_native_bash([wsl], []).is_none());
        for path in [
            r"C:\WINDOWS\SYSTEM32\bash.exe",
            r"C:\Windows\SysWOW64\bash.exe",
            r"C:\Windows\Sysnative\bash.exe",
            r"C:\Users\user\AppData\Local\Microsoft\WindowsApps\bash.exe",
        ] {
            assert!(is_wsl_launcher(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn native_bash_selection_supports_usr_bin_and_respects_path_order() {
        let temp = tempfile::tempdir().unwrap();
        let git = temp.path().join("Git");
        let first = git.join("usr/bin/bash.exe");
        let second = temp.path().join("MSYS2/bash.exe");
        for path in [&first, &second] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }
        assert_eq!(select_native_bash([], [git]), Some(first.clone()));
        assert_eq!(select_native_bash([first.clone(), second], []), Some(first));
        assert!(select_native_bash([temp.path().join("missing/bash.exe")], []).is_none());
    }

    #[tokio::test]
    async fn blocked_and_unclosed_fences_do_not_execute_embedded_inline_syntax() {
        let ws = tmp_workspace();
        let mut fm = empty_frontmatter();
        fm.allowed_tools = vec!["touch *".into()];
        for body in [
            "```!\nfalse !`touch unintended`\n```",
            "```!\n!`touch unintended`",
        ] {
            preprocess_dynamic_context(body, &fm, ws.path())
                .await
                .unwrap();
            assert!(!ws.path().join("unintended").exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_stdout_and_stderr_are_drained_with_bounded_prefixes() {
        let ws = tmp_workspace();
        let command = "for ((i=0; i<5000; i++)); do printf 1234567890; printf abcdefghij >&2; done; touch completed";
        let out = run_shell_blocking(command, ws.path(), 5000, 1024)
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(!out.timed_out);
        for stream in [out.stdout, out.stderr] {
            assert_eq!(stream.len(), 1024 + "...(truncated)".len());
            assert!(stream.ends_with("...(truncated)"));
        }
        assert!(ws.path().join("completed").exists());
    }

    #[tokio::test]
    async fn bash_selector_uses_bash_syntax() {
        let ws = tmp_workspace();
        let output = preprocess_dynamic_context(
            "!`[[ -d . ]] && printf bash-ok`",
            &empty_frontmatter(),
            ws.path(),
        )
        .await
        .unwrap();
        assert_eq!(output, "bash-ok");
    }

    #[tokio::test]
    async fn combined_dynamic_output_cannot_exceed_the_body_budget() {
        let ws = tmp_workspace();
        let body = "x".repeat(MAX_SKILL_BODY_BYTES - 100) + " !`printf '%01000d' 0`";
        let error = preprocess_dynamic_context(&body, &empty_frontmatter(), ws.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("rendered skill body exceeds"));
    }

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

    fn tmp_workspace() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    /// A timed-out command is killed together with everything it started.
    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_command_is_killed_with_its_children() {
        use crate::tools::builtin::process::tests::{long_lived_child, still_running_after_grace};
        let ws = tmp_workspace();
        let pid_file = ws.path().join("child.pid");
        let out = run_shell_blocking(&long_lived_child(&pid_file), ws.path(), 500, 1024)
            .await
            .unwrap();
        assert!(out.timed_out);
        assert!(
            !still_running_after_grace(&pid_file).await,
            "the timed-out command's child outlived it"
        );
    }

    /// A blocked inline command is replaced by its marker and the text
    /// around it passes through intact.
    #[tokio::test]
    async fn blocked_inline_commands_keep_the_surrounding_text() {
        let ws = tmp_workspace();
        let zsh = SkillFrontmatter {
            shell: Some("zsh".to_string()),
            ..empty_frontmatter()
        };
        let out = preprocess_dynamic_context("A !`echo hi` B", &zsh, ws.path())
            .await
            .unwrap();
        assert_eq!(
            out,
            "A [shell blocked: shell \"zsh\" not supported, only \"bash\"]!`echo hi` B"
        );
        let git_only = SkillFrontmatter {
            allowed_tools: vec!["git *".to_string()],
            ..empty_frontmatter()
        };
        let out = preprocess_dynamic_context("A !`echo hi` B", &git_only, ws.path())
            .await
            .unwrap();
        assert_eq!(
            out,
            "A [shell blocked: command not in allowed-tools]!`echo hi` B"
        );
    }

    /// A command killed by a signal reports exit code -1.
    #[cfg(unix)]
    #[tokio::test]
    async fn signal_killed_inline_command_reports_exit_minus_one() {
        let ws = tmp_workspace();
        let out = preprocess_dynamic_context("!`kill -9 $$`", &empty_frontmatter(), ws.path())
            .await
            .unwrap();
        assert_eq!(out, "stderr: exit code -1");
    }

    #[tokio::test]
    async fn fenced_blocks_for_other_shells_are_refused_not_run() {
        let ws = tmp_workspace();
        let marker = ws.path().join("ran");
        let frontmatter = SkillFrontmatter {
            shell: Some("zsh".to_string()),
            ..empty_frontmatter()
        };
        let body = format!("```!\ntouch '{}'\n```", marker.display());
        let out = preprocess_dynamic_context(&body, &frontmatter, ws.path())
            .await
            .unwrap();
        assert!(
            out.starts_with("[shell blocked: shell \"zsh\" not supported, only \"bash\"]"),
            "{out}"
        );
        assert!(out.contains("touch"), "the original fence is kept: {out}");
        assert!(!marker.exists(), "a blocked fence must not run");
    }

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
        assert_eq!(
            format_output(&out, 5000),
            "stderr: command timed out after 5000 ms"
        );
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
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(!out.contains("!`pwd`"));
        assert!(out.starts_with("CWD: "));
    }

    #[tokio::test]
    async fn preprocess_inline_preserves_surrounding_text() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "before\n!`echo middle`\nafter";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert_eq!(out, "before\nmiddle\n\nafter");
    }

    #[tokio::test]
    async fn preprocess_inline_anchor_rule_blocks_equals_prefix() {
        // `KEY=!`echo hi`` — the `!` is preceded by `=`, so it must
        // be left literal; the command must not run.
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "KEY=!`echo hi`";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert_eq!(out, "KEY=!`echo hi`");
    }

    #[tokio::test]
    async fn preprocess_inline_anchor_rule_at_start_of_line() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "!`echo start` rest";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("start"), "got: {out}");
        assert!(!out.contains("!`"));
    }

    #[tokio::test]
    async fn preprocess_inline_anchor_rule_after_whitespace() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "word !`echo afterspace` end";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("afterspace"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_inline_anchor_rule_after_tab() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "word\t!`echo aftertab` end";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("aftertab"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_inline_unclosed_backtick_left_literal() {
        // `!`echo hi` (no closing backtick) — leave literal.
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        // Body has an opening backtick after `!` but no closing one.
        let body = "no close: !`echo hi";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        // The unclosed `!` is left as a literal; the rest of the body
        // is unchanged.
        assert!(out.contains("!`echo hi"), "got: {out}");
    }

    #[tokio::test]
    #[cfg(unix)] // uses POSIX `sh -c` syntax
    async fn preprocess_inline_failure_appends_stderr() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "result: !`sh -c 'echo out; echo bad 1>&2; exit 7'`";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        // stdout inlined verbatim, then stderr line.
        assert!(out.starts_with("result: out\nstderr: bad"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_inline_command_not_in_allowlist_blocked() {
        let mut fm = empty_frontmatter();
        fm.allowed_tools = vec!["echo *".to_string()];
        let ws = tmp_workspace();
        let body = "Got: !`ls /`";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("[shell blocked: command not in allowed-tools]"));
        assert!(out.contains("!`ls /`"));
    }

    #[tokio::test]
    async fn preprocess_inline_command_in_allowlist_runs() {
        let mut fm = empty_frontmatter();
        fm.allowed_tools = vec!["echo *".to_string()];
        let ws = tmp_workspace();
        let body = "Got: !`echo hello`";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert_eq!(out, "Got: hello\n");
    }

    #[tokio::test]
    async fn preprocess_inline_unknown_shell_blocked() {
        let mut fm = empty_frontmatter();
        fm.shell = Some("powershell".to_string());
        let ws = tmp_workspace();
        let body = "Got: !`echo hi`";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("[shell blocked: shell \"powershell\" not supported"));
        assert!(out.contains("!`echo hi`"));
    }

    #[tokio::test]
    async fn preprocess_inline_bash_shell_explicit_runs() {
        let mut fm = empty_frontmatter();
        fm.shell = Some("bash".to_string());
        let ws = tmp_workspace();
        let body = "Got: !`echo hi`";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("hi"), "got: {out}");
    }

    #[tokio::test]
    #[cfg(unix)] // relies on bash single-quote/backslash semantics
    async fn preprocess_inline_single_pass_output_not_rescanned() {
        // A command whose stdout contains a `!`cmd`` placeholder —
        // that text must NOT be re-scanned. The shell receives
        // `echo '!\`printf INNER\`'` and prints `!\`printf INNER\``
        // literally (single quotes preserve the backslashes).
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "!`echo '!\\`printf INNER\\`'`";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
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
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("alpha"), "got: {out}");
        assert!(
            !out.contains("```"),
            "fence markers should be gone, got: {out}"
        );
    }

    #[tokio::test]
    #[cfg(unix)] // uses POSIX `printf`
    async fn preprocess_fenced_multiline_command() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "```!\nprintf 'one\\ntwo'\n```";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("one"), "got: {out}");
        assert!(out.contains("two"), "got: {out}");
    }

    #[tokio::test]
    async fn preprocess_fenced_unclosed_left_literal() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "before\n```!\necho hi\nstill no closer";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
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
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
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
    #[cfg(unix)] // uses POSIX `sh -c` syntax
    async fn preprocess_fenced_failure_appends_stderr() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "```!\nsh -c 'echo out; echo bad 1>&2; exit 3'\n```";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
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
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.contains("echo should-not-run"));
        assert!(out.contains("```bash"));
        assert!(out.contains("```"));
    }

    #[tokio::test]
    async fn preprocess_inline_works_inside_text_with_other_content() {
        let fm = empty_frontmatter();
        let ws = tmp_workspace();
        let body = "Header\n\n!`echo middle`\n\nFooter";
        let out = preprocess_dynamic_context(body, &fm, ws.path())
            .await
            .unwrap();
        assert!(out.starts_with("Header\n\nmiddle"), "got: {out}");
        assert!(out.ends_with("Footer"), "got: {out}");
    }

    // ---- run_shell_blocking smoke test (uses real shell) ----

    #[tokio::test]
    async fn run_shell_blocking_echo() {
        let ws = tmp_workspace();
        let out = run_shell_blocking("echo hi", ws.path(), 5_000, 100)
            .await
            .unwrap();
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.exit_code, 0);
        assert!(!out.timed_out);
    }

    #[tokio::test]
    #[cfg(unix)] // uses POSIX `sh -c` syntax
    async fn run_shell_blocking_nonzero_captures_stderr() {
        let ws = tmp_workspace();
        let out = run_shell_blocking("sh -c 'echo err 1>&2; exit 9'", ws.path(), 5_000, 100)
            .await
            .unwrap();
        assert_eq!(out.exit_code, 9);
        assert!(out.stderr.contains("err"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_shell_blocking_runs_in_workspace() {
        let ws = tmp_workspace();
        // `pwd` should resolve to the workspace path.
        let out = run_shell_blocking("pwd", ws.path(), 5_000, 100)
            .await
            .unwrap();
        let pwd = out.stdout.trim();
        // Resolve symlinks / canonicalize to handle macOS /private/var
        // vs /var, /tmp vs /private/tmp.
        let canonical_ws = ws
            .path()
            .canonicalize()
            .unwrap_or_else(|_| ws.path().to_path_buf());
        let canonical_pwd = PathBuf::from(pwd)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(pwd));
        assert_eq!(canonical_pwd, canonical_ws);
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn run_shell_blocking_runs_in_workspace() {
        let ws = tmp_workspace();
        // Git Bash exposes the native Windows spelling with -W.
        let out = run_shell_blocking("pwd -W", ws.path(), 5_000, 100)
            .await
            .unwrap();
        let pwd = out.stdout.trim();
        let canonical_ws = ws
            .path()
            .canonicalize()
            .unwrap_or_else(|_| ws.path().to_path_buf());
        let canonical_pwd = PathBuf::from(pwd)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(pwd));
        assert_eq!(canonical_pwd, canonical_ws);
    }

    #[tokio::test]
    async fn fenced_output_is_not_executed_by_inline_parser() {
        let ws = tmp_workspace();
        let source = "```!\nprintf '%s' '!`touch unintended`'\n```";
        let rendered = preprocess_dynamic_context(source, &empty_frontmatter(), ws.path())
            .await
            .unwrap();
        assert!(
            !ws.path().join("unintended").exists(),
            "fenced stdout was executed by inline pass; rendered={rendered:?}"
        );
        assert_eq!(rendered, "!`touch unintended`");
    }
}
