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

/// Background work is owned by the principal whose call created it: the
/// harness's Async runtime (bound to the default caller) sees its own
/// background Bash, and never one started by another principal.
#[tokio::test]
async fn background_bash_is_owned_by_the_calling_principal() {
    use crate::tools::builtin::test_harness::Caller;
    let harness = harness().await;
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

    // A call with no principal at all is system-owned — visible to no
    // principal (it used to be visible to every principal).
    let unowned = crate::tools::builtin::BashTool::new()
        .execute(json!({"command":"echo unowned", "run_in_background":true}))
        .await
        .unwrap();
    let hidden = async_call(&harness, "status", &id(&unowned), json!({})).await;
    assert_eq!(hidden["error"], "Task not found", "{hidden}");
}

/// A dispatch that carries no principal must not invent one: background
/// work it starts is system-owned, and principal-scoped tools refuse. (The
/// dispatcher used to hand tools `Some("")`, which passed their
/// principal checks and owned background tasks as principal "".)
#[tokio::test]
async fn principal_less_dispatch_is_system_owned_and_refused_by_scoped_tools() {
    use crate::async_exec::executor::registry::list_all_tasks_across_all_registries;
    let harness = harness().await;
    let dispatch = |tool: &str, params: Value| {
        let call = peko_engine::ToolCallSpec::new(tool, params);
        harness.tooling.dispatcher().execute(call)
    };

    let (_, receipt, ok) = dispatch(
        "Bash",
        json!({"command":"echo nobody", "run_in_background":true}),
    )
    .await
    .unwrap();
    assert!(ok, "{receipt}");
    let id = receipt["task_id"].as_str().unwrap();
    let entry = list_all_tasks_across_all_registries()
        .await
        .into_iter()
        .find(|e| e.task_id == id)
        .expect("background task registered");
    assert_eq!(
        entry.config.principal_id,
        *peko_subject::PrincipalId::system()
    );

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
