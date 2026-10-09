//! Daemon end-to-end check for the built-in file and shell tools.
//!
//! One scripted agent turn drives Read → Glob → Grep → Write → Edit → Bash
//! through a real `peko send` → daemon → mock LLM → tool loop. Every step is
//! verified by an effect the mock cannot fake: files written into the
//! principal's tool workspace, or the tool's result persisted in the
//! session transcript. (The mock replies from a fixed script, so a final
//! "done" sentinel alone would prove nothing about tool execution.)
//!
//! Tool semantics are tested in-process: per-tool unit tests next to each
//! implementation, and dispatcher-level tests on
//! `tools::builtin::test_harness::ToolHarness`, which covers all 19 tools
//! under `cargo test --lib`. This file only pins the daemon wiring those
//! cannot reach: workspace resolution (F42), process spawning, and result
//! persistence.
//!
//! Tier: mock-LLM (`make test-integration`; CI sets `MOCK_LLM_URL`). The
//! test early-returns when it is unset so a bare `cargo test` still passes.
//! `#[serial]` because the mock's per-substring counter is global.

mod common;
use common::{
    configure_mock, create_mock_principal_with_tools, run_with_timeout, DaemonGuard, PekoCli,
};
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

fn mock_llm_url() -> Option<String> {
    std::env::var("MOCK_LLM_URL")
        .ok()
        .filter(|url| !url.is_empty())
}

/// Directory the daemon's built-in tools treat as their workspace root:
/// `<PEKO_HOME>/data/workspaces` (see `tools::installation::install_runtime`).
fn workspace_dir(cli: &PekoCli) -> PathBuf {
    cli.peko_dir().join("data").join("workspaces")
}

/// Every persisted `.jsonl` under the data dir, concatenated — the session
/// transcript the tool results were fed back through.
fn transcripts(cli: &PekoCli) -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(&cli.peko_dir().join("data"), &mut out);
    out
}

/// Bounded tree listing of the data dir for failure messages.
fn dump_data_dir(cli: &PekoCli) -> String {
    fn walk(dir: &Path, depth: usize, out: &mut String) {
        if depth > 4 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let indent = "  ".repeat(depth + 1);
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if path.is_dir() {
                out.push_str(&format!("{indent}{name}/\n"));
                walk(&path, depth + 1, out);
            } else {
                out.push_str(&format!("{indent}{name}\n"));
            }
        }
    }
    let mut out = String::new();
    walk(&cli.peko_dir().join("data"), 0, &mut out);
    out
}

fn tool_call(name: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "tool_call": { "name": name, "arguments": arguments.to_string() } })
}

#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn daemon_runs_file_and_shell_tools_in_the_principal_workspace() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    // Markers appear only in seeded files or tool output — never in the
    // prompt or script — so finding them in the transcript proves the tool
    // ran and its result was persisted.
    let needle = "builtin-tools-e2e-5c2e";
    let read_marker = "READ_MARKER_91f3";
    let glob_marker = "glob-hit-4b7a.e2e";
    let grep_marker = "grep-hit-d06c.txt";

    let cli = PekoCli::new();
    let ws = workspace_dir(&cli);
    std::fs::create_dir_all(ws.join("found")).unwrap();
    std::fs::write(ws.join("read-me.txt"), format!("{read_marker}\n")).unwrap();
    std::fs::write(ws.join("found").join(glob_marker), "glob\n").unwrap();
    std::fs::write(ws.join("found").join(grep_marker), "needle GREP_TARGET\n").unwrap();

    let script = serde_json::json!({
        needle: [
            tool_call("Read", serde_json::json!({"file_path": "read-me.txt"})),
            tool_call("Glob", serde_json::json!({"pattern": "**/*.e2e"})),
            tool_call(
                "Grep",
                serde_json::json!({"pattern": "GREP_TARGET", "output_mode": "files_with_matches"}),
            ),
            tool_call("Write", serde_json::json!({"file_path": "out/result.txt", "content": "draft"})),
            tool_call(
                "Edit",
                serde_json::json!({"file_path": "out/result.txt", "old_string": "draft", "new_string": "final"}),
            ),
            tool_call("Bash", serde_json::json!({"command": "printf ran > out/bash.txt"})),
            "TOOLS_DONE",
        ],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;
    create_mock_principal_with_tools(&cli, "builtin_tools_e2e", &mock_url, &[]);
    let _daemon = DaemonGuard::spawn(&cli);

    let (output, _, _) = run_with_timeout(
        || {
            let mut cmd = cli.cmd();
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            cmd
        },
        &[
            "send",
            "builtin_tools_e2e",
            &format!("Run the workspace tool checks. Needle: {needle}"),
        ],
        Duration::from_secs(60),
    )
    .expect("run peko send");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let context = || {
        format!(
            "stdout: {stdout}\nstderr: {stderr}\ndata dir:\n{}",
            dump_data_dir(&cli)
        )
    };
    assert_eq!(
        output.status.code(),
        Some(0),
        "peko send failed\n{}",
        context()
    );
    assert!(
        stdout.contains("TOOLS_DONE"),
        "turn did not finish\n{}",
        context()
    );

    let read = |path: &str| std::fs::read_to_string(ws.join(path)).unwrap_or_default();
    assert_eq!(
        read("out/result.txt"),
        "final",
        "Write then Edit\n{}",
        context()
    );
    assert_eq!(
        read("out/bash.txt"),
        "ran",
        "Bash runs in the workspace\n{}",
        context()
    );

    let transcript = transcripts(&cli);
    for (tool, marker) in [
        ("Read", read_marker),
        ("Glob", glob_marker),
        ("Grep", grep_marker),
    ] {
        assert!(
            transcript.contains(marker),
            "{tool} result {marker:?} missing from the persisted transcript\n{}",
            context()
        );
    }
}
