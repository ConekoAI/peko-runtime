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
        Arc::new(AsyncSpawnTool::new(asynchronous.clone())),
        Arc::new(AsyncOutputTool::new(asynchronous.clone())),
        Arc::new(AsyncStatusTool::new(asynchronous.clone())),
        Arc::new(AsyncListTool::new(asynchronous.clone())),
        Arc::new(AsyncStopTool::new(asynchronous)),
        Arc::new(TaskCreateTool::new(todos.clone())),
        Arc::new(TaskGetTool::new(todos.clone())),
        Arc::new(TaskListTool::new(todos.clone())),
        Arc::new(TaskUpdateTool::new(todos)),
        Arc::new(PlanCreateTool::new(plans.clone())),
        Arc::new(PlanListTool::new(plans.clone())),
        Arc::new(PlanGetTool::new(plans.clone())),
        Arc::new(PlanAddStepTool::new(plans.clone())),
        Arc::new(PlanMarkStepTool::new(plans.clone())),
        Arc::new(PlanRecordEvidenceTool::new(plans.clone())),
        Arc::new(PlanCloseTool::new(plans)),
        Arc::new(peko_cron::CronCreateTool::new()),
        Arc::new(peko_cron::CronListTool::new()),
        Arc::new(peko_cron::CronDeleteTool::new()),
        Arc::new(peko_cron::CronUpdateTool::new()),
        Arc::new(peko_cron::CronTriggerTool::new()),
        Arc::new(peko_cron::CronHistoryTool::new()),
    ]
}

#[test]
fn inventory_matches_implementations_and_schemas_compile() {
    let implementations = tools();
    let actual: HashSet<_> = implementations.iter().map(|tool| tool.name()).collect();
    let declared: HashSet<_> = crate::principal::runtime::builtin_tools::all_tool_names()
        .into_iter()
        .collect();
    assert_eq!(actual, declared);
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
            "CronCreate",
            json!({"message":"remind me", "delay":"5m"}),
            true,
        ),
        (
            "CronCreate",
            json!({"tool":"Bash", "interval_ms":60000, "one_shot":true}),
            true,
        ),
        ("CronCreate", json!({"delay":"5m"}), false),
        ("CronCreate", json!({"message":"remind me"}), false),
        (
            "CronCreate",
            json!({"message":"remind me", "tool":"Bash", "delay":"5m"}),
            false,
        ),
        (
            "CronCreate",
            json!({"message":"remind me", "delay":"5m", "cron":"* * * * *"}),
            false,
        ),
        ("CronUpdate", json!({"id":"job", "enabled":false}), true),
        (
            "CronUpdate",
            json!({"id":"job", "label":"legacy", "enabled":false}),
            true,
        ),
        ("CronUpdate", json!({"id":"job"}), false),
        ("CronUpdate", json!({"enabled":false}), false),
        (
            "AsyncSpawn",
            json!({"tool":"Read", "params":{}, "timeout_secs":null}),
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
async fn agent_dispatch_accepts_the_legacy_role_argument() {
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
    // Reaching role resolution proves validation accepted the alias;
    // the empty fixture intentionally has no worker role to run.
    assert!(!success);
    assert!(
        display.contains("Agent template 'worker' not registered"),
        "{display}"
    );
    assert!(!display.contains("Invalid arguments"), "{display}");
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
