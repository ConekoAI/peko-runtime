//! CLI integration tests for the built-in tools (Phase B slice per
//! `docs/integration/TESTING.md` §7).
//!
//! Coverage mirrors the mockable `e2e_tests/tools/built-in/*.ps1` scripts
//! that previously exercised this surface outside CI:
//!
//! | PS script                  | Rust test                                          |
//! |----------------------------|----------------------------------------------------|
//! | `built-in/glob.ps1`        | `built_in_glob_finds_files`                        |
//! | `built-in/grep.ps1`        | `built_in_grep_searches_content`                   |
//! | `built-in/read_file.ps1`   | `built_in_read_file_returns_content`               |
//! | `built-in/write_file.ps1`  | `built_in_write_file_creates_file`                 |
//! | `built-in/Edit.ps1`        | `built_in_edit_modifies_file`                 |
//! | `built-in/Bash.ps1`        | `built_in_bash_executes_command`              |
//!
//! Each test:
//!   1. Builds an isolated [`PekoCli`] tempdir as `HOME`.
//!   2. Calls `POST /_test/configure` on the mock LLM to install a
//!      scripted `MOCK_LLM_SCRIPT` (and reset the per-substring counter).
//!   3. Spawns a plain `DaemonGuard`.
//!   4. Runs `peko send <agent> <prompt> --no-stream` and asserts on the
//!      parent's final stdout plus, where applicable, on the file the
//!      tool wrote into the tool workspace.
//!
//! Each PS script originally had 2–3 sub-tests per tool. We collapse to
//! a single representative case per tool because:
//!   - The shape is identical (LLM → tool call → file/stdout assertion).
//!   - The per-tool `Tool` impl is unit-tested in
//!     `src/tools/builtin/fs/*.rs` and `src/tools/builtin/shell.rs`.
//!   - Adding more sub-tests in this file would be coverage duplication.
//!
//! Tier: mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set). Tests early-return if unset so `cargo test`
//! still passes on a bare checkout.
//!
//! **`#[serial]`.** The mock's per-substring counter is global state
//! across all test binaries. Every test in this file is `#[serial]`
//! to avoid concurrent tests racing the same counter; per-test unique
//! needles are belt-and-suspenders.
//!
//! **Deferred from `e2e_tests/tools/`:**
//!   - `tool_all.ps1` (meta-runner) — replaced by `cargo test --test cli_tools`.
//!   - `tool_async.ps1` (7 sub-tests) — exercises `_async: true` plumbing
//!     against `AsyncTaskRegistry`. The PS file itself documents many of
//!     these as "EXPECTED FAIL (feature may be stubbed)".
//!   - `tool_timeout.ps1` (1 test) — depends on the LLM reasoning about
//!     `_timeout: 3` semantics. Real-LLM tier.
//!   - `tool_update_mid_session.ps1` (4 sub-tests) — tests ADR-019 mid-
//!     session `peko ext enable/disable --target`. Mostly real-LLM tier;
//!     the daemon-side enable/disable behavior is a config-level test,
//!     not a tool-dispatch test, and is a better fit for a dedicated
//!     ADR-019 PR.

mod common;
use common::{
    configure_mock, create_mock_principal_with_tools, run_with_timeout, DaemonGuard, PekoCli,
};
use serial_test::serial;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MOCK_LLM_URL` and return Some(url) if set, None otherwise.
/// Tests early-return on None so `cargo test` still passes on a bare
/// checkout without the docker-compose stack.
fn mock_llm_url() -> Option<String> {
    let url = std::env::var("MOCK_LLM_URL").ok()?;
    if url.is_empty() {
        return None;
    }
    Some(url)
}

/// Run a `peko …` command and return (stdout, stderr, status).
fn run(
    cli: &PekoCli,
    args: &[&str],
    timeout: Duration,
) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        args,
        timeout,
    )
    .expect("run peko command");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

fn assert_ok(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert_eq!(
        status.code(),
        Some(0),
        "exited non-zero (status={status:?})\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// Directory the daemon's built-in tools treat as their workspace root.
///
/// `ToolRuntime::register_builtins` (in `src/runtime/tool_runtime.rs:154-170`)
/// sets the per-tool `workspace_dir` to `path_resolver.agent_workspace(".", None).parent()`,
/// which resolves to `{data_dir}/workspaces`. The `data_dir` for a test
/// is `<peko_dir>/data` because `PekoCli` sets `PEKO_HOME=peko_dir` and
/// `default_data_dir()` (in `src/common/paths.rs:65-69`) appends `/data`
/// to `PEKO_HOME`. So a `write_file("foo.txt", …)` lands at
/// `<peko_dir>/data/workspaces/foo.txt` — the agent name is NOT in the
/// path (the tool workspace is the shared workspaces root, not the
/// per-agent personal dir).
fn workspace_dir(cli: &PekoCli) -> PathBuf {
    cli.peko_dir().join("data").join("workspaces")
}

/// Ensure the daemon's tool-workspace root exists. The daemon doesn't
/// create `<peko_dir>/data/workspaces/` on its own — it only writes
/// into it when a tool is invoked. Tests that pre-seed files (for
/// Read / grep / Edit inputs, or to verify
/// write_file's output) need to mkdir -p the root first.
fn ensure_workspace_dir(cli: &PekoCli) {
    std::fs::create_dir_all(workspace_dir(cli)).expect("create workspaces dir");
}

/// Write a mock-LLM-pointed agent that has all 6 built-in filesystem +
/// shell tools enabled.
///
/// **`[extensions] enabled` is a special filter.** The agent's
/// `init_builtins_async` (in `src/agent/agent.rs:121-135`) iterates
/// the per-agent tools and compares each whitelist pattern to
/// `tool.name()` (e.g. `"Write"`). The dispatcher check at
/// `src/extension/core/tool_registry.rs:60-63` does the same lookup
/// against `tool_owners[tool_name]`, which stores the canonical
/// extension ID (e.g. `"builtin:tool:Write"`). The whitelist
/// must therefore contain BOTH the bare tool name AND the canonical
/// extension ID — the bare name so the per-agent init registers the
/// tool, and the canonical ID so the dispatcher's `is_tool_enabled`
/// check at execution time resolves the owner and matches the
/// whitelist. Omitting either one yields
/// "Error: Tool 'write_file' is currently disabled..." in the
/// parent's tool result. See `docs/integration/TESTING.md` §7 for
/// the context.
fn write_builtin_agent(cli: &PekoCli, name: &str, mock_llm_url: &str) {
    create_mock_principal_with_tools(
        cli,
        name,
        mock_llm_url,
        &["Bash", "Read", "Write", "Glob", "Grep", "Edit"],
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `built-in/glob.ps1` T1+T2+T3: glob with `*.py` / `*.rs` / `**/*.rs`
/// patterns. We exercise one representative case (the recursive
/// `**/*.rs` pattern, which transitively covers the flat `*.py` and
/// `*.rs` cases).
///
/// Pre-seeds the workspace with a small file tree so the LLM's
/// scripted `tool_call(glob, …)` has something to find. The assertion
/// is on the parent's `GLOB_DONE` sentinel in stdout — proving the
/// tool call was dispatched, executed, and its result fed back to the
/// LLM. The tool's own filtering is unit-tested in
/// `src/tools/builtin/fs/glob.rs`.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn built_in_glob_finds_files() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "built-in-glob-f2a8";
    let agent_name = "built_in_glob";

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "Glob", "arguments":
                serde_json::json!({ "pattern": "**/*.rs" }).to_string()
            } },
            "GLOB_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_builtin_agent(&cli, agent_name, &mock_url);

    // Pre-seed the workspace with a tiny file tree so glob has something
    // to find. Files land in <peko_dir>/data/workspaces/ (see
    // workspace_dir() above).
    let ws = workspace_dir(&cli);
    ensure_workspace_dir(&cli);
    std::fs::create_dir_all(ws.join("src")).expect("create src dir");
    std::fs::write(ws.join("file1.rs"), "// file1\n").expect("write file1.rs");
    std::fs::write(ws.join("file2.rs"), "// file2\n").expect("write file2.rs");
    std::fs::write(ws.join("script.py"), "# script\n").expect("write script.py");
    std::fs::write(ws.join("src").join("main.rs"), "fn main() {}\n").expect("write src/main.rs");

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use your Glob tool with pattern '**/*.rs' to list Rust files in your \
         workspace. When you've seen the result, respond GLOB_DONE. Use the \
         needle '{needle}' in your response."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("GLOB_DONE"),
        "parent did not report GLOB_DONE: stdout={out} stderr={err}",
    );
}

/// `built-in/grep.ps1` T1+T2+T3: regex search with optional
/// `glob`/`case_insensitive` filters. We exercise a case-insensitive
/// search for `TODO|FIXME` — the most distinctive grep test in the
/// original PS suite.
///
/// Pre-seeds the workspace with files containing the patterns. The
/// assertion is on the parent's `GREP_DONE` sentinel in stdout.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn built_in_grep_searches_content() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "built-in-grep-b4c1";
    let agent_name = "built_in_grep";

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "Grep", "arguments":
                serde_json::json!({
                    "pattern": "TODO|FIXME",
                    "case_insensitive": true,
                }).to_string()
            } },
            "GREP_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_builtin_agent(&cli, agent_name, &mock_url);

    // Pre-seed the workspace with a file containing a TODO marker.
    let ws = workspace_dir(&cli);
    ensure_workspace_dir(&cli);
    std::fs::write(
        ws.join("notes.txt"),
        "TODO: implement login\nFIXME: handle errors properly\n",
    )
    .expect("write notes.txt");

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use your Grep tool with pattern 'TODO|FIXME' and case_insensitive=true \
         to search your workspace. When you've seen the result, respond \
         GREP_DONE. Use the needle '{needle}' in your response."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("GREP_DONE"),
        "parent did not report GREP_DONE: stdout={out} stderr={err}",
    );
}

/// `built-in/read_file.ps1` T1+T2: read entire file (T2 is line-range,
/// the same code path). The test pre-seeds a file and asks the LLM
/// to read it back; the assertion is on the parent's `READ_DONE`
/// sentinel.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn built_in_read_file_returns_content() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "built-in-readfile-d6e3";
    let agent_name = "built_in_read_file";
    let file_name = "built_in_read_file_T1.txt";

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "Read", "arguments":
                serde_json::json!({ "file_path": file_name }).to_string()
            } },
            "READ_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_builtin_agent(&cli, agent_name, &mock_url);

    // Pre-seed the workspace with a file the LLM will read.
    let ws = workspace_dir(&cli);
    ensure_workspace_dir(&cli);
    std::fs::write(
        ws.join(file_name),
        "Line 1: Hello\nLine 2: World\nLine 3: Testing\n",
    )
    .expect("write test file");

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use your Read tool to read '{file_name}'. When you've seen the \
         content, respond READ_DONE. Use the needle '{needle}' in your response."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("READ_DONE"),
        "parent did not report READ_DONE: stdout={out} stderr={err}",
    );
}

/// `built-in/write_file.ps1` T1+T2+T3: create / overwrite / nested.
/// We exercise the create-new case (T1) and assert on the file's
/// actual content on disk — the only built-in tool test that has a
/// real file-system side effect we can verify deterministically.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn built_in_write_file_creates_file() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "built-in-writefile-7a9f";
    let agent_name = "built_in_write_file";
    let file_name = "built_in_write_file_T1.txt";
    let file_content = "Hello from Write tool!";

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "Write", "arguments":
                serde_json::json!({
                    "file_path": file_name,
                    "content": file_content,
                }).to_string()
            } },
            "WRITE_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_builtin_agent(&cli, agent_name, &mock_url);
    // Ensure the tool workspace root exists — the daemon's `Write`
    // auto-creates the file's *parent* directory but not the workspace
    // root itself. Pre-creating it makes the test less reliant on that
    // implementation detail.
    ensure_workspace_dir(&cli);
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use your Write tool to create '{file_name}' with content \
         '{file_content}' in your workspace. When the file has been written, \
         respond WRITE_DONE. Use the needle '{needle}' in your response."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("WRITE_DONE"),
        "parent did not report WRITE_DONE: stdout={out} stderr={err}",
    );

    // The LLM's `Write` tool call wrote a file into the tool
    // workspace. We also dump the contents of `<peko_dir>/data/` on
    // failure so the path is obvious from the assertion message.
    let path = workspace_dir(&cli).join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!(
            "write_file did not create {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}"
        )
    });
    assert_eq!(
        actual, file_content,
        "write_file produced unexpected content at {path:?}",
    );
}

/// `built-in/Edit.ps1` T1+T2+T3: simple / multiple /
/// atomic-fail. We exercise the simple replacement (T1) — it covers
/// the in-place file mutation path. The atomic-fail case (T3) is
/// unit-tested in `src/tools/builtin/fs/edit.rs`.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn built_in_edit_modifies_file() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "built-in-strreplace-e0b7";
    let agent_name = "built_in_edit";
    let file_name = "built_in_edit_T1.txt";
    let old_string = "Original Name";
    let new_string = "New Name";

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "Edit", "arguments":
                serde_json::json!({
                    "file_path": file_name,
                    "old_string": old_string,
                    "new_string": new_string,
                    "replace_all": false,
                }).to_string()
            } },
            "EDIT_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_builtin_agent(&cli, agent_name, &mock_url);

    // Pre-seed the workspace with a file containing the old string.
    let ws = workspace_dir(&cli);
    ensure_workspace_dir(&cli);
    let initial =
        format!("[settings]\nname = \"{old_string}\"\nversion = \"1.0.0\"\ndebug = true\n");
    std::fs::write(ws.join(file_name), initial).expect("write initial file");

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use your Edit tool to replace '{old_string}' with \
         '{new_string}' in '{file_name}'. When the replacement is done, \
         respond EDIT_DONE. Use the needle '{needle}' in your response."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("EDIT_DONE"),
        "parent did not report EDIT_DONE: stdout={out} stderr={err}",
    );

    // Verify the replacement actually landed in the file.
    let path = ws.join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!(
            "Edit target not readable at {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}"
        )
    });
    assert!(
        actual.contains(new_string) && !actual.contains(old_string),
        "Edit did not produce the expected content at {path:?}: {actual}",
    );
}

/// `built-in/Bash.ps1` T1+T2: execute a basic shell command (T1)
/// vs. with a working directory (T2). We exercise T1 with a
/// cross-platform `echo` so the same test runs on Unix and Windows.
///
/// The Bash tool's actual command execution is unit-tested in
/// `src/tools/builtin/bash.rs`. The CLI integration concern is that
/// the tool call is dispatched end-to-end through the agent loop.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn built_in_bash_executes_command() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "built-in-bash-3f5d";
    let agent_name = "built_in_bash";

    // Cross-platform echo: `echo BASH_TEST_MARKER` works in sh, bash,
    // cmd, and PowerShell (with minor quoting differences — we use
    // single quotes to keep the literal identical).
    let bash_command = "echo BASH_TEST_MARKER";

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "Bash", "arguments":
                serde_json::json!({ "command": bash_command }).to_string()
            } },
            "BASH_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_builtin_agent(&cli, agent_name, &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Use your Bash tool to run '{bash_command}'. When the command has \
         executed, respond BASH_DONE. Use the needle '{needle}' in your \
         response."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("BASH_DONE"),
        "parent did not report BASH_DONE: stdout={out} stderr={err}",
    );
}

// ---------------------------------------------------------------------------
// Diagnostic
// ---------------------------------------------------------------------------

/// Recursively list `<peko_dir>/data/` (best-effort) and return a
/// human-readable dump for use in `unwrap_or_else` panic messages.
/// Walks up to 4 levels deep and is bounded to keep the panic
/// message readable. Helps diagnose where a tool's file actually
/// landed (or didn't).
fn dump_data_dir(cli: &PekoCli) -> String {
    let root = cli.peko_dir().join("data");
    if !root.exists() {
        return format!("  (does not exist: {})", root.display());
    }
    let mut out = String::new();
    fn walk(dir: &std::path::Path, depth: usize, out: &mut String) {
        if depth > 4 {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(it) => it,
            Err(e) => {
                out.push_str(&format!("  {}: read_dir error: {}\n", dir.display(), e));
                return;
            }
        };
        for e in entries.flatten() {
            let p = e.path();
            let indent = "  ".repeat(depth + 1);
            if p.is_dir() {
                out.push_str(&format!(
                    "{indent}{}/\n",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ));
                walk(&p, depth + 1, out);
            } else {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push_str(&format!(
                    "{indent}{} ({} bytes)\n",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    size
                ));
            }
        }
    }
    out.push_str(&format!("  (root) {}/\n", root.display()));
    walk(&root, 0, &mut out);
    out
}
