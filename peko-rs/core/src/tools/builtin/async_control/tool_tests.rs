//! Async action parameters through the production dispatcher and the
//! real `AsyncExecutor` (`ToolHarness`), spawning deterministic stub
//! tools so the tests do not depend on a platform shell.

use crate::tools::builtin::test_harness::ToolHarness;
use async_trait::async_trait;
use peko_tools_core::Tool;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Returns `{"stdout": "1\n2\n…\nN"}` — the shape `tail_lines` trims.
struct Emit;

#[async_trait]
impl Tool for Emit {
    fn name(&self) -> &str {
        "Emit"
    }
    fn description(&self) -> String {
        "test stub: numbered stdout lines".into()
    }
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let n = params["lines"].as_u64().unwrap_or(5);
        let stdout: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        Ok(json!({"stdout": stdout.join("\n"), "exit_code": 0}))
    }
}

/// Sleeps for `ms` milliseconds, then returns.
struct Sleep;

#[async_trait]
impl Tool for Sleep {
    fn name(&self) -> &str {
        "Sleep"
    }
    fn description(&self) -> String {
        "test stub: sleep".into()
    }
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let ms = params["ms"].as_u64().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(json!({"slept_ms": ms}))
    }
}

async fn harness() -> ToolHarness {
    let harness = ToolHarness::new().await;
    harness.register(Arc::new(Emit)).await;
    harness.register(Arc::new(Sleep)).await;
    harness
}

async fn spawn(harness: &ToolHarness, extra: Value) -> String {
    let mut params = json!({"action":"spawn"});
    for (key, value) in extra.as_object().unwrap() {
        params[key] = value.clone();
    }
    let receipt = harness.call("Async", params).await.ok();
    receipt["task_id"]
        .as_str()
        .expect("receipt task_id")
        .to_string()
}

async fn async_call(harness: &ToolHarness, action: &str, task_id: &str, extra: Value) -> Value {
    let mut params = json!({"action": action, "task_id": task_id});
    for (key, value) in extra.as_object().unwrap() {
        params[key] = value.clone();
    }
    harness.call("Async", params).await.ok()
}

#[tokio::test]
async fn output_tails_stdout_and_status_reports_label() {
    let harness = harness().await;
    let id = spawn(
        &harness,
        json!({"tool":"Emit", "params":{"lines":5}, "label":"numbers"}),
    )
    .await;

    let full = async_call(&harness, "output", &id, json!({"block":true})).await;
    assert_eq!(full["is_terminal"], true, "{full}");
    assert_eq!(full["result"]["stdout"], "1\n2\n3\n4\n5");
    assert_eq!(
        full["result"]["exit_code"], 0,
        "non-stdout fields pass through"
    );

    let tail = async_call(&harness, "output", &id, json!({"tail_lines":2})).await;
    assert_eq!(tail["result"]["stdout"], "4\n5");
    let wide = async_call(&harness, "output", &id, json!({"tail_lines":50})).await;
    assert_eq!(
        wide["result"]["stdout"], "1\n2\n3\n4\n5",
        "tail beyond length keeps all"
    );

    let status = async_call(&harness, "status", &id, json!({})).await;
    assert_eq!(status["label"], "numbers");
    assert_eq!(status["tool_name"], "Emit");
    assert_eq!(status["is_terminal"], true);
    assert_eq!(status["status"], "completed");
}

#[tokio::test]
async fn timeout_secs_bounds_the_task_lifetime() {
    let harness = harness().await;
    let id = spawn(
        &harness,
        json!({"tool":"Sleep", "params":{"ms":30000}, "timeout_secs":1}),
    )
    .await;
    let out = async_call(
        &harness,
        "output",
        &id,
        json!({"block":true, "timeout":10000}),
    )
    .await;
    assert_eq!(out["is_terminal"], true, "{out}");
    assert_eq!(out["status"], "timed_out", "{out}");
}

#[tokio::test]
async fn running_tasks_report_partial_output_and_stop_is_idempotent() {
    let harness = harness().await;
    let id = spawn(&harness, json!({"tool":"Sleep", "params":{"ms":30000}})).await;

    let polled = async_call(&harness, "output", &id, json!({})).await;
    assert_eq!(polled["is_terminal"], false, "{polled}");
    assert!(polled["result"].is_null());

    let waited = async_call(
        &harness,
        "output",
        &id,
        json!({"block":true, "timeout":100}),
    )
    .await;
    assert_eq!(
        waited["is_terminal"], false,
        "block honours its timeout: {waited}"
    );

    let stopped = async_call(&harness, "stop", &id, json!({})).await;
    assert_eq!(stopped["success"], true, "{stopped}");
    assert_eq!(stopped["already_terminal"], false);
    let status = async_call(&harness, "status", &id, json!({})).await;
    assert_eq!(status["status"], "cancelled", "{status}");

    let again = async_call(&harness, "stop", &id, json!({})).await;
    assert_eq!(again["success"], true, "{again}");
    assert_eq!(again["already_terminal"], true, "second stop is a no-op");
}

#[tokio::test]
async fn list_filters_by_status_and_tool() {
    let harness = harness().await;
    let done = spawn(&harness, json!({"tool":"Emit", "params":{}})).await;
    async_call(&harness, "output", &done, json!({"block":true})).await;
    let running = spawn(&harness, json!({"tool":"Sleep", "params":{"ms":30000}})).await;

    // `list` also merges the process-wide background registry (background
    // Bash, subagents), which sibling tests populate concurrently. Assert
    // the filter holds for every entry and classifies this test's tasks.
    let list = |filter: Value| {
        let mut params = json!({"action":"list"});
        for (key, value) in filter.as_object().unwrap() {
            params[key] = value.clone();
        }
        harness.call("Async", params)
    };
    let entries = |list: &Value| -> Vec<(String, String, String)> {
        list["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["task_id"].as_str().unwrap().to_string(),
                    t["status"].as_str().unwrap().to_string(),
                    t["tool_name"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let has = |rows: &[(String, String, String)], id: &str| rows.iter().any(|r| r.0 == id);

    let completed = entries(&list(json!({"status_filter":"completed"})).await.ok());
    assert!(
        completed.iter().all(|r| r.1 == "completed"),
        "{completed:?}"
    );
    assert!(
        has(&completed, &done) && !has(&completed, &running),
        "{completed:?}"
    );

    let active = entries(&list(json!({"status_filter":"running"})).await.ok());
    assert!(active.iter().all(|r| r.1 == "running"), "{active:?}");
    assert!(has(&active, &running) && !has(&active, &done), "{active:?}");

    let sleeps = entries(&list(json!({"tool_filter":"Sleep"})).await.ok());
    assert!(sleeps.iter().all(|r| r.2 == "Sleep"), "{sleeps:?}");
    assert!(has(&sleeps, &running) && !has(&sleeps, &done), "{sleeps:?}");

    async_call(&harness, "stop", &running, json!({})).await;
}

#[tokio::test]
async fn unknown_task_ids_are_reported_not_errors() {
    let harness = harness().await;
    let out = async_call(&harness, "output", "missing", json!({})).await;
    assert_eq!(out["error"], "Task not found");
    let stop = async_call(&harness, "stop", "missing", json!({})).await;
    assert_eq!(stop["success"], false);
    assert_eq!(stop["message"], "Task not found");
    let status = async_call(&harness, "status", "missing", json!({})).await;
    assert_eq!(status["error"], "Task not found");
}

#[tokio::test]
async fn spawning_an_unknown_tool_fails_the_task() {
    let harness = harness().await;
    let id = spawn(&harness, json!({"tool":"NoSuchTool", "params":{}})).await;
    let out = async_call(
        &harness,
        "output",
        &id,
        json!({"block":true, "timeout":5000}),
    )
    .await;
    assert_eq!(out["is_terminal"], true, "{out}");
    assert_eq!(out["status"], "failed", "{out}");
}

/// Shell commands that print `started`, then idle long enough to be
/// observed and stopped.
fn long_running_command() -> &'static str {
    if cfg!(windows) {
        "Write-Output started; Start-Sleep -Seconds 30"
    } else {
        "echo started; sleep 30"
    }
}

/// Poll `Async output` until the running task's live output contains
/// `needle`.
async fn wait_for_partial_output(harness: &ToolHarness, id: &str, needle: &str) -> Value {
    for _ in 0..100 {
        let out = async_call(harness, "output", id, json!({})).await;
        if out["partial_output"]
            .as_str()
            .is_some_and(|p| p.contains(needle))
        {
            return out;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no live output containing {needle:?} for {id}");
}

/// Bash `run_in_background` and `Async action=spawn tool=Bash` are one
/// path: each registers exactly one task, in the caller principal's
/// executor, with the same ownership and delivery settings.
#[tokio::test]
async fn bash_background_and_async_bash_register_one_task_on_one_path() {
    let harness = harness().await;
    let direct = harness
        .call(
            "Bash",
            json!({"command":"echo direct", "run_in_background":true}),
        )
        .await
        .ok();
    let spawned = harness
        .call(
            "Async",
            json!({"action":"spawn", "tool":"Bash", "params":{"command":"echo spawned"}}),
        )
        .await
        .ok();
    let ids = [
        direct["task_id"].as_str().unwrap().to_string(),
        spawned["task_id"].as_str().unwrap().to_string(),
    ];

    for (id, text) in ids.iter().zip(["direct", "spawned"]) {
        let out = async_call(&harness, "output", id, json!({"block":true})).await;
        assert_eq!(out["status"], "completed", "{out}");
        assert!(
            out["result"]["stdout"].as_str().unwrap().contains(text),
            "{out}"
        );
        let status = async_call(&harness, "status", id, json!({})).await;
        assert_eq!(status["tool_name"], "Bash");
        assert_eq!(
            status["parent_session_key"],
            crate::tools::builtin::test_harness::SESSION
        );
    }
    // Both live in the principal's own registry.
    let registry = harness
        .tooling
        .task_registry_for(&peko_subject::PrincipalId(
            crate::tools::builtin::test_harness::PRINCIPAL.into(),
        ));
    for id in &ids {
        assert!(
            registry.read().await.get(id).is_some(),
            "{id} not in the principal registry"
        );
    }
}

/// An Async task body runs inline under the task's own timeout. It used
/// to pass through the foreground timeout, which detached the work and
/// completed the task with a "queued" receipt instead of the result.
#[tokio::test]
async fn async_spawn_runs_past_the_foreground_timeout_and_returns_the_result() {
    let harness = ToolHarness::with_router_timeout(1).await;
    harness.register(Arc::new(Sleep)).await;
    let id = spawn(&harness, json!({"tool":"Sleep", "params":{"ms":2500}})).await;
    // Poll without blocking: a foreground call longer than the 1s router
    // timeout would itself detach.
    let mut out = Value::Null;
    for _ in 0..100 {
        out = async_call(&harness, "output", &id, json!({})).await;
        if out["is_terminal"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(out["status"], "completed", "{out}");
    assert_eq!(out["result"], json!({"slept_ms": 2500}), "{out}");
}

/// Both entrances stream live output while running, and stop kills them.
#[tokio::test]
async fn background_bash_streams_live_output_and_stops_from_either_entrance() {
    let harness = harness().await;
    let direct = harness
        .call(
            "Bash",
            json!({"command": long_running_command(), "run_in_background":true}),
        )
        .await
        .ok();
    let spawned = harness
        .call(
            "Async",
            json!({"action":"spawn", "tool":"Bash", "params":{"command": long_running_command()}}),
        )
        .await
        .ok();
    for receipt in [direct, spawned] {
        let id = receipt["task_id"].as_str().unwrap().to_string();
        let running = wait_for_partial_output(&harness, &id, "started").await;
        assert_eq!(running["is_terminal"], false, "{running}");
        let stopped = async_call(&harness, "stop", &id, json!({})).await;
        assert_eq!(stopped["success"], true, "{stopped}");
        let status = async_call(&harness, "status", &id, json!({})).await;
        assert_eq!(status["status"], "cancelled", "{status}");
    }
}

/// Bash's `timeout` (milliseconds) bounds a background task.
#[tokio::test]
async fn background_bash_timeout_bounds_the_task() {
    let harness = harness().await;
    let receipt = harness
        .call(
            "Bash",
            json!({"command": long_running_command(), "run_in_background":true, "timeout":500}),
        )
        .await
        .ok();
    let id = receipt["task_id"].as_str().unwrap();
    let out = async_call(
        &harness,
        "output",
        id,
        json!({"block":true, "timeout":10000}),
    )
    .await;
    assert_eq!(out["status"], "timed_out", "{out}");
}

/// Background work belongs to the principal whose call created it: the
/// harness's Async runtime (bound to the default caller) sees its own
/// background Bash, never one started by another principal.
#[tokio::test]
async fn background_bash_is_owned_by_the_calling_principal() {
    use crate::tools::builtin::test_harness::Caller;
    let harness = harness().await;
    harness.bind_principal("did:peko:other").await;
    let mine = harness
        .call(
            "Bash",
            json!({"command":"echo owned", "run_in_background":true}),
        )
        .await
        .ok();
    let theirs = harness
        .call_as(
            &Caller::principal("did:peko:other"),
            "Bash",
            json!({"command":"echo foreign", "run_in_background":true}),
        )
        .await
        .ok();
    let id = |receipt: &Value| receipt["task_id"].as_str().expect("receipt").to_string();

    let status = async_call(&harness, "status", &id(&mine), json!({})).await;
    assert_eq!(status["tool_name"], "Bash", "owner sees its task: {status}");
    let foreign = async_call(&harness, "status", &id(&theirs), json!({})).await;
    assert_eq!(foreign["error"], "Task not found", "{foreign}");
    let stop = async_call(&harness, "stop", &id(&theirs), json!({})).await;
    assert_eq!(
        stop["success"], false,
        "cannot cancel another principal's task"
    );
}

/// A dispatch that carries no principal cannot start background work, and
/// principal-scoped tools refuse it. (The dispatcher used to hand tools
/// `Some("")`, which passed their principal checks.)
#[tokio::test]
async fn principal_less_dispatch_cannot_start_background_work() {
    let harness = harness().await;
    let dispatch = |tool: &str, params: Value| {
        let call = peko_engine::ToolCallSpec::new(tool, params);
        harness.tooling.dispatcher().execute(call)
    };

    let (display, _, ok) = dispatch(
        "Bash",
        json!({"command":"echo nobody", "run_in_background":true}),
    )
    .await
    .unwrap();
    assert!(!ok, "no principal, no background task");
    assert!(display.contains("Async runtime"), "{display}");

    let (display, _, ok) = dispatch(
        "Cron",
        json!({"action":"create", "message":"m", "delay":"5m"}),
    )
    .await
    .unwrap();
    assert!(!ok, "Cron must refuse a principal-less call");
    assert!(display.contains("Principal context"), "{display}");
    assert!(harness.cron.jobs().is_empty());
}

/// `run_in_background` inside a background task body is moot: the body
/// runs inline and the one task carries the result — no second task.
#[tokio::test]
async fn async_spawn_of_background_bash_does_not_spawn_a_second_task() {
    let harness = harness().await;
    let before = harness.call("Async", json!({"action":"list"})).await.ok()["total"]
        .as_u64()
        .unwrap();
    let id = spawn(
        &harness,
        json!({"tool":"Bash", "params":{"command":"echo nested", "run_in_background":true}}),
    )
    .await;
    let out = async_call(&harness, "output", &id, json!({"block":true})).await;
    assert_eq!(out["status"], "completed", "{out}");
    assert!(
        out["result"]["stdout"].as_str().unwrap().contains("nested"),
        "the task holds the command's output, not another receipt: {out}"
    );
    let after = harness.call("Async", json!({"action":"list"})).await.ok()["total"]
        .as_u64()
        .unwrap();
    assert_eq!(after, before + 1, "exactly one task");
}

/// Foreground calls are not background tasks: their routing entries stay
/// out of `Async list` unless the call detaches on the foreground timeout,
/// which turns it into a visible, stoppable task.
#[tokio::test]
async fn only_detached_foreground_calls_appear_as_tasks() {
    let harness = ToolHarness::with_router_timeout(1).await;
    harness.register(Arc::new(Emit)).await;
    harness.register(Arc::new(Sleep)).await;

    harness.call("Emit", json!({"lines":1})).await.ok();
    let listed = harness.call("Async", json!({"action":"list"})).await.ok();
    assert_eq!(listed["total"], 0, "inline calls are not tasks: {listed}");

    let receipt = harness.call("Sleep", json!({"ms":30000})).await.ok();
    assert_eq!(receipt["_async_status"], "queued", "{receipt}");
    let id = receipt["task_id"].as_str().unwrap();
    let status = async_call(&harness, "status", id, json!({})).await;
    assert_eq!(
        status["status"], "running",
        "detached call is a task: {status}"
    );
    let stopped = async_call(&harness, "stop", id, json!({})).await;
    assert_eq!(stopped["success"], true, "{stopped}");
}

/// A foreground call that detaches on timeout becomes a task in the
/// calling principal's own registry — not in a process-wide one.
#[tokio::test]
async fn detached_foreground_calls_register_in_the_principals_registry() {
    use crate::tools::builtin::test_harness::PRINCIPAL;
    let harness = ToolHarness::with_router_timeout(1).await;
    harness.register(Arc::new(Sleep)).await;

    let receipt = harness.call("Sleep", json!({"ms":30000})).await.ok();
    let id = receipt["task_id"].as_str().unwrap().to_string();
    let registry = harness
        .tooling
        .task_registry_for(&peko_subject::PrincipalId(PRINCIPAL.into()));
    let entry = registry
        .read()
        .await
        .get(&id)
        .cloned()
        .expect("detached call registered in the principal's registry");
    assert_eq!(entry.config.principal_id.0, PRINCIPAL);
    async_call(&harness, "stop", &id, json!({})).await;
}

/// `list` counts the tasks still running; a blocking `output` without a
/// timeout waits on its default (minutes), not a second; a finished task's
/// status reports how long it ran.
#[tokio::test]
async fn list_counts_active_tasks_and_output_blocks_on_its_default_timeout() {
    let harness = harness().await;
    let quick = spawn(&harness, json!({"tool":"Emit", "params":{"lines":1}})).await;
    let slow = spawn(&harness, json!({"tool":"Sleep", "params":{"ms":1500}})).await;
    async_call(&harness, "output", &quick, json!({"block": true})).await;

    let listed = harness.call("Async", json!({"action":"list"})).await.ok();
    assert_eq!(listed["active"], 1, "{listed}");

    let out = async_call(&harness, "output", &slow, json!({"block": true})).await;
    assert_eq!(out["is_terminal"], true, "{out}");
    assert_eq!(out["result"]["slept_ms"], 1500, "{out}");
    assert!(out["elapsed_seconds"].as_i64().unwrap() >= 1, "{out}");
    let status = async_call(&harness, "status", &slow, json!({})).await;
    assert!(
        status["duration_seconds"].as_i64().unwrap() >= 1,
        "{status}"
    );
}
