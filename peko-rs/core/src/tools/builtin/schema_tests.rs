//! Check the advertised contracts against actual built-in implementations.

use super::*;
use peko_tools_core::Tool;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

fn tools() -> Vec<Arc<dyn Tool>> {
    let asynchronous = Arc::new(crate::async_exec::executor::TestAsyncRuntime::new());
    let todos = Arc::new(tasks::TestTodoRuntime::new());
    let plans = Arc::new(plan::TestPlanPort::new());
    let channels = Arc::new(peko_channel::NoopChannelPort);
    vec![
        Arc::new(BashTool::new()),
        Arc::new(ReadTool::new()),
        Arc::new(WriteTool::new()),
        Arc::new(EditTool::new()),
        Arc::new(GlobTool::new()),
        Arc::new(GrepTool::new()),
        Arc::new(SessionTool::new(Arc::new(SessionCache::new("test")))),
        Arc::new(AgentTool::new(Arc::new(
            messaging::agent::TestSubagentRuntime::new(),
        ))),
        Arc::new(AgentCatalogTool::new(vec![])),
        Arc::new(SkillTool::new(Arc::new(
            crate::extensions::skill::reader::WorkspaceSkillRuntime::new("skills".into()),
        ))),
        Arc::new(ModelListTool::new(Weak::new())),
        Arc::new(ModelCallTool::new(Weak::new())),
        Arc::new(WorkflowTool::new(
            Weak::new(),
            Arc::new(crate::ipc::run_tokens::RunTokenRegistry::new()),
        )),
        Arc::new(ChannelReadTool::new(channels.clone())),
        Arc::new(ChannelSendTool::new_local_only(
            channels,
            "did:peko:test".into(),
        )),
        Arc::new(AsyncTool::new(asynchronous)),
        Arc::new(TaskTool::new(todos)),
        Arc::new(PlanTool::new(plans)),
        Arc::new(peko_cron::CronTool::new()),
    ]
}

#[test]
fn inventory_matches_implementations_and_schemas_compile() {
    let implementations = tools();
    let actual: HashSet<_> = implementations.iter().map(|tool| tool.name()).collect();
    let declared: HashSet<_> = crate::tools::installation::all_tool_names()
        .into_iter()
        .collect();
    assert_eq!(actual, declared);
    assert_eq!(actual.len(), 19);
    for retired in [
        "TaskCreate",
        "TaskGet",
        "TaskList",
        "TaskUpdate",
        "PlanCreate",
        "PlanList",
        "PlanGet",
        "PlanAddStep",
        "PlanMarkStep",
        "PlanRecordEvidence",
        "PlanClose",
        "CronCreate",
        "CronList",
        "CronDelete",
        "CronUpdate",
        "CronTrigger",
        "CronHistory",
        "AsyncSpawn",
        "AsyncOutput",
        "AsyncStatus",
        "AsyncList",
        "AsyncStop",
    ] {
        assert!(
            !actual.contains(retired),
            "retired tool {retired} is registered"
        );
    }
    assert_eq!(
        declared.len(),
        crate::tools::installation::BUILTIN_INSTALLATIONS.len(),
        "duplicate manifest names"
    );
    assert_eq!(actual.len(), implementations.len(), "duplicate wire names");
    let catalog = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/architecture/builtin-tools.md"
    ));
    for tool in implementations {
        assert!(
            catalog.contains(&format!("### {}\n", tool.name())),
            "{} missing from documentation",
            tool.name()
        );
        jsonschema::validator_for(&tool.parameters())
            .unwrap_or_else(|error| panic!("{}: {error}", tool.name()));
    }
}

#[test]
fn schemas_accept_valid_calls_and_reject_incomplete_or_conflicting_calls() {
    let schemas: HashMap<_, _> = tools()
        .into_iter()
        .map(|tool| (tool.name().to_string(), tool.parameters()))
        .collect();
    let cases: Vec<(&str, Value, bool)> = vec![
        (
            "Agent",
            json!({"prompt":"work", "role":"worker", "path":"child"}),
            true,
        ),
        ("Agent", json!({"role":"worker", "path":"child"}), false),
        (
            "Agent",
            json!({"prompt":"", "role":"worker", "path":"child"}),
            false,
        ),
        (
            "Agent",
            json!({"prompt":"work", "role":"worker", "path":"child", "overwrite":true}),
            false,
        ),
        (
            "Agent",
            json!({"action":"branch", "prompt":"work", "role":"worker", "path":"child", "overwrite":true, "page_limit":10000}),
            true,
        ),
        (
            "Agent",
            json!({"action":"resume", "prompt":"work", "role":"worker", "path":"sess:/child", "page_limit":5}),
            false,
        ),
        (
            "Agent",
            json!({"prompt":"work", "role":"worker", "path":"child", "page_limit":10001}),
            false,
        ),
        ("Session", json!({"action":"list"}), true),
        ("Session", json!({"action":"copy", "path":"sess:/a"}), false),
        (
            "Session",
            json!({"action":"copy", "path":"sess:/a", "target":"sess:/b"}),
            true,
        ),
        ("Session", json!({"action":"move", "path":"sess:/a"}), false),
        (
            "Session",
            json!({"action":"move", "path":"sess:/a", "title":"Renamed"}),
            true,
        ),
        (
            "Session",
            json!({"action":"move", "path":"sess:/a", "page_limit":0}),
            true,
        ),
        ("Session", json!({"action":"remove"}), false),
        ("Session", json!({"action":"find"}), false),
        ("Session", json!({"action":"read_page", "page":1}), true),
        ("Session", json!({"action":"read_page", "page":0}), false),
        (
            "Session",
            json!({"action":"search_pages", "query":"needle"}),
            true,
        ),
        ("ModelCall", json!({"prompt":"classify"}), true),
        ("ModelCall", json!({}), false),
        (
            "ModelCall",
            json!({"state":"text", "questions":{"ok":{"type":"boolean"}}}),
            true,
        ),
        (
            "ModelCall",
            json!({"prompt":"classify", "state":"text"}),
            false,
        ),
        ("ModelCall", json!({"state":"text", "questions":{}}), false),
        (
            "ModelCall",
            json!({"state":"text", "questions":{"ok":{}}, "system":"judge"}),
            false,
        ),
        (
            "Cron",
            json!({"action":"create", "message":"remind me", "delay":"5m"}),
            true,
        ),
        (
            "Cron",
            json!({"action":"create", "tool":"Bash", "interval_ms":60000, "one_shot":true}),
            true,
        ),
        ("Cron", json!({"action":"create", "delay":"5m"}), false),
        (
            "Cron",
            json!({"action":"create", "message":"remind me"}),
            false,
        ),
        (
            "Cron",
            json!({"action":"create", "message":"remind me", "tool":"Bash", "delay":"5m"}),
            false,
        ),
        (
            "Cron",
            json!({"action":"create", "message":"remind me", "delay":"5m", "cron":"* * * * *"}),
            false,
        ),
        (
            "Cron",
            json!({"action":"update", "id":"job", "enabled":false}),
            true,
        ),
        (
            "Cron",
            json!({"action":"update", "id":"job", "label":"legacy", "enabled":false}),
            true,
        ),
        ("Cron", json!({"action":"update", "id":"job"}), false),
        ("Cron", json!({"action":"update", "enabled":false}), false),
        (
            "Async",
            json!({"action":"spawn", "tool":"Read", "params":{}, "timeout_secs":null}),
            true,
        ),
        ("Workflow", json!({"path":"triage.py"}), true),
        (
            "Workflow",
            json!({"path":"triage.py", "_workflow_depth":1}),
            false,
        ),
    ];
    for (name, args, expected) in cases {
        let validator = jsonschema::validator_for(&schemas[name]).unwrap();
        assert_eq!(validator.is_valid(&args), expected, "{name}: {args}");
    }
}

#[test]
fn domain_action_schemas_reject_missing_unknown_and_irrelevant_fields() {
    let tools = tools();
    let cases = [
        (
            "Task",
            json!({"action":"create", "subject":"review"}),
            "taskId",
        ),
        (
            "Task",
            json!({"action":"get", "taskId":"todo:1"}),
            "subject",
        ),
        ("Task", json!({"action":"list"}), "owner"),
        (
            "Task",
            json!({"action":"update", "taskId":"todo:1", "owner":"worker"}),
            "subject",
        ),
        (
            "Plan",
            json!({"action":"create", "title":"work", "nodes":[{"step":"first"}]}),
            "planId",
        ),
        ("Plan", json!({"action":"list"}), "planId"),
        ("Plan", json!({"action":"get", "planId":"p"}), "title"),
        (
            "Plan",
            json!({"action":"add_step", "planId":"p", "step":"next"}),
            "output",
        ),
        (
            "Plan",
            json!({"action":"mark_step", "planId":"p", "nodeId":"n", "status":"completed"}),
            "output",
        ),
        (
            "Plan",
            json!({"action":"record_evidence", "planId":"p", "nodeId":"n", "output":"done"}),
            "status",
        ),
        (
            "Plan",
            json!({"action":"close", "planId":"p", "reason":"done"}),
            "nodeId",
        ),
        (
            "Cron",
            json!({"action":"create", "message":"reminder", "delay":"5m"}),
            "id",
        ),
        ("Cron", json!({"action":"list"}), "id"),
        ("Cron", json!({"action":"delete", "id":"job"}), "enabled"),
        (
            "Cron",
            json!({"action":"update", "id":"job", "enabled":false}),
            "message",
        ),
        ("Cron", json!({"action":"trigger", "id":"job"}), "enabled"),
        ("Cron", json!({"action":"history", "id":"job"}), "enabled"),
        (
            "Async",
            json!({"action":"spawn", "tool":"Read", "params":{}}),
            "task_id",
        ),
        (
            "Async",
            json!({"action":"output", "task_id":"task"}),
            "tool",
        ),
        (
            "Async",
            json!({"action":"status", "task_id":"task"}),
            "block",
        ),
        ("Async", json!({"action":"list"}), "task_id"),
        ("Async", json!({"action":"stop", "task_id":"task"}), "block"),
        ("Session", json!({"action":"status"}), "target"),
        ("Session", json!({"action":"list"}), "include_tools"),
        ("Session", json!({"action":"history"}), "peer"),
        ("Session", json!({"action":"find", "query":"word"}), "page"),
        (
            "Session",
            json!({"action":"copy", "path":"sess:/a", "target":"sess:/b"}),
            "recursive",
        ),
        (
            "Session",
            json!({"action":"move", "path":"sess:/a", "title":"name"}),
            "recursive",
        ),
        (
            "Session",
            json!({"action":"remove", "path":"sess:/a"}),
            "title",
        ),
        ("Session", json!({"action":"list_pages"}), "query"),
        ("Session", json!({"action":"read_page", "page":1}), "query"),
        (
            "Session",
            json!({"action":"search_pages", "query":"word"}),
            "page",
        ),
    ];
    for (name, args, irrelevant) in cases {
        let tool = tools.iter().find(|tool| tool.name() == name).unwrap();
        let validator = jsonschema::validator_for(&tool.parameters()).unwrap();
        assert!(validator.is_valid(&args), "{name}: {args}");
        for field in ["action", irrelevant, "undeclared_field"] {
            let mut invalid = args.clone();
            let schema = tool.parameters();
            let property = &schema["properties"][field];
            invalid[field] = if field == "action" {
                json!("invalid")
            } else if let Some(values) = property["enum"].as_array() {
                values[0].clone()
            } else {
                match property["type"].as_str() {
                    Some("boolean") => json!(true),
                    Some("integer") => json!(1),
                    _ => json!("unused"),
                }
            };
            assert!(!validator.is_valid(&invalid), "{name}: {invalid}");
        }
        let mut missing = args;
        missing.as_object_mut().unwrap().remove("action");
        assert!(!validator.is_valid(&missing), "{name}: missing action");
    }
}

#[tokio::test]
async fn task_domain_dispatch_preserves_session_ownership_and_audits_actions_once() {
    use crate::tools::metadata::ToolSource;
    let directory = tempfile::tempdir().unwrap();
    let audit = Arc::new(
        peko_observability::Observability::with_audit_dir("test", directory.path().to_path_buf())
            .unwrap(),
    );
    let catalog = Arc::new(crate::tools::catalog::ToolCatalog::new());
    catalog
        .register_system(
            Arc::new(TaskTool::new(Arc::new(tasks::TestTodoRuntime::new()))),
            ToolSource::BuiltIn,
        )
        .await;
    let dispatcher = crate::tools::dispatcher::ToolDispatcher::new(
        catalog.clone(),
        Arc::new(crate::extensions::workspace_dispatcher::WorkspaceHookDispatcher::new()),
        Arc::new(
            crate::extensions::framework::transport::async_router::AsyncExecutionRouter::new(),
        ),
        Some(audit.clone()),
    );
    let mut task_id = Value::Null;
    for (session, params) in [
        ("a", json!({"action":"create", "subject":"review"})),
        ("b", json!({"action":"list"})),
        ("a", json!({"action":"list"})),
    ] {
        let mut call = peko_engine::ToolCallSpec::new("Task", params);
        call.session_id = Some(session.into());
        call.principal_id = Some("principal".into());
        let (_, result, success) = dispatcher.execute(call).await.unwrap();
        assert!(success, "{result}");
        if session == "a" && task_id.is_null() {
            task_id = result["taskId"].clone();
            assert!(!task_id.is_null(), "{result}");
        } else if session == "b" {
            assert_eq!(result.as_array().unwrap().len(), 0, "{result}");
        } else {
            assert_eq!(result.as_array().unwrap().len(), 1, "{result}");
        }
    }
    for name in ["TaskCreate", "TaskGet", "TaskList", "TaskUpdate"] {
        assert!(catalog
            .get(name, peko_subject::PrincipalId::system())
            .await
            .is_none());
    }
    let (_, _, success) = dispatcher
        .execute(peko_engine::ToolCallSpec::new(
            "Task",
            json!({"action":"private argument text"}),
        ))
        .await
        .unwrap();
    assert!(!success);
    let events = audit.get_audit_log(10).await;
    assert_eq!(events.len(), 4);
    let invalid = events
        .iter()
        .find(|event| event.details["success"] == false)
        .unwrap();
    assert!(invalid.details["action"].is_null());
    assert!(!invalid
        .details
        .to_string()
        .contains("private argument text"));
    assert!(events
        .iter()
        .all(|event| event.event_type == "tool.call" && event.details["tool_name"] == "Task"));
    assert!(events
        .iter()
        .any(|event| event.details["action"] == "create"));
}

#[tokio::test]
async fn workflow_dispatch_preserves_hidden_depth_for_the_recursion_guard() {
    use crate::extensions::framework::transport::async_router::AsyncExecutionRouter;
    use crate::extensions::workspace_dispatcher::WorkspaceHookDispatcher;
    use crate::tools::catalog::ToolCatalog;
    use crate::tools::dispatcher::ToolDispatcher;
    use crate::tools::metadata::ToolSource;
    use peko_engine::ToolCallSpec;

    let catalog = Arc::new(ToolCatalog::new());
    catalog
        .register_system(
            Arc::new(WorkflowTool::new(
                Weak::new(),
                Arc::new(crate::ipc::run_tokens::RunTokenRegistry::new()),
            )),
            ToolSource::BuiltIn,
        )
        .await;
    let dispatcher = ToolDispatcher::new(
        catalog,
        Arc::new(WorkspaceHookDispatcher::new()),
        Arc::new(AsyncExecutionRouter::new()),
        None,
    );
    let (display, _, success) = dispatcher
        .execute(ToolCallSpec::new(
            "Workflow",
            json!({"path":"anything.py", "_workflow_depth":MAX_WORKFLOW_DEPTH}),
        ))
        .await
        .unwrap();
    assert!(!success);
    assert!(display.contains("nesting depth"), "{display}");
    assert!(!display.contains("Invalid arguments"), "{display}");
}

#[tokio::test]
async fn agent_dispatch_requires_the_role_argument() {
    use crate::tools::metadata::ToolSource;

    let tooling = crate::tools::runtime::ToolingRuntime::standalone();
    tooling
        .catalog()
        .register_system(
            Arc::new(AgentTool::new(Arc::new(
                messaging::agent::TestSubagentRuntime::new(),
            ))),
            ToolSource::BuiltIn,
        )
        .await;
    let (display, _, success) = tooling
        .dispatcher()
        .execute(peko_engine::ToolCallSpec::new(
            "Agent",
            json!({"prompt":"work", "agent":"worker", "path":"child"}),
        ))
        .await
        .unwrap();
    assert!(!success);
    assert!(display.contains("role"), "{display}");
    assert!(display.contains("required"), "{display}");
}

#[tokio::test]
async fn glob_dispatch_uses_the_callers_workspace_and_preserves_explicit_paths() {
    use crate::tools::metadata::ToolSource;

    let caller = tempfile::tempdir().unwrap();
    let fallback = tempfile::tempdir().unwrap();
    std::fs::write(caller.path().join("caller.txt"), "caller").unwrap();
    std::fs::write(fallback.path().join("fallback.txt"), "fallback").unwrap();
    let tooling = crate::tools::runtime::ToolingRuntime::standalone();
    tooling
        .catalog()
        .register_system(
            Arc::new(GlobTool::new().with_workspace(fallback.path())),
            ToolSource::BuiltIn,
        )
        .await;
    for (params, expected, absent) in [
        (json!({"pattern":"*.txt"}), "caller.txt", "fallback.txt"),
        (
            json!({"pattern":"*.txt", "path":fallback.path()}),
            "fallback.txt",
            "caller.txt",
        ),
    ] {
        let mut call = peko_engine::ToolCallSpec::new("Glob", params);
        call.workspace = Some(caller.path().to_string_lossy().into_owned());
        let (_, result, success) = tooling.dispatcher().execute(call).await.unwrap();
        assert!(success, "{result}");
        let output = result.to_string();
        assert!(output.contains(expected), "{output}");
        assert!(!output.contains(absent), "{output}");
    }
}
