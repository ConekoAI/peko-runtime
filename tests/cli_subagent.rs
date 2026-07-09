//! CLI integration tests for the `builtin:tool:Agent` path
//! (Phase B slice per `docs/integration/TESTING.md` §7).
//!
//! Coverage mirrors the mockable `e2e_tests/subagent/*.ps1` scripts that
//! previously exercised this surface outside CI:
//!
//! | PS script                  | Rust tests                                                                                                |
//! |----------------------------|-----------------------------------------------------------------------------------------------------------|
//! | `subagent_blocking.ps1`    | `subagent_blocking_t1_write_file`, `subagent_blocking_t2_isolated`, `subagent_blocking_t4_inline_read`      |
//! | `subagent_nesting.ps1`     | `subagent_nesting_t1_depth2_writes_file`, `subagent_nesting_t2_depth_limit`                               |
//! | `subagent_isolation.ps1`   | `subagent_isolation_t1_shared_workspace`, `subagent_isolation_t2_isolated_writes_file`                     |
//! | `subagent_async.ps1`       | (deferred — `_async` path requires a populated `AsyncTaskRegistry`, not directly seedable from a test)    |
//! | `subagent_status_list.ps1` | (deferred — same reason: `task` tool reads from in-process registry)                                      |
//!
//! **Principal model.** After the "Principal as the single actor" migration,
//! `peko send <name>` targets a *Principal*, whose root agent
//! (`principals/<name>/agents/root/AGENT.md` or `principals/<name>/agents/root.md`)
//! is the parent that calls the `Agent` tool. The `Agent` tool's `subagent_type`
//! resolves to a sibling subagent prompt at
//! `principals/<name>/agents/<type>/AGENT.md` (see
//! `AgentService::resolve_principal_agent`). These tests therefore:
//!   * create the Principal via [`create_mock_principal_with_tools`], granting
//!     the capability tools (`Agent`, `Write`, `Read`, `Bash`) that the
//!     dispatcher's owner check requires for both the root agent and any
//!     subagent it spawns (subagents share the Principal's permission
//!     boundary); and
//!   * write a `worker` subagent prompt at
//!     `principals/<name>/agents/worker/AGENT.md` or
//!     `principals/<name>/agents/worker.md` (the resolver accepts both the
//!     directory form and the flat-file form). The default `root.md` is a
//!     *file* and only serves as the root agent prompt.
//!     A spawned subagent's tool whitelist comes from `ExtensionConfig::default()`
//!     (which includes `Agent`, `Write`, `Read`, `Bash`, …), so the `worker`
//!     prompt needs no tool frontmatter — `AGENT.md` has no `tools` field anyway.
//!
//! Each test:
//!   1. Builds an isolated [`PekoCli`] tempdir as `HOME`.
//!   2. Calls `POST /_test/configure` on the mock LLM to install a
//!      scripted `MOCK_LLM_SCRIPT` (and reset the per-substring counter).
//!   3. Creates the Principal + `worker` subagent via [`setup_principal`].
//!   4. Spawns a plain `DaemonGuard` (no `--interval` — subagent tests
//!      don't poll, and the child subagent's blocking LLM call goes
//!      straight through the same mock endpoint).
//!   5. Runs `peko send <principal> <prompt> --no-stream` and asserts on the
//!      parent's final stdout plus, where applicable, on the file the
//!      child wrote into the tool workspace.
//!
//! Tier: mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set). Tests early-return if unset so `cargo test`
//! still passes on a bare checkout.
//!
//! **Substring keying.** The parent agent's LLM call and each child
//! subagent's LLM call all hit the same mock endpoint. The parent sees
//! the `peko send` prompt in its first user message; each child sees a
//! brand-new two-message request whose `user` is the wrapped task
//! (template at `src/agent/subagent_announce.rs:146-155`). To route
//! the parent vs. child to different script entries, each test embeds
//! a per-speaker unique needle into the message that speaker sees: the
//! parent needle is in the `peko send` prompt itself, the child needle
//! is in the parent's `Agent` `prompt` arg (which the child sees
//! wrapped). See per-test comments for the exact placement.
//!
//! **`#[serial]`.** The mock's per-substring counter is global state
//! across all test binaries. Every test in this file is `#[serial]` to
//! avoid concurrent tests racing the same counter; per-test unique
//! needles are belt-and-suspenders.

mod common;
use common::{
    configure_mock, create_mock_principal_with_tools, run_with_timeout, DaemonGuard, PekoCli,
};
use serial_test::serial;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// The `subagent_type` every test spawns. Resolves to
/// `principals/<name>/agents/worker/AGENT.md`.
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

/// Directory the daemon's built-in tools treat as their workspace root.
///
/// `ToolRuntime::register_builtins` (in `src/runtime/tool_runtime.rs:154-170`)
/// sets the per-tool `workspace_dir` to `path_resolver.agent_workspace(".", None).parent()`,
/// which resolves to `{data_dir}/workspaces`. The `data_dir` for a test
/// is `<peko_dir>/data` because `PekoCli` sets `PEKO_HOME=peko_dir` and
/// `default_data_dir()` (in `src/common/paths.rs:65-69`) appends `/data`
/// to `PEKO_HOME`. So a child subagent's `write_file("foo.txt", …)` lands
/// at `<peko_dir>/data/workspaces/foo.txt` — the parent agent name is
/// NOT in the path (the tool workspace is the shared workspaces root,
/// not the per-agent personal dir).
fn workspace_dir(cli: &PekoCli) -> PathBuf {
    cli.peko_dir().join("data").join("workspaces")
}

/// Write a `worker` subagent prompt for the given Principal using the
/// flat-file layout (`agents/<worker>.md`).
fn write_worker_subagent_flat(cli: &PekoCli, principal: &str, worker: &str) {
    let dir = cli
        .peko_dir()
        .join("principals")
        .join(principal)
        .join("agents");
    std::fs::create_dir_all(&dir).expect("create agents dir");
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

/// Write a `worker` subagent prompt for the given Principal.
///
/// `AgentService::resolve_principal_agent` resolves a `subagent_type` to
/// `<workspace>/agents/<type>/AGENT.md` (the directory form) or
/// `<workspace>/agents/<type>.md` (the flat-file form). The root
/// prompt `agents/root.md` created by `peko principal create` is a *file*
/// and is NOT a valid `subagent_type`, so each test creates an explicit
/// `worker` subagent here. The subagent's tool whitelist comes from
/// `ExtensionConfig::default()` (Agent/Write/Read/Bash/…), so the prompt body
/// and frontmatter carry no tool grants — `AGENT.md` has no `tools` field.
fn write_worker_subagent(cli: &PekoCli, principal: &str, worker: &str) {
    let dir = cli
        .peko_dir()
        .join("principals")
        .join(principal)
        .join("agents")
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
    std::fs::write(dir.join("AGENT.md"), agent_md).expect("write worker AGENT.md");
}

/// Create the Principal under test and its `worker` subagent.
///
/// Grants the capability tools (`Agent`, `Write`, `Read`, `Bash`) the
/// dispatcher's owner check requires. Subagents share the Principal's
/// permission boundary, so these grants cover both the root agent's own
/// tool calls and any tool calls a spawned `worker` makes. Must be called
/// BEFORE `DaemonGuard::spawn` (it only writes files).
fn setup_principal(cli: &PekoCli, name: &str, mock_llm_url: &str) {
    create_mock_principal_with_tools(cli, name, mock_llm_url, &["Agent", "Write", "Read", "Bash"]);
    write_worker_subagent(cli, name, WORKER);
}

/// Create the Principal under test and its `worker` subagent as a flat file.
fn setup_principal_flat(cli: &PekoCli, name: &str, mock_llm_url: &str) {
    create_mock_principal_with_tools(cli, name, mock_llm_url, &["Agent", "Write", "Read", "Bash"]);
    write_worker_subagent_flat(cli, name, WORKER);
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
                serde_json::json!({ "prompt": task_for_child, "subagent_type": WORKER }).to_string()
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
        &["send", agent_name, &prompt, "--no-stream"],
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
    let path = workspace_dir(&cli).join(file_name);
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
}

/// `subagent_blocking.ps1` TEST 2: same shape as T1 but with
/// `isolated: true` on the parent's `Agent` arg. Verifies the
/// `isolated` flag is plumbed through without erroring.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_blocking_t2_isolated() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let parent_needle = "subagent-block-t2-p-q3jh";
    let child_needle = "subagent-block-t2-c-q3jh";
    let agent_name = "subagent_blocking_t2";
    let file_name = "subagent_blocking_T2_isolated.txt";
    let file_content = "ISOLATED_SUBAGENT_WAS_HERE";

    let task_for_child = format!(
        "Write '{file_name}' with content '{file_content}' via Write. \
         Substring '{child_needle}' for mock routing. (test=subagent_blocking_T2_isolated)"
    );

    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({
                    "prompt": task_for_child,
                    "subagent_type": WORKER,
                    "isolated": true,
                }).to_string()
            } },
            "ISOLATED_SUCCESS",
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
        "Spawn an isolated subagent to do a task. When it returns, respond \
         with ISOLATED_SUCCESS. Use the needle '{parent_needle}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("ISOLATED_SUCCESS"),
        "parent did not report ISOLATED_SUCCESS: stdout={out} stderr={err}",
    );

    let path = workspace_dir(&cli).join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!("child file not written at {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}")
    });
    assert_eq!(actual, file_content);
}

/// `subagent_blocking.ps1` TEST 4: the inline-result contract. The
/// parent first writes a file with `Write`, then spawns a child
/// that uses `read_file` to return the file content. The parent's
/// blocking tool result for `Agent` should include the child's
/// text (`INLINE_RESULT_OK`) in its `output` field — proving the
/// child's text made it through the inline-result channel, not via
/// the async-receipt + polling path.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_blocking_t4_inline_read() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let parent_needle = "subagent-block-t4-p-k8lf";
    let child_needle = "subagent-block-t4-c-k8lf";
    let agent_name = "subagent_blocking_t4";
    let file_name = "subagent_blocking_T4_inline.txt";
    let file_content = "INLINE_RESULT_OK";

    let task_for_child = format!(
        "Read '{file_name}' via Read and return its content as your final \
         text. Substring '{child_needle}' for mock routing. \
         (test=subagent_blocking_T4_inline)"
    );

    let script = serde_json::json!({
        parent_needle: [
            // Parent turn 1: write the file the child will read.
            { "tool_call": { "name": "Write", "arguments":
                serde_json::json!({ "file_path": file_name, "content": file_content }).to_string()
            } },
            // Parent turn 2: spawn the child.
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_child, "subagent_type": WORKER }).to_string()
            } },
            // Parent turn 3: report success. The child text was
            // `INLINE_RESULT_OK` and the parent's blocking tool
            // result includes it; the parent's LLM sees it and
            // responds.
            "INLINE_SUCCESS",
        ],
        child_needle: [
            { "tool_call": { "name": "Read", "arguments":
                serde_json::json!({ "file_path": file_name }).to_string()
            } },
            // Child's final text — this is what gets captured into
            // the parent's blocking receipt's `output` field.
            file_content,
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    setup_principal(&cli, agent_name, &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "First Write '{file_name}' with content '{file_content}'. Then \
         spawn a subagent to read it and return the content. Then respond with \
         INLINE_SUCCESS. Use the needle '{parent_needle}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("INLINE_SUCCESS"),
        "parent did not report INLINE_SUCCESS: stdout={out} stderr={err}",
    );
    // NOTE: We don't assert that the child's `file_content` text appears
    // in the parent's stdout. The MOCK_LLM_SCRIPT sequences hardcoded
    // text responses; the parent's third turn is hardcoded `INLINE_SUCCESS`,
    // not a synthesized echo of the blocking tool result. Proving the
    // inline channel carries the child text would require a real LLM
    // or a more sophisticated mock that can read prior tool results —
    // out of scope for this migration. The `INLINE_SUCCESS` assertion
    // above is sufficient to prove the blocking tool call completed.
}

/// `subagent_nesting.ps1` TEST 1: a 3-level chain (parent → child-A
/// → grandchild-B) where grandchild-B writes a file. The production
/// default `max_depth` for `AgentSpawnTool` is 3 (see
/// `src/tools/builtin/messaging/agent.rs`), so this chain
/// stays within budget. Each LLM call has its own unique needle.
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
                serde_json::json!({ "prompt": task_for_child_a, "subagent_type": WORKER }).to_string()
            } },
            "NESTING_SUCCESS",
        ],
        child_a_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_grandchild, "subagent_type": WORKER }).to_string()
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
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("NESTING_SUCCESS"),
        "parent did not report NESTING_SUCCESS: stdout={out} stderr={err}",
    );

    // The grandchild wrote the file into the tool workspace
    // (`<peko_dir>/data/workspaces/{file_name}`), since the tool
    // workspace is the shared workspaces root.
    let path = workspace_dir(&cli).join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!("grandchild file not written at {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}")
    });
    assert_eq!(actual, file_content);
}

/// `subagent_nesting.ps1` TEST 2: depth-limit enforcement (smoke).
///
/// With production default `max_depth = 3` (see
/// `src/tools/builtin/messaging/agent.rs`), a chain of
/// 4 levels (parent=0 → child=1 → grandchild=2 → great-grandchild=3)
/// fits, and a 5th-level spawn attempt is rejected. This test
/// exercises the 3-level chain's parent→A→B→C path; the depth-limit
/// code path itself is unit-tested in
/// `src/agent/tests/subagent_integration_tests.rs::test_depth_limit_enforcement`,
/// which directly calls `spawn_and_execute` with explicit
/// `ExecutionConfig { max_depth: 2 }`. We keep this test as a smoke
/// test for the multi-level dispatch plumbing through the CLI.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_nesting_t2_depth_limit() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let parent_needle = "subagent-nest-t2-p-q1w2";
    let child_a_needle = "subagent-nest-t2-c1-q1w2";
    let grandchild_needle = "subagent-nest-t2-c2-q1w2";
    let agent_name = "subagent_nesting_t2";

    let task_for_child_a = format!(
        "You are Subagent-A at depth 1. Spawn a grandchild Subagent-B. \
         Embed '{child_a_needle}' in your tool_call task arg so the mock \
         can route your LLM call. Embed '{grandchild_needle}' in the task \
         string you pass to Subagent-B. \
         (test=subagent_nesting_T2_depth_limit)"
    );
    let task_for_grandchild = format!(
        "You are Subagent-B at depth 2. Spawn a great-grandchild Subagent-C. \
         Substring '{grandchild_needle}' for mock routing. \
         (test=subagent_nesting_T2_depth_limit)"
    );

    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_child_a, "subagent_type": WORKER }).to_string()
            } },
            "PARENT_DONE",
        ],
        child_a_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_grandchild, "subagent_type": WORKER }).to_string()
            } },
            "CHILD_A_DONE",
        ],
        grandchild_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": "would-be-depth-3-task", "subagent_type": WORKER }).to_string()
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
        "Spawn a subagent that spawns a grandchild that spawns a \
         great-grandchild. Use the needle '{parent_needle}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(60),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("PARENT_DONE"),
        "parent did not report PARENT_DONE: stdout={out} stderr={err}",
    );
}

/// `subagent_isolation.ps1` TEST 1: shared workspace. The parent
/// writes a file, then spawns a non-isolated child that reads it
/// back. With `isolated: false` (the default), the child has access
/// to the parent's session context.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_isolation_t1_shared_workspace() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let parent_needle = "subagent-iso-t1-shared-r4km";
    let child_needle = "subagent-iso-t1-shared-c-r4km";
    let agent_name = "subagent_isolation_t1";
    let file_name = "subagent_isolation_T1_shared.txt";
    let file_content = "SHARED_CONTEXT_SECRET";

    let task_for_child = format!(
        "Read '{file_name}' via Read and return its content as your \
         final text. Substring '{child_needle}' for mock routing. \
         (test=subagent_isolation_T1_shared_workspace)"
    );

    let script = serde_json::json!({
        parent_needle: [
            // Parent turn 1: write the file the child will read.
            { "tool_call": { "name": "Write", "arguments":
                serde_json::json!({ "file_path": file_name, "content": file_content }).to_string()
            } },
            // Parent turn 2: spawn a non-isolated child.
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({
                    "prompt": task_for_child,
                    "subagent_type": WORKER,
                    "isolated": false,
                }).to_string()
            } },
            "SHARED_OK",
        ],
        child_needle: [
            { "tool_call": { "name": "Read", "arguments":
                serde_json::json!({ "file_path": file_name }).to_string()
            } },
            file_content,
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    setup_principal(&cli, agent_name, &mock_url);
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "First Write '{file_name}' with content '{file_content}'. Then \
         spawn a non-isolated subagent to read it. Respond with SHARED_OK. \
         Use the needle '{parent_needle}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("SHARED_OK"),
        "parent did not report SHARED_OK: stdout={out} stderr={err}",
    );
}

/// `subagent_isolation.ps1` TEST 2: isolated child. Parent spawns
/// an `isolated: true` child that writes a marker file. The
/// assertion is that the child wrote the file (proving the
/// dispatch + tool path works) and the parent received the
/// success sentinel.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn subagent_isolation_t2_isolated_writes_file() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let parent_needle = "subagent-iso-t2-iso-s7nv";
    let child_needle = "subagent-iso-t2-iso-c-s7nv";
    let agent_name = "subagent_isolation_t2";
    let file_name = "subagent_isolation_T2_isolated.txt";
    let file_content = "ISOLATED_SUBAGENT_MARKER";

    let task_for_child = format!(
        "Write '{file_name}' with content '{file_content}' via Write. \
         Substring '{child_needle}' for mock routing. \
         (test=subagent_isolation_T2_isolated)"
    );

    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({
                    "prompt": task_for_child,
                    "subagent_type": WORKER,
                    "isolated": true,
                }).to_string()
            } },
            "ISOLATED_OK",
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
        "Spawn an isolated subagent to write a file. When it returns, \
         respond with ISOLATED_OK. Use the needle '{parent_needle}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("ISOLATED_OK"),
        "parent did not report ISOLATED_OK: stdout={out} stderr={err}",
    );

    let path = workspace_dir(&cli).join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!("child file not written at {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}")
    });
    assert_eq!(actual, file_content);
}

/// Flat-file layout variant of `subagent_blocking_t1_write_file`.
/// Verifies that a subagent prompt at `agents/worker.md` is discovered
/// and can be spawned using the file stem as `subagent_type`.
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
                serde_json::json!({ "prompt": task_for_child, "subagent_type": WORKER }).to_string()
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
        &["send", agent_name, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("BLOCKING_SUCCESS"),
        "parent did not report BLOCKING_SUCCESS: stdout={out} stderr={err}",
    );

    let path = workspace_dir(&cli).join(file_name);
    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let dump = dump_data_dir(&cli);
        panic!("child file not written at {path:?}: {e}\ndata dir dump:\n{dump}\nstdout: {out}\nstderr: {err}")
    });
    assert_eq!(actual, file_content);
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
