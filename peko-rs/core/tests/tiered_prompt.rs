//! CLI integration tests for ADR-052 (tiered system prompt:
//! T0 principal / T1 role / T2 instance), driven end-to-end against the
//! real daemon + CLI + mock LLM. Model on `cli_subagent.rs`.
//!
//! | Test                                   | ADR-052 slice                                          |
//! |----------------------------------------|--------------------------------------------------------|
//! | `d3_role_prompt_reaches_spawned_child` | D3 — named subagent runs its own role body (T1)        |
//! | `d4_principal_identity_section`        | D4 — `[identity]`/`[intent]` ride the tail (T0)        |
//! | `d5_self_position_section`             | D5 — the agent's own session position (T2)             |
//! | `peer_agent_role_for_peer_turns`       | D3 — `routing.peer_agent` role for peer-facing turns   |
//! | `d5_agents_md_project_context`         | D5 — focus-dir AGENTS.md project instructions (T2)     |
//! | `d2_memory_update_notice`              | D2 — changed tail section re-injects with a notice     |
//! | `d6_workspace_hook_prompt_section`     | D6 — `PromptSection` workspace-hook bind               |
//!
//! **Where the prompts are observed.** The frozen system prompt is
//! deliberately NOT persisted to the session JSONL (the legacy
//! `add_system` path is gone — see `engine/src/agentic_loop.rs`), so
//! the T1 assertions (tests 1 + 4) can't read it back from disk. Those
//! tests instead interpose a tiny in-process recording proxy between the
//! daemon and the mock LLM: the principal's catalog entry points at the
//! proxy (via a `MOCK_LLM_URL` env override — `DaemonGuard` re-seeds
//! the catalog from that env var at spawn), the proxy forwards every
//! request to the real mock and records the raw request bodies. The
//! assertions then inspect the actual wire payload the provider saw.
//! The tail `<runtime-context>` sections (tests 2, 3, 5, 6, 7) ARE
//! persisted as user messages, so those assert on the session JSONL.
//!
//! Tier: mock-LLM (CI runs against the docker-compose stack with
//! `MOCK_LLM_URL` set). Tests early-return if unset so `cargo test`
//! still passes on a bare checkout. All tests are `#[serial]` because
//! they share the mock's per-substring counter and (tests 1 + 4) the
//! process-wide `MOCK_LLM_URL` env var.

mod common;
use common::{
    configure_mock, create_mock_principal_with_tools, run_with_timeout, DaemonGuard, PekoCli,
};
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared helpers
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

/// `peko send <principal> <prompt>` and assert the run succeeds and the
/// scripted final text lands on stdout.
fn send_and_assert(cli: &PekoCli, principal: &str, prompt: &str, expect: &str, secs: u64) {
    let (out, err, status) = run(cli, &["send", principal, prompt], Duration::from_secs(secs));
    assert_ok(&out, &err, &status);
    assert!(
        out.contains(expect),
        "send did not report {expect}: stdout={out} stderr={err}",
    );
}

/// The principal workspace root (`<peko_dir>/principals/<name>/`).
fn workspace(cli: &PekoCli, principal: &str) -> PathBuf {
    cli.peko_dir().join("principals").join(principal)
}

/// Parse + patch + rewrite `principal.toml` (mirrors the capability
/// patch in `common::agent::create_mock_principal_with_tools`).
fn patch_principal_config(
    cli: &PekoCli,
    principal: &str,
    f: impl FnOnce(&mut peko_core::principal::config::PrincipalConfig),
) {
    let path = workspace(cli, principal).join("principal.toml");
    let raw = std::fs::read_to_string(&path).expect("read principal.toml");
    let mut cfg: peko_core::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    f(&mut cfg);
    std::fs::write(
        &path,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");
}

/// Overwrite the principal's root persona (`agents/root.md`) with a
/// custom body containing `marker`.
fn write_root_persona(cli: &PekoCli, principal: &str, marker: &str) {
    let path = workspace(cli, principal).join("agents").join("root.md");
    let body = format!(
        "---\n\
         name: root\n\
         description: Custom root persona for the tiered-prompt suite\n\
         ---\n\n\
         You are the root agent of a test principal. Persona marker: {marker}.\n"
    );
    std::fs::write(&path, body).expect("write agents/root.md");
}

/// Write a named role file (`agents/<role>.md`) whose body contains `marker`.
fn write_role(cli: &PekoCli, principal: &str, role: &str, marker: &str) {
    let dir = workspace(cli, principal).join("agents");
    std::fs::create_dir_all(&dir).expect("create agents dir");
    let body = format!(
        "---\n\
         name: {role}\n\
         description: Test role for the tiered-prompt suite\n\
         ---\n\n\
         You are the {role} role. Role marker: {marker}.\n"
    );
    std::fs::write(dir.join(format!("{role}.md")), body).expect("write role file");
}

// ---------------------------------------------------------------------------
// Session JSONL inspection
// ---------------------------------------------------------------------------

/// Recursively collect every session JSONL under `root` (any path with a
/// `sessions` component, extension `.jsonl`).
fn collect_session_jsonls(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let entries = match std::fs::read_dir(root) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_session_jsonls(&path, depth + 1, out);
        } else if path.extension().is_some_and(|e| e == "jsonl")
            && path.components().any(|c| c.as_os_str() == "sessions")
        {
            out.push(path);
        }
    }
}

/// Extract all text content from the `message.v2` events of one JSONL
/// page, JSON-unescaped (raw substring search on the file would trip on
/// `\"` escapes inside the serialized text).
fn jsonl_message_text(path: &Path) -> String {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = String::new();
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("message.v2") {
            continue;
        }
        collect_strings(&value, &mut out);
    }
    out
}

fn collect_strings(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

/// Concatenated, JSON-unescaped message text of every session JSONL
/// under the test's peko dir whose contents mention `needle` (the
/// needle routes us to the session that served the turn).
fn session_text_for_needle(cli: &PekoCli, needle: &str) -> String {
    let mut jsonls = Vec::new();
    collect_session_jsonls(&cli.peko_dir(), 0, &mut jsonls);
    let mut out = String::new();
    for path in &jsonls {
        let raw = std::fs::read_to_string(path).unwrap_or_default();
        if raw.contains(needle) {
            out.push_str(&jsonl_message_text(path));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Recording proxy (tests 1 + 4 — the frozen system prompt never touches
// the session JSONL, so we observe it on the wire instead)
// ---------------------------------------------------------------------------

type Recordings = Arc<Mutex<Vec<serde_json::Value>>>;

/// Bind a throwaway HTTP proxy on 127.0.0.1 that records every request
/// body (parsed as JSON) and forwards to the real mock LLM, buffering
/// the upstream SSE response and relaying it with `connection: close`.
/// Returns (proxy base URL, shared recording buffer).
async fn start_recording_proxy(mock_url: &str) -> (String, Recordings) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind recording proxy");
    let port = listener.local_addr().expect("proxy local addr").port();
    let recordings: Recordings = Arc::new(Mutex::new(Vec::new()));
    let mock = mock_url.trim_end_matches('/').to_string();
    let rec = Arc::clone(&recordings);
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let mock = mock.clone();
            let rec = Arc::clone(&rec);
            tokio::spawn(async move {
                let _ = proxy_connection(socket, &mock, rec).await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), recordings)
}

async fn proxy_connection(
    mut socket: tokio::net::TcpStream,
    mock: &str,
    rec: Recordings,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Read the request head (up to \r\n\r\n), one byte at a time —
    // requests are small and this keeps the parser trivial.
    let mut head = Vec::with_capacity(4096);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < 256 * 1024, "request head too large");
        socket.read_exact(&mut byte).await?;
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("POST").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let content_length: usize = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    socket.read_exact(&mut body).await?;

    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) {
        rec.lock().expect("recordings poisoned").push(json);
    }

    let client = reqwest::Client::new();
    let upstream = tokio::time::timeout(
        Duration::from_secs(30),
        client
            .request(
                method.parse().unwrap_or(reqwest::Method::POST),
                format!("{mock}{path}"),
            )
            .header("content-type", "application/json")
            .body(body)
            .send(),
    )
    .await??;
    let status = upstream.status();
    let resp_body = upstream.bytes().await?;
    let head_out = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        resp_body.len()
    );
    socket.write_all(head_out.as_bytes()).await?;
    socket.write_all(&resp_body).await?;
    socket.shutdown().await?;
    Ok(())
}

/// Extract the text of one wire message (`content` may be a plain
/// string or an array of parts).
fn wire_message_text(msg: &serde_json::Value) -> String {
    match msg.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Concatenated text of all system messages in one recorded request.
fn request_system_text(req: &serde_json::Value) -> String {
    req.get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                .map(wire_message_text)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Text of the first user message in one recorded request (the mock
/// routes on the same field, so this is the needle carrier).
fn request_first_user_text(req: &serde_json::Value) -> String {
    req.get("messages")
        .and_then(|m| m.as_array())
        .and_then(|msgs| {
            msgs.iter()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        })
        .map(wire_message_text)
        .unwrap_or_default()
}

/// Find the recorded request whose first user message carries `needle`.
fn request_for_needle(rec: &Recordings, needle: &str) -> serde_json::Value {
    let requests = rec.lock().expect("recordings poisoned");
    requests
        .iter()
        .find(|req| request_first_user_text(req).contains(needle))
        .unwrap_or_else(|| {
            panic!(
                "no recorded LLM request carries needle '{needle}' \
                 ({} requests recorded)",
                requests.len()
            )
        })
        .clone()
}

/// RAII guard for the process-wide `MOCK_LLM_URL` override the proxy
/// tests need (`DaemonGuard::spawn` re-seeds the catalog from this env
/// var, so pointing it at the proxy is what routes the daemon's LLM
/// traffic through the recorder). Restores the previous value on drop.
struct MockUrlOverride(Option<String>);

impl MockUrlOverride {
    fn set(new: &str) -> Self {
        let prev = std::env::var("MOCK_LLM_URL").ok();
        std::env::set_var("MOCK_LLM_URL", new);
        Self(prev)
    }
}

impl Drop for MockUrlOverride {
    fn drop(&mut self) {
        match &self.0 {
            Some(prev) => std::env::set_var("MOCK_LLM_URL", prev),
            None => std::env::remove_var("MOCK_LLM_URL"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// ADR-052 D3: an Agent-tool spawn with `agent: "researcher"` runs the
/// researcher role body as its system prompt (T1), not the root persona.
///
/// The frozen system prompt isn't persisted to the session JSONL, so
/// the assertions read the recorded wire requests: the child's request
/// (routed by the child needle embedded in the Agent `prompt` arg) must
/// carry ROLE_MARKER in its system message and NOT ROOT_PERSONA_MARKER;
/// the parent's request carries the root persona.
///
/// Multi-thread runtime: `peko send` blocks the test thread
/// synchronously while the recording-proxy task must keep accepting
/// connections — a current-thread runtime would starve it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn d3_role_prompt_reaches_spawned_child() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let real_mock = mock_llm_url().unwrap();

    let parent_needle = "tiered-d3-parent-k4x9";
    let child_needle = "tiered-d3-child-k4x9";
    let principal = "tiered_d3_role";

    let task_for_child = format!(
        "You are researching a topic. Reply with D3_CHILD_DONE. \
         The substring '{child_needle}' routes your LLM call at the mock. \
         (test=d3_role_prompt_reaches_spawned_child)"
    );
    let script = serde_json::json!({
        parent_needle: [
            { "tool_call": { "name": "Agent", "arguments":
                serde_json::json!({ "prompt": task_for_child, "agent": "researcher", "path": "d3-research" }).to_string()
            } },
            "D3_PARENT_DONE",
        ],
        child_needle: ["D3_CHILD_DONE"],
    })
    .to_string();
    configure_mock(&real_mock, &script).await;

    let (proxy_url, recordings) = start_recording_proxy(&real_mock).await;
    let _mock_override = MockUrlOverride::set(&proxy_url);

    let cli = PekoCli::new();
    create_mock_principal_with_tools(
        &cli,
        principal,
        &proxy_url,
        &["Agent", "Bash", "agent:researcher"],
    );
    write_root_persona(&cli, principal, "ROOT_PERSONA_MARKER_D3QX7");
    write_role(&cli, principal, "researcher", "ROLE_MARKER_D3QX7");
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Spawn the researcher subagent for the task described in your context, \
         then report D3_PARENT_DONE. Use the needle '{parent_needle}'."
    );
    send_and_assert(&cli, principal, &prompt, "D3_PARENT_DONE", 60);

    // The child's wire request: system prompt = researcher role body.
    let child_req = request_for_needle(&recordings, child_needle);
    let child_system = request_system_text(&child_req);
    assert!(
        child_system.contains("ROLE_MARKER_D3QX7"),
        "spawned child's system prompt must carry the researcher role body; got:\n{child_system}"
    );
    assert!(
        !child_system.contains("ROOT_PERSONA_MARKER_D3QX7"),
        "spawned child's system prompt must NOT be the root persona; got:\n{child_system}"
    );

    // Cross-check: the parent (peer-child root agent) kept the root persona.
    let parent_req = request_for_needle(&recordings, parent_needle);
    let parent_system = request_system_text(&parent_req);
    assert!(
        parent_system.contains("ROOT_PERSONA_MARKER_D3QX7"),
        "parent's system prompt must carry the root persona; got:\n{parent_system}"
    );
}

/// ADR-052 D4 (T0): `[identity]` + `[intent]` from `principal.toml`
/// render as a `## Principal identity` section of the tail
/// `<runtime-context>` user message.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn d4_principal_identity_section() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "tiered-d4-ident-m2v8";
    let principal = "tiered_d4_identity";

    let script = serde_json::json!({ needle: ["D4_TURN_OK"] }).to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    create_mock_principal_with_tools(&cli, principal, &mock_url, &[]);
    patch_principal_config(&cli, principal, |cfg| {
        cfg.identity.display_name = Some("IDENT_NAME_MARKER_D4QX7".to_string());
        cfg.identity.description = Some("IDENT_DESC_MARKER_D4QX7".to_string());
        cfg.intent.goals = vec!["GOAL_MARKER_D4QX7".to_string()];
        cfg.intent.values = vec!["VALUE_MARKER_D4QX7".to_string()];
        cfg.intent.preferences = vec!["PREF_MARKER_D4QX7".to_string()];
    });
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!("Say D4_TURN_OK. Use the needle '{needle}'.");
    send_and_assert(&cli, principal, &prompt, "D4_TURN_OK", 30);

    let session_text = session_text_for_needle(&cli, needle);
    assert!(
        !session_text.is_empty(),
        "no session JSONL found carrying needle '{needle}'"
    );
    for marker in [
        "<runtime-context>",
        "## Principal identity",
        "IDENT_NAME_MARKER_D4QX7",
        "IDENT_DESC_MARKER_D4QX7",
        "GOAL_MARKER_D4QX7",
        "VALUE_MARKER_D4QX7",
        "PREF_MARKER_D4QX7",
    ] {
        assert!(
            session_text.contains(marker),
            "peer-child session missing '{marker}'\n--- session text ---\n{session_text}"
        );
    }
}

/// ADR-052 D5 (T2): the tail session-context section renders the
/// agent's OWN position in the session tree — for a `peko send` turn
/// that's the standing peer child of the trunk.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn d5_self_position_section() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "tiered-d5-selfpos-p6w3";
    let principal = "tiered_d5_selfpos";

    let script = serde_json::json!({ needle: ["D5_TURN_OK"] }).to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    create_mock_principal_with_tools(&cli, principal, &mock_url, &[]);
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!("Say D5_TURN_OK. Use the needle '{needle}'.");
    send_and_assert(&cli, principal, &prompt, "D5_TURN_OK", 30);

    let session_text = session_text_for_needle(&cli, needle);
    assert!(
        !session_text.is_empty(),
        "no session JSONL found carrying needle '{needle}'"
    );
    for marker in ["your position:", "standing peer child"] {
        assert!(
            session_text.contains(marker),
            "peer-child session missing '{marker}'\n--- session text ---\n{session_text}"
        );
    }
}

/// ADR-052 D3 (peer turns): `routing.peer_agent = "comms"` makes
/// peer-facing turns run the comms role body instead of the root
/// persona. Observed on the wire via the recording proxy (the frozen
/// system prompt isn't persisted to the session JSONL). Multi-thread
/// runtime for the same reason as `d3_role_prompt_reaches_spawned_child`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn peer_agent_role_for_peer_turns() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let real_mock = mock_llm_url().unwrap();

    let needle = "tiered-peeragent-r8t5";
    let principal = "tiered_peer_agent";

    let script = serde_json::json!({ needle: ["PEER_AGENT_OK"] }).to_string();
    configure_mock(&real_mock, &script).await;

    let (proxy_url, recordings) = start_recording_proxy(&real_mock).await;
    let _mock_override = MockUrlOverride::set(&proxy_url);

    let cli = PekoCli::new();
    create_mock_principal_with_tools(&cli, principal, &proxy_url, &[]);
    write_root_persona(&cli, principal, "ROOT_MARKER_PAQX7");
    write_role(&cli, principal, "comms", "COMMS_MARKER_PAQX7");
    patch_principal_config(&cli, principal, |cfg| {
        cfg.routing.peer_agent = Some("comms".to_string());
    });
    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!("Say PEER_AGENT_OK. Use the needle '{needle}'.");
    send_and_assert(&cli, principal, &prompt, "PEER_AGENT_OK", 30);

    let req = request_for_needle(&recordings, needle);
    let system = request_system_text(&req);
    assert!(
        system.contains("COMMS_MARKER_PAQX7"),
        "peer turn must run the peer_agent (comms) role body; got:\n{system}"
    );
    assert!(
        !system.contains("ROOT_MARKER_PAQX7"),
        "peer turn must NOT run the root persona when peer_agent is set; got:\n{system}"
    );
}

/// ADR-052 D5 (T2): after a Bash call lands in a project directory, the
/// nearest AGENTS.md renders as a `## Project instructions (<path>)`
/// tail section on the next iteration.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn d5_agents_md_project_context() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "tiered-d5-projctx-n9b2";
    let principal = "tiered_d5_project";

    let cli = PekoCli::new();
    create_mock_principal_with_tools(&cli, principal, &mock_url, &["Bash"]);
    let project_dir = workspace(&cli, principal).join("project");
    std::fs::create_dir_all(&project_dir).expect("create project dir");
    std::fs::write(
        project_dir.join("AGENTS.md"),
        "# Project rules\n\nAlways run tests before committing. \
         Rule marker: PROJECT_RULE_MARKER_D5QX7.\n",
    )
    .expect("write project AGENTS.md");

    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "Bash", "arguments":
                serde_json::json!({ "command": "ls", "cwd": project_dir }).to_string()
            } },
            "PROJECT_CTX_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "List the project directory as instructed, then say PROJECT_CTX_DONE. \
         Use the needle '{needle}'."
    );
    send_and_assert(&cli, principal, &prompt, "PROJECT_CTX_DONE", 45);

    let session_text = session_text_for_needle(&cli, needle);
    assert!(
        !session_text.is_empty(),
        "no session JSONL found carrying needle '{needle}'"
    );
    for marker in ["## Project instructions (", "PROJECT_RULE_MARKER_D5QX7"] {
        assert!(
            session_text.contains(marker),
            "peer-child session missing '{marker}'\n--- session text ---\n{session_text}"
        );
    }
}

/// ADR-052 D2: when a tier's rendered text changes between iterations,
/// the section is re-injected with an explicit
/// `_Updated — replaces the previous "memory" section._` notice.
///
/// Note on scope: `RuntimeContextState` is per-run (a fresh
/// `RuntimeContextState::default()` per `run_inner`), so a change
/// between two separate `peko send`s re-injects the section PLAIN on
/// the new run's first iteration — the notice semantics only fire
/// within one run. This test therefore rewrites MEMORY.md mid-run via
/// a scripted Bash call: iteration 1 injects `memory-v1`, the tool call
/// swaps the file, iteration 2 detects the change and re-injects with
/// the update notice.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn d2_memory_update_notice() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "tiered-d2-memupd-s4f7";
    let principal = "tiered_d2_memory";

    let cli = PekoCli::new();
    create_mock_principal_with_tools(&cli, principal, &mock_url, &["Bash"]);
    let memory_path = workspace(&cli, principal).join("MEMORY.md");
    std::fs::write(&memory_path, "memory-v1-marker\n").expect("seed MEMORY.md v1");

    let rewrite = format!("printf 'memory-v2-marker\\n' > '{}'", memory_path.display());
    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "Bash", "arguments":
                serde_json::json!({ "command": rewrite }).to_string()
            } },
            "MEM_V2_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!(
        "Update your memory file as instructed, then say MEM_V2_DONE. \
         Use the needle '{needle}'."
    );
    send_and_assert(&cli, principal, &prompt, "MEM_V2_DONE", 45);

    let session_text = session_text_for_needle(&cli, needle);
    assert!(
        !session_text.is_empty(),
        "no session JSONL found carrying needle '{needle}'"
    );
    for marker in [
        "memory-v1-marker",
        "memory-v2-marker",
        "_Updated — replaces the previous \"memory\" section._",
    ] {
        assert!(
            session_text.contains(marker),
            "peer-child session missing '{marker}'\n--- session text ---\n{session_text}"
        );
    }
}

/// ADR-052 D6: a workspace hook bound to `PromptSection` contributes a
/// named `## <section>` tail section whose body is the command's stdout.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn d6_workspace_hook_prompt_section() {
    if mock_llm_url().is_none() {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    }
    let mock_url = mock_llm_url().unwrap();

    let needle = "tiered-d6-hook-h3j6";
    let principal = "tiered_d6_hook";

    let cli = PekoCli::new();
    create_mock_principal_with_tools(&cli, principal, &mock_url, &[]);

    // Modelled on the end-to-end test in
    // `src/extensions/workspace_hooks.rs`: a /bin/sh script whose stdout
    // becomes the section body, plus a hook.toml binding it.
    let hook_dir = workspace(&cli, principal).join("hooks").join("standup");
    std::fs::create_dir_all(&hook_dir).expect("create hook dir");
    let script_path = hook_dir.join("section.sh");
    std::fs::write(
        &script_path,
        "#!/bin/sh\necho 'Standup notes: hook section marker HOOK_SECTION_MARKER_D6QX7.'\n",
    )
    .expect("write hook script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&script_path)
            .expect("stat hook script")
            .permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&script_path, perm).expect("chmod hook script");
    }
    let manifest = format!(
        "binds = [{{ point = \"PromptSection\", section = \"standup-notes\" }}]\n\
         command = \"{}\"\n\
         args = []\n\
         output = \"text\"\n",
        script_path.display().to_string().replace('\\', "\\\\")
    );
    std::fs::write(hook_dir.join("hook.toml"), manifest).expect("write hook.toml");

    let script = serde_json::json!({ needle: ["D6_TURN_OK"] }).to_string();
    configure_mock(&mock_url, &script).await;

    let _daemon = DaemonGuard::spawn(&cli);

    let prompt = format!("Say D6_TURN_OK. Use the needle '{needle}'.");
    send_and_assert(&cli, principal, &prompt, "D6_TURN_OK", 30);

    let session_text = session_text_for_needle(&cli, needle);
    assert!(
        !session_text.is_empty(),
        "no session JSONL found carrying needle '{needle}'"
    );
    for marker in ["## standup-notes", "HOOK_SECTION_MARKER_D6QX7"] {
        assert!(
            session_text.contains(marker),
            "peer-child session missing '{marker}'\n--- session text ---\n{session_text}"
        );
    }
}
