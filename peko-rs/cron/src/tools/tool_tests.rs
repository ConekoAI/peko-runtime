//! Behavior tests for the `Cron` domain tool, driven through
//! [`CronTool::with_runtime`] against the file-backed test runtime.

use super::CronTool;
use crate::testing::FileCronRuntime;
use crate::{CronJobAction, ScheduleKind};
use peko_subject::PrincipalId;
use peko_tools_core::{Tool, ToolContext};
use serde_json::{json, Value};
use std::sync::Arc;

const ALICE: &str = "did:peko:alice";
const BOB: &str = "did:peko:bob";

struct Fixture {
    _dir: tempfile::TempDir,
    runtime: Arc<FileCronRuntime>,
    tool: CronTool,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Arc::new(FileCronRuntime::new(dir.path()));
        let tool = CronTool::with_runtime(runtime.clone());
        Self {
            _dir: dir,
            runtime,
            tool,
        }
    }

    async fn call(&self, principal: &str, params: Value) -> anyhow::Result<Value> {
        let ctx = ToolContext::for_hook_run("run", "call", "Cron")
            .with_principal_id(principal)
            .with_session_id("session-1");
        self.tool.execute_with_context(params, &ctx).await
    }

    async fn ok(&self, principal: &str, params: Value) -> Value {
        self.call(principal, params.clone())
            .await
            .unwrap_or_else(|e| panic!("{params} failed: {e}"))
    }

    async fn err(&self, principal: &str, params: Value) -> String {
        match self.call(principal, params.clone()).await {
            Ok(value) => panic!("{params} unexpectedly succeeded: {value}"),
            Err(e) => e.to_string(),
        }
    }

    async fn create(&self, principal: &str, params: Value) -> String {
        let mut params = params;
        params["action"] = json!("create");
        let created = self.ok(principal, params).await;
        created["job_id"].as_str().unwrap().to_string()
    }

    fn job(&self, principal: &str, id: &str) -> crate::CronJob {
        self.runtime
            .scheduler(&PrincipalId(principal.into()))
            .get_job(id)
            .unwrap()
            .unwrap_or_else(|| panic!("job {id} missing for {principal}"))
    }
}

const ACTIONS: [&str; 6] = ["create", "list", "delete", "update", "trigger", "history"];

#[tokio::test]
async fn unbound_tool_reports_missing_runtime() {
    let ctx = ToolContext::for_hook_run("run", "call", "Cron").with_principal_id(ALICE);
    for action in ACTIONS {
        let error = CronTool::new()
            .execute_with_context(json!({"action": action}), &ctx)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("not initialized"),
            "{action}: {error}"
        );
    }
}

#[tokio::test]
async fn every_action_refuses_a_contextless_call() {
    let fx = Fixture::new();
    for action in ACTIONS {
        let error = fx
            .tool
            .execute(json!({"action": action}))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("use execute_with_context"),
            "{action}: {error}"
        );
    }
}

#[tokio::test]
async fn every_action_rejects_mistyped_arguments() {
    let fx = Fixture::new();
    for (action, bad) in [
        ("create", json!({"message": 7})),
        ("delete", json!({"id": 7})),
        ("update", json!({"enabled": "yes"})),
        ("trigger", json!({"label": 7})),
        ("history", json!({"limit": "ten"})),
    ] {
        let mut params = bad;
        params["action"] = json!(action);
        let error = fx.err(ALICE, params).await;
        assert!(error.contains("arguments"), "{action}: {error}");
    }
}

#[tokio::test]
async fn targeted_actions_require_an_id_or_label() {
    let fx = Fixture::new();
    for action in ["delete", "update", "trigger", "history"] {
        // `enabled` gives update something to change; the others ignore it.
        for params in [
            json!({"action": action, "enabled": true}),
            json!({"action": action, "enabled": true, "id": ""}),
        ] {
            let error = fx.err(ALICE, params).await;
            assert!(error.contains("id or label"), "{action}: {error}");
        }
    }
}

#[tokio::test]
async fn every_action_requires_a_principal_context() {
    let fx = Fixture::new();
    for action in ACTIONS {
        let error = fx
            .tool
            .execute_with_context(
                json!({"action": action, "id": "x", "message": "m", "delay": "5m"}),
                &ToolContext::for_hook_run("run", "call", "Cron"),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("Principal context"),
            "{action}: {error}"
        );
    }
}

#[tokio::test]
async fn create_message_job_records_owner_session_and_send_action() {
    let fx = Fixture::new();
    let id = fx
        .create(
            ALICE,
            json!({"message":"stand-up", "interval_ms":60000, "label":"standup"}),
        )
        .await;
    let job = fx.job(ALICE, &id);
    assert_eq!(job.principal_id.0, ALICE);
    assert_eq!(job.name, "standup");
    assert_eq!(job.origin_session.as_deref(), Some("session-1"));
    assert!(!job.delete_after_run);
    assert!(matches!(
        job.schedule,
        ScheduleKind::Every { every_ms: 60000 }
    ));
    assert!(matches!(
        job.action,
        CronJobAction::Send { ref message, target: None } if message == "stand-up"
    ));
}

#[tokio::test]
async fn create_tool_job_defaults_params_and_keeps_run_options() {
    let fx = Fixture::new();
    let id = fx
        .create(
            ALICE,
            json!({"tool":"Bash", "cron":"0 9 * * *", "timezone":"Europe/Paris",
                   "wake_on_completion":true, "timeout_secs":30}),
        )
        .await;
    let job = fx.job(ALICE, &id);
    assert!(
        job.name.starts_with("cron-"),
        "generated label: {}",
        job.name
    );
    assert!(matches!(
        job.schedule,
        ScheduleKind::Cron { ref expr, tz: Some(ref tz) } if expr == "0 9 * * *" && tz == "Europe/Paris"
    ));
    match job.action {
        CronJobAction::SpawnTool {
            tool_name,
            tool_params,
            wake_on_completion,
            timeout_secs,
        } => {
            assert_eq!(tool_name, "Bash");
            assert_eq!(tool_params, json!({}));
            assert_eq!(wake_on_completion, Some(true));
            assert_eq!(timeout_secs, Some(30));
        }
        other => panic!("expected SpawnTool, got {other:?}"),
    }
}

#[tokio::test]
async fn create_rejects_ambiguous_or_empty_job_shapes() {
    let fx = Fixture::new();
    for (params, needle) in [
        (
            json!({"tool":"Bash", "message":"m", "delay":"5m"}),
            "mutually exclusive",
        ),
        (json!({"delay":"5m"}), "requires either"),
        (json!({"message":"  ", "delay":"5m"}), "non-empty"),
        (json!({"tool":" ", "delay":"5m"}), "non-empty"),
        (json!({"message":"m"}), "No schedule"),
        (
            json!({"message":"m", "delay":"5m", "cron":"* * * * *"}),
            "cannot be combined",
        ),
        (json!({"message":"m", "delay":"0s"}), "positive"),
        (json!({"message":"m", "cron":"not a cron"}), "Invalid cron"),
        (json!({"message":"m", "at":"tomorrow"}), "RFC3339"),
        (
            json!({"message":"m", "interval_ms":1000}),
            "every_ms >= 60000",
        ),
    ] {
        let mut params = params;
        params["action"] = json!("create");
        let error = fx.err(ALICE, params.clone()).await;
        assert!(error.contains(needle), "{params}: {error}");
    }
    assert!(
        fx.runtime.jobs().is_empty(),
        "rejections must not persist jobs"
    );
}

#[tokio::test]
async fn create_resolves_schedule_precedence_and_one_shot_rules() {
    let fx = Fixture::new();
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let cases = [
        (
            json!({"at": at, "interval_ms": 60000, "cron": "* * * * *"}),
            "at",
            true,
        ),
        (
            json!({"interval_ms": 60000, "cron": "* * * * *", "idle_ms": 60000}),
            "every",
            false,
        ),
        (
            json!({"cron": "* * * * *", "idle_ms": 60000}),
            "cron",
            false,
        ),
        (json!({"idle_ms": 150000}), "idle:2", false),
        (json!({"idle_ms": 10}), "idle:1", false),
        (json!({"delay": "90s"}), "at", true),
        (
            json!({"interval_ms": 60000, "one_shot": true}),
            "every",
            true,
        ),
    ];
    for (schedule, expected, one_shot) in cases {
        let mut params = schedule.clone();
        params["message"] = json!("tick");
        let id = fx.create(ALICE, params).await;
        let job = fx.job(ALICE, &id);
        let kind = match job.schedule {
            ScheduleKind::At { .. } => "at".to_string(),
            ScheduleKind::Every { .. } => "every".to_string(),
            ScheduleKind::Cron { .. } => "cron".to_string(),
            ScheduleKind::Idle { minutes } => format!("idle:{minutes}"),
        };
        assert_eq!(kind, expected, "{schedule}");
        assert_eq!(job.delete_after_run, one_shot, "{schedule}");
    }
}

#[tokio::test]
async fn list_returns_only_the_callers_jobs() {
    let fx = Fixture::new();
    let mine = fx.create(ALICE, json!({"message":"a", "delay":"5m"})).await;
    fx.create(BOB, json!({"message":"b", "delay":"5m"})).await;
    let listed = fx.ok(ALICE, json!({"action":"list"})).await;
    assert_eq!(listed["count"], 1, "{listed}");
    assert_eq!(listed["jobs"][0]["job_id"], mine.as_str());
}

#[tokio::test]
async fn delete_by_id_or_label_removes_only_the_callers_job() {
    let fx = Fixture::new();
    let by_id = fx.create(ALICE, json!({"message":"a", "delay":"5m"})).await;
    let by_label = fx
        .create(
            ALICE,
            json!({"message":"b", "delay":"5m", "label":"nightly"}),
        )
        .await;
    let deleted = fx.ok(ALICE, json!({"action":"delete", "id": by_id})).await;
    assert_eq!(deleted["cancelled"], true);
    let deleted = fx
        .ok(ALICE, json!({"action":"delete", "label":"nightly"}))
        .await;
    assert_eq!(deleted["job_id"], by_label.as_str());
    assert!(fx.runtime.jobs().is_empty());
}

#[tokio::test]
async fn update_toggles_enabled_resets_failures_and_sets_wake() {
    let fx = Fixture::new();
    let id = fx
        .create(
            ALICE,
            json!({"tool":"Bash", "interval_ms":60000, "label":"poll"}),
        )
        .await;
    let scheduler = fx.runtime.scheduler(&PrincipalId(ALICE.into()));
    scheduler
        .update_job_after_run(&id, "failed", chrono::Utc::now())
        .unwrap();
    assert_eq!(fx.job(ALICE, &id).consecutive_failures, 1);

    fx.ok(
        ALICE,
        json!({"action":"update", "label":"poll", "enabled":false}),
    )
    .await;
    assert!(!fx.job(ALICE, &id).enabled);
    assert_eq!(
        fx.job(ALICE, &id).consecutive_failures,
        1,
        "disable keeps the count"
    );

    fx.ok(
        ALICE,
        json!({"action":"update", "id": id, "enabled":true, "wake_on_completion":true}),
    )
    .await;
    let job = fx.job(ALICE, &id);
    assert!(job.enabled);
    assert_eq!(
        job.consecutive_failures, 0,
        "re-enable resets the retry budget"
    );
    assert!(matches!(
        job.action,
        CronJobAction::SpawnTool {
            wake_on_completion: Some(true),
            ..
        }
    ));

    let error = fx.err(ALICE, json!({"action":"update", "id": id})).await;
    assert!(
        error.contains("enabled") || error.contains("wake"),
        "{error}"
    );
}

#[tokio::test]
async fn trigger_fires_disabled_jobs_and_coalesces_in_flight_runs() {
    let fx = Fixture::new();
    let id = fx
        .create(ALICE, json!({"tool":"Bash", "interval_ms":60000}))
        .await;
    fx.ok(ALICE, json!({"action":"update", "id": id, "enabled":false}))
        .await;
    let first = fx.ok(ALICE, json!({"action":"trigger", "id": id})).await;
    assert_eq!(first["triggered"], true);
    let second = fx.ok(ALICE, json!({"action":"trigger", "id": id})).await;
    assert_eq!(first["run_id"], second["run_id"], "in-flight run coalesces");
}

#[tokio::test]
async fn history_is_newest_first_with_default_and_capped_limits() {
    let fx = Fixture::new();
    let id = fx
        .create(ALICE, json!({"tool":"Bash", "interval_ms":60000}))
        .await;
    let scheduler = fx.runtime.scheduler(&PrincipalId(ALICE.into()));
    let start = chrono::Utc::now() - chrono::Duration::hours(2);
    for minute in 0..60 {
        scheduler
            .record_run(&crate::CronRun {
                id: format!("run_{minute:02}"),
                job_id: id.clone(),
                started_at: start + chrono::Duration::minutes(minute),
                finished_at: Some(start + chrono::Duration::minutes(minute)),
                status: "success".into(),
                output: None,
                error: None,
            })
            .unwrap();
    }
    let history = fx.ok(ALICE, json!({"action":"history", "id": id})).await;
    assert_eq!(history["count"], 10, "default limit");
    assert_eq!(history["runs"][0]["id"], "run_59", "newest first");
    for (limit, expected) in [(0, 1), (3, 3), (500, 50)] {
        let history = fx
            .ok(ALICE, json!({"action":"history", "id": id, "limit": limit}))
            .await;
        assert_eq!(history["count"], expected, "limit {limit}");
    }
}

/// Regression: one-shot jobs reap themselves after firing, but their run
/// history is retained so "what did my reminder do?" stays answerable. The
/// per-principal ownership check used to consult only live jobs, making
/// that history unreachable through the tool.
#[tokio::test]
async fn history_of_a_fired_one_shot_job_stays_readable_by_its_owner_only() {
    let fx = Fixture::new();
    let id = fx
        .create(
            ALICE,
            json!({"message":"remind me", "delay":"1m", "label":"reminder"}),
        )
        .await;
    let run_id = fx.runtime.fire_and_reap(&id, "success").unwrap();
    assert!(fx.runtime.jobs().is_empty(), "one-shot job reaped");

    let history = fx.ok(ALICE, json!({"action":"history", "id": id})).await;
    assert_eq!(history["count"], 1, "{history}");
    assert_eq!(history["runs"][0]["id"], run_id.as_str());

    let error = fx.err(BOB, json!({"action":"history", "id": id})).await;
    assert!(error.contains("not found"), "{error}");
    let error = fx
        .err(ALICE, json!({"action":"history", "label":"reminder"}))
        .await;
    assert!(
        error.contains("not found"),
        "labels resolve live jobs only: {error}"
    );
}

#[tokio::test]
async fn other_principals_cannot_reach_a_job_by_id_or_label() {
    let fx = Fixture::new();
    let id = fx
        .create(
            ALICE,
            json!({"tool":"Bash", "interval_ms":60000, "label":"private"}),
        )
        .await;
    for selector in [json!({"id": id}), json!({"label": "private"})] {
        for (action, extra) in [
            ("delete", json!({})),
            ("update", json!({"enabled": false})),
            ("trigger", json!({})),
            ("history", json!({})),
        ] {
            let mut params = selector.clone();
            params["action"] = json!(action);
            for (key, value) in extra.as_object().unwrap() {
                params[key] = value.clone();
            }
            let error = fx.err(BOB, params.clone()).await;
            assert!(error.contains("not found"), "{params}: {error}");
        }
    }
    let job = fx.job(ALICE, &id);
    assert!(job.enabled, "refused update must not apply");
    let runs = fx
        .runtime
        .scheduler(&PrincipalId(ALICE.into()))
        .get_run_history(&id, 10)
        .unwrap();
    assert!(runs.is_empty(), "refused trigger must not fire");
}

#[tokio::test]
async fn unknown_action_is_rejected() {
    let fx = Fixture::new();
    let error = fx.err(ALICE, json!({"action":"purge"})).await;
    assert!(error.contains("supported action"), "{error}");
}
