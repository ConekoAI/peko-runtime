//! CLI integration tests for agent-owned session management (the
//! unified session/run framework — Phase 5b of the coin-model plan).
//!
//! Covers the `session` tool's stable-id lifecycle (no chapters: the
//! live `root:*` id never gains a `#` suffix; paging is
//! storage-internal) and self-delete guard, and the `Agent` tool's
//! spawn → `session list` → `action:"resume"` → `cleanup:"delete"`
//! lifecycle:
//!
//! | Test | What it pins |
//! |------|--------------|
//! | `session_new_refused_live_id_stays_stable` | scripted `session new` returns the demoted-action refusal, writes no `chapters.json`, and the NEXT `peko send` keeps the same live `root:*` id with both turns stitched in one history |
//! | `session_delete_current_session_refused` | `session delete` on the caller's own live session returns the structured refusal and the session survives |
//! | `agent_spawn_list_resume_cleanup_delete` | spawn registers a `trigger=="spawn"` session; `action:"resume"` + `session_key` continues it with history; `cleanup:"delete"` routes through the guarded delete and removes it |
//!
//! Each test drives MULTIPLE sequential `peko send` runs against one
//! daemon, re-scripting the mock LLM between sends
//! (`POST /_test/configure` resets the per-substring counter). Session
//! ids the script needs (the live `root:*` id, the spawned session's
//! uuid) are read from `sessions.json` between sends — the mock cannot
//! see tool results, so every id the script references must be known
//! before the send starts.
//!
//! Tier: mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set). Tests early-return if unset so `cargo test`
//! still passes on a bare checkout.
//!
//! **`#[serial]`.** The mock's per-substring counter is global state
//! across all test binaries; per-test unique needles are
//! belt-and-suspenders.

mod common;
use common::{
    configure_mock, create_mock_principal_with_tools, run_with_timeout, DaemonGuard, PekoCli,
};
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// The `subagent_type` every Agent call spawns.
const WORKER: &str = "worker";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MOCK_LLM_URL` and return Some(url) if set, None otherwise.
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

/// Locate the principal's sessions directory by walking the peko dir
/// for `sessions.json` (`<local_root>/sessions/sessions.json`).
fn sessions_dir(cli: &PekoCli) -> PathBuf {
    fn walk(dir: &Path, depth: usize) -> Option<PathBuf> {
        if depth > 6 {
            return None;
        }
        if dir.join("sessions.json").exists() {
            return Some(dir.to_path_buf());
        }
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(found) = walk(&p, depth + 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(cli.peko_dir().as_path(), 0)
        .unwrap_or_else(|| panic!("no sessions.json found under {}", cli.peko_dir().display()))
}

/// Parse the principal's `sessions.json` into a JSON object keyed by
/// session id.
fn sessions_index(cli: &PekoCli) -> serde_json::Value {
    let path = sessions_dir(cli).join("sessions.json");
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&content).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// The live conversational session id: the `root:` key (ids are
/// stable — no `#` suffix is ever minted).
fn live_root_id(cli: &PekoCli) -> String {
    let index = sessions_index(cli);
    let obj = index.as_object().expect("sessions.json is an object");
    obj.keys()
        .find(|k| k.starts_with("root:") && !k.contains('#'))
        .cloned()
        .unwrap_or_else(|| panic!("no live root session in sessions.json: {obj:?}"))
}

/// Write the `worker` subagent prompt (directory layout).
fn write_worker_subagent(cli: &PekoCli, principal: &str) {
    let dir = cli
        .peko_dir()
        .join("principals")
        .join(principal)
        .join("agents")
        .join(WORKER);
    std::fs::create_dir_all(&dir).expect("create worker subagent dir");
    let agent_md = format!(
        "---\n\
         name: {WORKER}\n\
         description: Test subagent for the cli_session_manage integration suite\n\
         ---\n\n\
         You are a test subagent. Follow the task instructions exactly.\n"
    );
    std::fs::write(dir.join("AGENT.md"), agent_md).expect("write worker AGENT.md");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `session new` no longer rotates anything: the action is demoted
/// (the tool returns a refusal), no `chapters.json` is written, and
/// the next message keeps the same live `root:*` id — paging is
/// storage-internal and never changes the session id.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn session_new_refused_live_id_stays_stable() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let principal = "sess_mgr_new";
    let first_needle = "sessnew-first-m4qx";
    let second_needle = "sessnew-second-m4qx";

    let cli = PekoCli::new();
    create_mock_principal_with_tools(&cli, principal, &mock_url, &["session"]);
    let _daemon = DaemonGuard::spawn(&cli);

    // Send #1: the agent calls `session new` (refused — the action is
    // demoted) and reports.
    let script = serde_json::json!({
        first_needle: [
            { "tool_call": { "name": "session", "arguments":
                serde_json::json!({ "action": "new", "title": "first chapter" }).to_string()
            } },
            "NEW_REFUSED",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let prompt = format!(
        "Start a fresh chapter of this conversation with the session tool, then \
         respond with NEW_REFUSED regardless of the outcome. Use the needle '{first_needle}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", principal, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("NEW_REFUSED"),
        "send #1 did not report NEW_REFUSED: stdout={out} stderr={err}",
    );

    // No pending chapter mechanism exists anymore.
    let dir = sessions_dir(&cli);
    assert!(
        !dir.join("chapters.json").exists(),
        "chapters.json must never be written (chapters are gone)"
    );
    let live_before = live_root_id(&cli);

    // Send #2: an ordinary message on the same live session.
    let script = serde_json::json!({ second_needle: ["SECOND_TURN_OK"] }).to_string();
    configure_mock(&mock_url, &script).await;

    let prompt = format!("Say SECOND_TURN_OK. Use the needle '{second_needle}'.");
    let (out, err, status) = run(
        &cli,
        &["send", principal, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("SECOND_TURN_OK"),
        "send #2 did not report SECOND_TURN_OK: stdout={out} stderr={err}",
    );

    // Id stability: the live id is unchanged and no `#`-suffixed
    // session was minted by either send.
    let index = sessions_index(&cli);
    let obj = index.as_object().unwrap();
    assert!(
        obj.contains_key(&live_before),
        "live session id {live_before} should be untouched: {obj:?}"
    );
    assert!(
        !obj.keys().any(|k| k.contains('#')),
        "no `#`-suffixed session id may ever appear: {obj:?}"
    );

    // Continuity: one stitched history holds both turns. If the
    // transcript paged under a low `rotate_bytes` test override, the
    // older page (`<id>.<n>.jsonl`) carries send #1 — either way no
    // `#` id is involved.
    let safe = |id: &str| id.replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "-");
    let stem = safe(&live_before);
    let mut stitched = String::new();
    for entry in std::fs::read_dir(&dir)
        .expect("read sessions dir")
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == format!("{stem}.jsonl")
            || (name.starts_with(&format!("{stem}.")) && name.ends_with(".jsonl"))
        {
            stitched.push_str(
                &std::fs::read_to_string(entry.path())
                    .unwrap_or_else(|e| panic!("read {}: {e}", entry.path().display())),
            );
        }
    }
    assert!(
        stitched.contains("NEW_REFUSED") && stitched.contains("SECOND_TURN_OK"),
        "the stable-id history (pages + current) must hold both turns"
    );
}

/// `session delete` on the caller's own current session returns the
/// structured self-guard refusal; the session survives.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn session_delete_current_session_refused() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let principal = "sess_mgr_del";
    let first_needle = "sessdel-first-b7wz";
    let second_needle = "sessdel-second-b7wz";

    let cli = PekoCli::new();
    create_mock_principal_with_tools(&cli, principal, &mock_url, &["session"]);
    let _daemon = DaemonGuard::spawn(&cli);

    // Send #1: establish the live session.
    let script = serde_json::json!({ first_needle: ["FIRST_OK"] }).to_string();
    configure_mock(&mock_url, &script).await;
    let prompt = format!("Say FIRST_OK. Use the needle '{first_needle}'.");
    let (out, err, status) = run(
        &cli,
        &["send", principal, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(out.contains("FIRST_OK"), "stdout={out} stderr={err}");

    let live_id = live_root_id(&cli);

    // Send #2: the agent tries to delete the session it is running in.
    // The tool returns the structured refusal as the tool result; the
    // parent then reports.
    let script = serde_json::json!({
        second_needle: [
            { "tool_call": { "name": "session", "arguments":
                serde_json::json!({ "action": "delete", "session_key": live_id }).to_string()
            } },
            "DELETE_REFUSED",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;
    let prompt = format!(
        "Delete the current session with the session tool, then respond with \
         DELETE_REFUSED regardless of the outcome. Use the needle '{second_needle}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", principal, &prompt, "--no-stream"],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("DELETE_REFUSED"),
        "send #2 did not report DELETE_REFUSED: stdout={out} stderr={err}",
    );

    // The live session survived the refused delete.
    let index = sessions_index(&cli);
    assert!(
        index.as_object().unwrap().contains_key(&live_id),
        "live session {live_id} must survive the refused delete"
    );
    let safe = live_id.replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "-");
    assert!(
        sessions_dir(&cli).join(format!("{safe}.jsonl")).exists(),
        "live transcript must survive the refused delete"
    );
}

/// Full Agent-tool lifecycle on the coin model: spawn registers a
/// `trigger == "spawn"` session → `action:"resume"` re-attaches with
/// history → `cleanup: "delete"` removes it through the guarded
/// delete.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn agent_spawn_list_resume_cleanup_delete() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let principal = "sess_mgr_agent";
    let p1 = "sessagent-p1-c2vd";
    let c1 = "sessagent-c1-c2vd";
    let p2 = "sessagent-p2-c2vd";
    let c2 = "sessagent-c2-c2vd";
    let p3 = "sessagent-p3-c2vd";
    let c3 = "sessagent-c3-c2vd";

    let cli = PekoCli::new();
    create_mock_principal_with_tools(
        &cli,
        principal,
        &mock_url,
        &["Agent", "Write", "Read", "session", WORKER],
    );
    write_worker_subagent(&cli, principal);
    let _daemon = DaemonGuard::spawn(&cli);

    // ── Send #1: spawn (cleanup keep) + list ────────────────────────
    let script = serde_json::json!({
        p1: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": format!("Do part one. Needle '{c1}'."), "subagent_type": WORKER }).to_string()
            } },
            { "tool_call": { "name": "session", "arguments":
                serde_json::json!({ "action": "list" }).to_string()
            } },
            "SPAWN_DONE",
        ],
        c1: ["CHILD_ONE_DONE"],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let prompt = format!(
        "Spawn a subagent for a task (its instructions are in your system prompt), \
         then list your sessions with the session tool, then respond with SPAWN_DONE. \
         Use the needle '{p1}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", principal, &prompt, "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("SPAWN_DONE"),
        "send #1 did not report SPAWN_DONE: stdout={out} stderr={err}",
    );

    // The spawned session exists with trigger "spawn".
    let index = sessions_index(&cli);
    let spawn_id = index
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, v)| v["trigger"] == "spawn")
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| panic!("no spawned session in index: {index:?}"));

    // ── Send #2: resume with history (cleanup keep) ─────────────────
    let script = serde_json::json!({
        p2: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({
                    "action": "resume",
                    "session_key": spawn_id,
                    "prompt": format!("Do part two. Needle '{c2}'."),
                    "subagent_type": WORKER,
                }).to_string()
            } },
            "RESUME_DONE",
        ],
        c2: ["CHILD_TWO_DONE"],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let prompt = format!(
        "Re-attach the previous subagent session to continue its task, then respond \
         with RESUME_DONE. Use the needle '{p2}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", principal, &prompt, "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("RESUME_DONE"),
        "send #2 did not report RESUME_DONE: stdout={out} stderr={err}",
    );

    // History preserved across the resume: the session transcript
    // holds BOTH child turns.
    let transcript = std::fs::read_to_string(sessions_dir(&cli).join(format!("{spawn_id}.jsonl")))
        .expect("spawned session transcript exists after resume");
    assert!(
        transcript.contains("CHILD_ONE_DONE") && transcript.contains("CHILD_TWO_DONE"),
        "resumed session must keep its full prior history"
    );

    // ── Send #3: resume again with cleanup:"delete" ─────────────────
    let script = serde_json::json!({
        p3: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({
                    "action": "resume",
                    "session_key": spawn_id,
                    "prompt": format!("Final part. Needle '{c3}'."),
                    "subagent_type": WORKER,
                    "cleanup": "delete",
                }).to_string()
            } },
            "CLEANUP_DONE",
        ],
        c3: ["CHILD_THREE_DONE"],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let prompt = format!(
        "Re-attach the same subagent session for one last task and delete the \
         session afterwards (cleanup delete), then respond with CLEANUP_DONE. \
         Use the needle '{p3}'."
    );
    let (out, err, status) = run(
        &cli,
        &["send", principal, &prompt, "--no-stream"],
        Duration::from_secs(45),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("CLEANUP_DONE"),
        "send #3 did not report CLEANUP_DONE: stdout={out} stderr={err}",
    );

    // cleanup:"delete" routed through the guarded delete: the session
    // is gone from the index and the transcript is removed.
    let index = sessions_index(&cli);
    assert!(
        !index.as_object().unwrap().contains_key(&spawn_id),
        "deleted child session must leave the index: {index:?}"
    );
    assert!(
        !sessions_dir(&cli)
            .join(format!("{spawn_id}.jsonl"))
            .exists(),
        "deleted child session transcript must be removed"
    );
}
