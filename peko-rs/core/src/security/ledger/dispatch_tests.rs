//! ADR-079 regression tests through the real dispatcher (the tool
//! harness): what every call writes to the ledger, and when.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peko_subject::PrincipalId;
use peko_tools_core::{ActingFor, Compartment, Origin, Tool, Trust};
use serde_json::{json, Value};

use crate::security::ledger::read::{self, Call};
use crate::security::ledger::redact::SecretSource;
use crate::tools::builtin::test_harness::{Caller, ToolHarness, PRINCIPAL};

fn guest(trust: Trust) -> ActingFor {
    ActingFor {
        actor: "user:hub:x".into(),
        name: None,
        trust,
        compartment: Compartment::Conversation {
            channel: "dm_x".into(),
        },
        origin: Origin::Ingress { channel: None },
    }
}

/// Signals when it starts, then waits to be released.
struct Gate {
    started: tokio::sync::mpsc::Sender<()>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Tool for Gate {
    fn name(&self) -> &'static str {
        "Gate"
    }
    fn description(&self) -> String {
        "test gate".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn execute(&self, _: Value) -> anyhow::Result<Value> {
        self.started.send(()).await.ok();
        self.release.notified().await;
        Ok(json!("released"))
    }
}

/// Sleeps for `ms`, then reports it.
struct Nap;

#[async_trait]
impl Tool for Nap {
    fn name(&self) -> &'static str {
        "Nap"
    }
    fn description(&self) -> String {
        "test nap".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"ms": {"type": "integer"}}})
    }
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        tokio::time::sleep(Duration::from_millis(params["ms"].as_u64().unwrap_or(0))).await;
        Ok(json!("rested"))
    }
}

fn only(calls: &[Call], tool: &str) -> Call {
    let matching: Vec<_> = calls.iter().filter(|c| c.tool() == Some(tool)).collect();
    assert_eq!(matching.len(), 1, "one {tool} call: {calls:#?}");
    matching[0].clone()
}

async fn wait_for(h: &ToolHarness, done: impl Fn(&[Call]) -> bool) -> Vec<Call> {
    for _ in 0..100 {
        let calls = h.ledger_calls();
        if done(&calls) {
            return calls;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "ledger never reached the expected state: {:#?}",
        h.ledger_calls()
    );
}

#[tokio::test]
async fn the_start_is_durable_before_the_call_runs_and_the_end_follows() {
    let h = ToolHarness::new().await;
    let (started, mut started_rx) = tokio::sync::mpsc::channel(1);
    let release = Arc::new(tokio::sync::Notify::new());
    h.register(Arc::new(Gate {
        started,
        release: Arc::clone(&release),
    }))
    .await;

    let tooling = Arc::clone(&h.tooling);
    let running = tokio::spawn(async move {
        let mut call = peko_engine::ToolCallSpec::new("Gate", json!({}));
        call.principal_id = Some(PRINCIPAL.into());
        call.session_id = Some("session".into());
        call.acting_for = Some(ActingFor::runtime());
        call.tool_call_id = Some("toolu_1".into());
        tooling.dispatcher().execute(call).await
    });
    started_rx.recv().await.expect("gate started");
    let in_flight = only(&h.ledger_calls(), "Gate");
    assert!(in_flight.start.is_some() && in_flight.ends.is_empty());
    assert_eq!(in_flight.outcome(), None);

    release.notify_one();
    running.await.unwrap().unwrap();
    let call = only(&h.ledger_calls(), "Gate");
    assert_eq!(call.outcome(), Some("ok"));
    let start = call.start.as_ref().unwrap();
    assert!(call.call_id.starts_with("call_"));
    assert_eq!(start["tool_call_id"], "toolu_1");
    assert_eq!(start["session_id"], "session");
    assert_eq!(start["source"], "builtin");
    assert_eq!(call.ends[0]["result"]["excerpt"], "released");

    // The global log names the same call, with the same digest.
    let global = h
        .audit_events()
        .await
        .into_iter()
        .find(|e| e.event_type == "tool.call" && e.details["tool_name"] == "Gate")
        .expect("tool.call");
    assert_eq!(global.details["call_id"], call.call_id);
    assert_eq!(global.details["params_digest"], start["params_digest"]);
    assert_eq!(global.details["outcome"], "ok");
}

#[tokio::test]
async fn refusals_unknown_tools_and_bad_arguments_are_recorded() {
    let h = ToolHarness::new().await;
    h.call_as(
        &Caller::acting_for(guest(Trust::Untrusted)),
        "Bash",
        json!({"command": "echo hi"}),
    )
    .await
    .err("not available");
    h.call("Nope", json!({})).await.err("not available");
    h.call("Read", json!({"nope": 1})).await.err("");

    let calls = h.ledger_calls();
    let bash = only(&calls, "Bash");
    assert_eq!(bash.outcome(), Some("floor_denied"));
    assert_eq!(bash.stamp().unwrap().actor, "user:hub:x");
    let nope = only(&calls, "Nope");
    assert_eq!(nope.outcome(), Some("error"));
    assert_eq!(nope.start.as_ref().unwrap()["source"], "unavailable");
    assert_eq!(only(&calls, "Read").outcome(), Some("error"));
}

#[tokio::test]
async fn a_failing_command_is_an_error_with_its_exit_code() {
    let h = ToolHarness::new().await;
    h.call("Bash", json!({"command": "echo out; echo bad >&2; exit 3"}))
        .await;
    let bash = only(&h.ledger_calls(), "Bash");
    assert_eq!(bash.outcome(), Some("error"));
    let end = &bash.ends[0];
    assert_eq!(end["exit_code"], 3);
    let excerpt = end["result"]["excerpt"].as_str().unwrap();
    assert!(
        excerpt.contains("out") && excerpt.contains("[stderr]\nbad"),
        "{excerpt}"
    );
}

#[tokio::test]
async fn a_detached_call_ends_twice_under_one_call_id() {
    let h = ToolHarness::with_router_timeout(1).await;
    h.register(Arc::new(Nap)).await;
    let receipt = h.call("Nap", json!({"ms": 2500})).await.ok();
    assert_eq!(receipt["reason"], "timeout", "{receipt}");

    let calls = wait_for(&h, |calls| {
        calls
            .iter()
            .any(|c| c.tool() == Some("Nap") && c.ends.len() == 2)
    })
    .await;
    let nap = only(&calls, "Nap");
    assert_eq!(nap.ends[0]["outcome"], "detached");
    assert_eq!(nap.ends[0]["task_id"], receipt["task_id"]);
    assert_eq!(nap.ends[1]["outcome"], "ok");
    assert_eq!(nap.ends[1]["late"], true);
    assert_eq!(nap.ends[1]["result"]["excerpt"], "rested");
    assert_eq!(nap.outcome(), Some("ok"));
}

#[tokio::test]
async fn a_background_body_names_its_task_and_spawning_call() {
    let h = ToolHarness::new().await;
    h.write("notes/a.txt", "alpha");
    let receipt = h
        .call(
            "Async",
            json!({"action": "spawn", "tool": "Read", "params": {"file_path": "notes/a.txt"}}),
        )
        .await
        .ok();
    let calls = wait_for(&h, |calls| {
        calls
            .iter()
            .any(|c| c.tool() == Some("Read") && !c.ends.is_empty())
    })
    .await;
    let spawn = only(&calls, "Async");
    let body = only(&calls, "Read");
    let parent = &body.start.as_ref().unwrap()["parent"];
    assert_eq!(parent["kind"], "task");
    assert_eq!(parent["id"], receipt["task_id"]);
    assert_eq!(parent["call_id"], spawn.call_id);
    assert_eq!(body.parent_call_id(), Some(spawn.call_id.as_str()));
}

struct Known(&'static str);

impl SecretSource for Known {
    fn secrets(&self) -> Vec<(String, String)> {
        vec![("vault:provider:x/default".into(), self.0.into())]
    }
}

#[tokio::test]
async fn known_secrets_never_reach_the_ledger() {
    let h = ToolHarness::new().await;
    let secret = "zz-provider-key-0123456789";
    h.tooling
        .dispatcher()
        .set_secret_source(Arc::new(Known(secret)));
    h.call("Bash", json!({"command": format!("echo {secret}")}))
        .await
        .ok();
    let text = std::fs::read_dir(h.ledger_dir())
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(!text.contains(secret), "the secret leaked: {text}");
    let bash = only(&h.ledger_calls(), "Bash");
    assert_eq!(
        bash.start.as_ref().unwrap()["params"]["command"],
        "echo [secret:vault:provider:x/default]"
    );
    assert!(bash.ends[0]["result"]["excerpt"]
        .as_str()
        .unwrap()
        .contains("[secret:vault:provider:x/default]"));
}

#[tokio::test]
async fn an_unopenable_ledger_refuses_everyone_but_the_owner() {
    let h = ToolHarness::new().await;
    let blocker = h.write("not-a-dir", "x");
    let other = PrincipalId("other".into());
    assert!(h
        .tooling
        .dispatcher()
        .bind_ledger(&other, &blocker.join("ledger"))
        .is_err());
    h.write("notes/a.txt", "alpha");

    let trusted = Caller {
        principal: "other".into(),
        ..Caller::acting_for(guest(Trust::Trusted))
    };
    h.call_as(&trusted, "Read", json!({"file_path": "notes/a.txt"}))
        .await
        .err("ledger is unavailable");
    let owner = Caller::principal("other");
    h.call_as(&owner, "Read", json!({"file_path": "notes/a.txt"}))
        .await
        .ok();
    let failures = h
        .audit_events()
        .await
        .into_iter()
        .filter(|e| e.event_type == crate::security::ledger::EVENT_WRITE_FAILED)
        .count();
    assert_eq!(failures, 2);
}

#[tokio::test]
async fn a_harness_ledger_verifies_against_the_global_log() {
    let h = ToolHarness::new().await;
    h.call("Bash", json!({"command": "echo one"})).await.ok();
    h.call("Glob", json!({"pattern": "*"})).await;
    h.tooling.dispatcher().checkpoint_ledgers(true).await;
    let audit_dir = h.ledger_dir().parent().unwrap().join("audit");
    let report = read::verify(h.ledger_dir(), &audit_dir, PRINCIPAL).unwrap();
    assert!(report.is_intact(), "{report:#?}");
    assert_eq!(report.chain.records, 5, "genesis + two calls");
}
