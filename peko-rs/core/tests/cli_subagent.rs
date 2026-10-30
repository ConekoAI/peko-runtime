//! Three daemon checks for Agent dispatch: directory and flat role layouts,
//! and recursive delegation. Assert filesystem effects and persisted Agent
//! receipts; scripted final text alone cannot prove a child ran.
//! Serial because each test replaces the shared mock script.

mod common;
use common::{configure_mock, create_mock_principal, run_with_timeout, DaemonGuard, PekoCli};
use serial_test::serial;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// The role every test spawns, stored at
/// `principals/<name>/roles/worker/ROLE.md`.
const WORKER: &str = "worker";

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

/// Workspace root used by the daemon's filesystem tools in these tests.
/// `PekoCli` sets `PEKO_HOME`, and relative Write paths resolve under the
/// principal's own folder (`<PEKO_HOME>/principals/<name>`).
fn principal_workspace(cli: &PekoCli, principal: &str) -> PathBuf {
    cli.peko_dir().join("principals").join(principal)
}

/// Write a `worker` subagent prompt for the given Principal using the
/// flat-file layout (`roles/<worker>.md`).
fn write_worker_subagent_flat(cli: &PekoCli, principal: &str, worker: &str) {
    let dir = cli
        .peko_dir()
        .join("principals")
        .join(principal)
        .join("roles");
    std::fs::create_dir_all(&dir).expect("create roles dir");
    let agent_md = format!(
        "---\n\
         name: {worker}\n\
         description: Test subagent for the cli_subagent integration suite (flat file)\n\
         ---\n\n\
         You are a test subagent. Follow the task instructions exactly, \
         using the Write/Read/Agent tools as directed.\n"
    );
    std::fs::write(dir.join(format!("{worker}.md")), agent_md).expect("write worker .md");
}

/// Write the worker role in the directory layout (`roles/<worker>/ROLE.md`).
fn write_worker_subagent(cli: &PekoCli, principal: &str, worker: &str) {
    let dir = cli
        .peko_dir()
        .join("principals")
        .join(principal)
        .join("roles")
        .join(worker);
    std::fs::create_dir_all(&dir).expect("create worker subagent dir");
    let agent_md = format!(
        "---\n\
         name: {worker}\n\
         description: Test subagent for the cli_subagent integration suite\n\
         ---\n\n\
         You are a test subagent. Follow the task instructions exactly, \
         using the Write/Read/Agent tools as directed.\n"
    );
    std::fs::write(dir.join("ROLE.md"), agent_md).expect("write worker ROLE.md");
}

/// Create the mock principal and its worker role before starting the daemon.
fn setup_principal(cli: &PekoCli, name: &str, mock_llm_url: &str) {
    create_mock_principal(cli, name, mock_llm_url);
    write_worker_subagent(cli, name, WORKER);
}

/// Create the Principal under test and its `worker` subagent as a flat file.
fn setup_principal_flat(cli: &PekoCli, name: &str, mock_llm_url: &str) {
    create_mock_principal(cli, name, mock_llm_url);
    write_worker_subagent_flat(cli, name, WORKER);
}

/// Verify the successful child output actually returned through Agent's
/// persisted tool result, rather than trusting the mock's final sentinel.
fn assert_child_receipts(cli: &PekoCli, principal: &str, expected: &[&str]) {
    use peko_message::ContentBlock;
    use peko_session::events::SessionEvent;

    let mut dirs = vec![cli
        .peko_dir()
        .join("data/principals")
        .join(principal)
        .join("local/sessions")];
    let mut outputs = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("read session directory") {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            for line in std::fs::read_to_string(path).unwrap().lines() {
                let event: SessionEvent = serde_json::from_str(line).expect("session event");
                let SessionEvent::MessageV2(message) = event else {
                    continue;
                };
                for block in message.message.content {
                    if let ContentBlock::ToolResult {
                        name,
                        content,
                        is_error,
                        ..
                    } = block
                    {
                        if name != "Agent" {
                            continue;
                        }
                        assert!(!is_error, "Agent returned a tool error: {content:?}");
                        let text: String = content
                            .iter()
                            .filter_map(|block| {
                                if let ContentBlock::Text { text } = block {
                                    Some(text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        let receipt: serde_json::Value =
                            serde_json::from_str(&text).expect("Agent receipt JSON");
                        assert_eq!(receipt["success"], true, "{receipt}");
                        assert_eq!(receipt["status"], "completed", "{receipt}");
                        outputs.push(
                            receipt["output"]
                                .as_str()
                                .expect("child output")
                                .to_string(),
                        );
                    }
                }
            }
        }
    }
    outputs.sort();
    let mut expected: Vec<_> = expected.iter().map(|text| text.to_string()).collect();
    expected.sort();
    assert_eq!(outputs, expected, "persisted Agent receipts");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `subagent_blocking.ps1` TEST 1: parent spawns a child that uses
/// `Write` to create a file, then the parent reports success.
///
/// Script: parent turn 1 = `tool_call(Agent, …)`; parent turn 2
/// = text `BLOCKING_SUCCESS` (the parent's LLM sees the blocking
/// tool result in its context). Child turn 1 = `tool_call(write_file)`;
/// child turn 2 = text `CHILD_DONE` (the child's last assistant text
/// becomes the `output` field in the parent's blocking receipt).
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_blocking_t1_write_file() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let parent_needle = "subagent-block-t1-p-vp7b";
    let child_needle = "subagent-block-t1-c-vp7b";
    let agent_name = "subagent_blocking_t1";
    let file_name = "subagent_blocking_T1.txt";
    let file_content = "SUBAGENT_WAS_HERE";

    // The parent's tool_call `prompt` arg is what the child sees as its
    // user message (wrapped by build_subagent_task_message). Embed the
    // child needle into the prompt string so the mock routes the child's
    // LLM call to the child script.
    let task_for_child = format!(
        "Write '{file_name}' with content '{file_content}' via Write. \
         The substring '{child_needle}' is in this task on purpose so the mock \
         can route the child's LLM call. (test=subagent_blocking_T1)"
    );

    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_child, "role": WORKER, "path": "t1-write" }).to_string()
            } },
            "BLOCKING_SUCCESS",
        ],
        child_needle: [
            { "tool_call": { "name": "Write", "arguments":
                serde_json::json!({ "file_path": file_name, "content": file_content }).to_string()
            } },
            "CHILD_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    setup_principal(&cli, agent_name, &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Spawn a subagent to do a task; the task description is in your system prompt. \
         When it returns, respond with BLOCKING_SUCCESS if the child wrote the file. \
         Use the needle '{parent_needle}' in your response."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("BLOCKING_SUCCESS"),
        "parent did not report BLOCKING_SUCCESS: stdout={out} stderr={err}",
    );

    // The child wrote a file into the tool workspace. We also dump the
    // contents of `<peko_dir>/data/` on failure so the path is obvious
    // from the assertion message.
    let path = principal_workspace(&cli, agent_name).join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!(
            "child file not written at {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}"
        )
    });
    assert_eq!(
        actual, file_content,
        "child wrote unexpected content to {path:?}",
    );
    assert_child_receipts(&cli, agent_name, &["CHILD_DONE"]);
}

/// `subagent_nesting.ps1` TEST 1: a 3-level chain (parent → child-A
/// → grandchild-B) where grandchild-B writes a file. The production
/// concurrency budget admits this chain. Each LLM call has its own unique needle.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_nesting_t1_depth2_writes_file() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let parent_needle = "subagent-nest-t1-p-z9aa";
    let child_a_needle = "subagent-nest-t1-ca-z9aa";
    let grandchild_needle = "subagent-nest-t1-gb-z9aa";
    let agent_name = "subagent_nesting_t1";
    let file_name = "nesting_depth2.txt";
    let file_content = "DEPTH_2_REACHED";

    let task_for_child_a = format!(
        "You are Subagent-A at depth 1. Delegate to a grandchild (Subagent-B) \
         that uses Write to create '{file_name}' with content \
         '{file_content}'. Pass the substring '{child_a_needle}' in your own \
         tool_call task arg so the mock can route your LLM call. The substring \
         '{grandchild_needle}' must also appear in the task you pass to \
         Subagent-B so the mock can route its LLM call. \
         (test=subagent_nesting_T1_depth2)"
    );
    let task_for_grandchild = format!(
        "Write '{file_name}' with content '{file_content}' via Write. \
         (test=depth2 grandchild, routed by needle {grandchild_needle})"
    );

    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_child_a, "role": WORKER, "path": "t1-child-a" }).to_string()
            } },
            "NESTING_SUCCESS",
        ],
        child_a_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_grandchild, "role": WORKER, "path": "t1-grandchild" }).to_string()
            } },
            "CHILD_A_DONE",
        ],
        grandchild_needle: [
            { "tool_call": { "name": "Write", "arguments":
                serde_json::json!({ "file_path": file_name, "content": file_content }).to_string()
            } },
            "GRANDCHILD_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    setup_principal(&cli, agent_name, &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Spawn a subagent that spawns a grandchild that writes a file. When \
         the chain completes, respond with NESTING_SUCCESS. Use the needle \
         '{parent_needle}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("NESTING_SUCCESS"),
        "parent did not report NESTING_SUCCESS: stdout={out} stderr={err}",
    );

    // The grandchild wrote the file into the tool workspace
    // (`<peko_dir>/principals/{principal}/{file_name}`), since the tool
    // workspace is the principal's own folder.
    let path = principal_workspace(&cli, agent_name).join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!("grandchild file not written at {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}")
    });
    assert_eq!(actual, file_content);
    assert_child_receipts(&cli, agent_name, &["CHILD_A_DONE", "GRANDCHILD_DONE"]);
}

/// Flat-file layout variant of `subagent_blocking_t1_write_file`.
/// Verifies that a subagent prompt at `roles/worker.md` is discovered
/// and can be spawned using the file stem as the `role` argument.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_blocking_t1_flat_file() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let parent_needle = "subagent-block-t1-flat-p-vp7b";
    let child_needle = "subagent-block-t1-flat-c-vp7b";
    let agent_name = "subagent_blocking_t1_flat";
    let file_name = "subagent_blocking_T1_flat.txt";
    let file_content = "SUBAGENT_WAS_HERE_FLAT";

    let task_for_child = format!(
        "Write '{file_name}' with content '{file_content}' via Write. \
         The substring '{child_needle}' is in this task on purpose so the mock \
         can route the child's LLM call. (test=subagent_blocking_T1_flat)"
    );

    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_child, "role": WORKER, "path": "t1-flat" }).to_string()
            } },
            "BLOCKING_SUCCESS",
        ],
        child_needle: [
            { "tool_call": { "name": "Write", "arguments":
                serde_json::json!({ "file_path": file_name, "content": file_content }).to_string()
            } },
            "CHILD_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    setup_principal_flat(&cli, agent_name, &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Spawn a subagent to do a task; the task description is in your system prompt. \
         When it returns, respond with BLOCKING_SUCCESS if the child wrote the file. \
         Use the needle '{parent_needle}' in your response."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("BLOCKING_SUCCESS"),
        "parent did not report BLOCKING_SUCCESS: stdout={out} stderr={err}",
    );

    let path = principal_workspace(&cli, agent_name).join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!("child file not written at {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}")
    });
    assert_eq!(actual, file_content);
    assert_child_receipts(&cli, agent_name, &["CHILD_DONE"]);
}

// ---------------------------------------------------------------------------
// Diagnostic
// ---------------------------------------------------------------------------

/// Recursively list `<peko_dir>/data/` (best-effort) and return a
/// human-readable dump for use in `unwrap_or_else` panic messages.
/// Walks up to 4 levels deep and is bounded to keep the panic
/// message readable. Helps diagnose where a child subagent's file
/// actually landed (or didn't).
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
