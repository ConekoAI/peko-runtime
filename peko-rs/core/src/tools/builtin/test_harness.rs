//! Dispatcher-level harness for built-in tool tests.
//!
//! [`ToolHarness::new`] installs all 19 built-in tools behind the real
//! [`ToolDispatcher`](crate::tools::dispatcher::ToolDispatcher) — argument
//! validation, workspace injection, hooks, timeout routing, and the
//! `tool.call` audit sink — and calls them the way an agentic loop does:
//! with a principal, a session, and a workspace on every call.
//!
//! Backends prefer production storage in a tempdir; fakes stand in only
//! where the real dependency is a daemon:
//!
//! | Backend | Tools |
//! |---|---|
//! | temp workspace | Bash, Read, Write, Edit, Glob, Grep, Skill, RoleCatalog |
//! | real `AsyncExecutor` over this catalog | Async |
//! | real `ChannelStore` | ChannelRead, ChannelSend (local branches) |
//! | real `CronScheduler` files (`peko_cron::testing`) | Cron |
//! | real `ModelCatalog` file | ModelList |
//! | in-memory fakes | Task, Plan, Agent, Session |
//! | unbound — fail closed without a `PrincipalManager` | ModelCall, Workflow |
//!
//! Add behavior tests next to the tool they cover; use this harness when a
//! test needs dispatch semantics, more than one tool, or a production
//! backend. Domain-specific fakes stay in their own modules.

use super::*;
use crate::async_exec::executor::{standalone_inbox_registry, AsyncExecutor, AsyncExecutorRuntime};
use crate::tools::metadata::ToolSource;
use crate::tools::runtime::ToolingRuntime;
use peko_channel::{ChannelConfig, ChannelId, ChannelPort, ChannelStore, CreateOpts};
use peko_cron::testing::FileCronRuntime;
use peko_observability::Observability;
use peko_subject::PrincipalId;
use peko_tools_core::Tool;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

/// Principal every call runs as unless a test overrides the caller.
pub(crate) const PRINCIPAL: &str = "did:peko:harness";
/// Session every call runs in unless a test overrides the caller.
pub(crate) const SESSION: &str = "harness-session";

/// Identity a call is attributed to.
#[derive(Clone, Debug)]
pub(crate) struct Caller {
    pub principal: String,
    pub session: String,
}

impl Default for Caller {
    fn default() -> Self {
        Self {
            principal: PRINCIPAL.into(),
            session: SESSION.into(),
        }
    }
}

impl Caller {
    pub(crate) fn principal(principal: &str) -> Self {
        Self {
            principal: principal.into(),
            ..Self::default()
        }
    }
}

/// One dispatched call: the LLM-facing display text, the structured
/// result, and whether the dispatcher counted it a success.
#[derive(Debug)]
pub(crate) struct Outcome {
    pub display: String,
    pub value: Value,
    pub success: bool,
}

impl Outcome {
    /// The structured result, panicking with the display text on failure.
    #[track_caller]
    pub(crate) fn ok(self) -> Value {
        assert!(self.success, "expected success, got: {}", self.display);
        self.value
    }

    /// The failure text, which must mention `needle`.
    #[track_caller]
    pub(crate) fn err(self, needle: &str) -> String {
        assert!(!self.success, "expected failure, got: {}", self.value);
        assert!(
            self.display.contains(needle),
            "failure should mention {needle:?}: {}",
            self.display
        );
        self.display
    }
}

/// The fakes behind tools whose real backends need a daemon. Shared with
/// [`builtin_tools`] so schema tests and the harness build one inventory.
pub(crate) struct Fakes {
    pub todos: Arc<tasks::TestTodoRuntime>,
    pub plans: Arc<plan::TestPlanPort>,
    pub subagents: Arc<messaging::agent::TestSubagentRuntime>,
}

impl Default for Fakes {
    fn default() -> Self {
        Self {
            todos: Arc::new(tasks::TestTodoRuntime::new()),
            plans: Arc::new(plan::TestPlanPort::new()),
            subagents: Arc::new(messaging::agent::TestSubagentRuntime::new()),
        }
    }
}

/// Production-backed dependencies the harness owns.
pub(crate) struct Backends {
    pub workspace: PathBuf,
    pub cron: Arc<FileCronRuntime>,
    pub channels: Arc<dyn ChannelPort>,
    pub async_runtime: SharedAsyncRuntime,
    pub models: Weak<peko_providers::catalog::ModelCatalog>,
}

/// Every built-in tool, one per wire name, wired to `backends` and `fakes`.
pub(crate) fn builtin_tools(backends: &Backends, fakes: &Fakes) -> Vec<Arc<dyn Tool>> {
    let workspace = &backends.workspace;
    vec![
        Arc::new(BashTool::new().with_workspace(workspace.clone())),
        Arc::new(ReadTool::new().with_workspace(workspace.clone())),
        Arc::new(WriteTool::new().with_workspace(workspace.clone())),
        Arc::new(EditTool::new().with_workspace(workspace.clone())),
        Arc::new(GlobTool::new().with_workspace(workspace.clone())),
        Arc::new(GrepTool::new().with_workspace(workspace.clone())),
        Arc::new(SessionTool::new(Arc::new(SessionCache::new(SESSION)))),
        Arc::new(AgentTool::new(fakes.subagents.clone())),
        Arc::new(AgentCatalogTool::from_workspace(workspace.clone())),
        Arc::new(SkillTool::new(Arc::new(
            crate::extensions::skill::WorkspaceSkillRuntime::new(workspace.join("skills")),
        ))),
        Arc::new(ModelListTool::new(backends.models.clone())),
        Arc::new(ModelCallTool::new(Weak::new())),
        Arc::new(WorkflowTool::new(
            Weak::new(),
            Arc::new(crate::ipc::run_tokens::RunTokenRegistry::new()),
        )),
        Arc::new(ChannelReadTool::new(backends.channels.clone())),
        Arc::new(ChannelSendTool::new_local_only(
            backends.channels.clone(),
            PRINCIPAL.into(),
        )),
        Arc::new(AsyncTool::new(backends.async_runtime.clone())),
        Arc::new(TaskTool::new(fakes.todos.clone())),
        Arc::new(PlanTool::new(fakes.plans.clone())),
        Arc::new(peko_cron::CronTool::with_runtime(backends.cron.clone())),
    ]
}

/// All 19 built-in tools behind the production dispatcher.
pub(crate) struct ToolHarness {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    pub tooling: Arc<ToolingRuntime>,
    pub audit: Arc<Observability>,
    pub cron: Arc<FileCronRuntime>,
    pub channels: Arc<dyn ChannelPort>,
    /// Keeps `ModelList`'s weak catalog handle alive.
    _models: Arc<peko_providers::catalog::ModelCatalog>,
    pub fakes: Fakes,
}

impl ToolHarness {
    pub(crate) async fn new() -> Self {
        let root = tempfile::tempdir().expect("harness tempdir");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("harness workspace");
        let audit = Arc::new(
            Observability::with_audit_dir("harness", root.path().join("audit"))
                .expect("harness audit sink"),
        );
        let tooling = Arc::new(ToolingRuntime::new(
            Arc::new(crate::tools::catalog::ToolCatalog::new()),
            Arc::new(crate::extensions::workspace_dispatcher::WorkspaceHookDispatcher::new()),
            Arc::new(crate::extensions::framework::core::config::ExtensionServices::new()),
            Arc::new(
                crate::extensions::framework::transport::async_router::AsyncExecutionRouter::new(),
            ),
            Some(audit.clone()),
        ));
        let channels: Arc<dyn ChannelPort> = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: root.path().join("runtime"),
            shared_dir: None,
        }));
        let models =
            peko_providers::catalog::ModelCatalog::load_or_init(&root.path().join("models.toml"))
                .await
                .expect("harness model catalog");
        let async_runtime = Arc::new(AsyncExecutorRuntime::new(
            Arc::new(AsyncExecutor::new(standalone_inbox_registry())),
            Arc::downgrade(&tooling),
            None,
            PrincipalId(PRINCIPAL.into()),
        ))
        .as_shared();
        let backends = Backends {
            workspace: workspace.clone(),
            cron: Arc::new(FileCronRuntime::new(root.path().join("cron"))),
            channels: channels.clone(),
            async_runtime,
            models: Arc::downgrade(&models),
        };
        let fakes = Fakes::default();
        for tool in builtin_tools(&backends, &fakes) {
            tooling
                .catalog()
                .register_system(tool, ToolSource::BuiltIn)
                .await;
        }
        Self {
            workspace,
            tooling,
            audit,
            cron: backends.cron,
            channels,
            _models: models,
            fakes,
            _root: root,
        }
    }

    /// The caller's workspace: relative tool paths resolve here.
    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Write `contents` to `relative` inside the workspace.
    pub(crate) fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.workspace.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create workspace parent");
        }
        std::fs::write(&path, contents).expect("write workspace file");
        path
    }

    /// Dispatch `tool` as the default caller.
    pub(crate) async fn call(&self, tool: &str, params: Value) -> Outcome {
        self.call_as(&Caller::default(), tool, params).await
    }

    /// Dispatch `tool` as `caller`, the way the agentic loop does.
    pub(crate) async fn call_as(&self, caller: &Caller, tool: &str, params: Value) -> Outcome {
        let mut call = peko_engine::ToolCallSpec::new(tool, params);
        call.principal_id = Some(caller.principal.clone());
        call.session_id = Some(caller.session.clone());
        call.agent_id = Some("harness-agent".into());
        call.workspace = Some(self.workspace.to_string_lossy().into_owned());
        match self.tooling.dispatcher().execute(call).await {
            Ok((display, value, success)) => Outcome {
                display,
                value,
                success,
            },
            Err(error) => Outcome {
                display: format!("{error:#}"),
                value: Value::Null,
                success: false,
            },
        }
    }

    /// A channel created by `creator`, who becomes its first member.
    pub(crate) async fn channel(&self, creator: &str, name: &str) -> ChannelId {
        self.channels
            .create(
                &PrincipalId(creator.into()),
                CreateOpts {
                    name: name.into(),
                    ..Default::default()
                },
            )
            .await
            .expect("create harness channel")
    }

    /// `tool.call` audit events, oldest first.
    pub(crate) async fn audit_events(&self) -> Vec<peko_observability::AuditEvent> {
        let mut events = self.audit.get_audit_log(10_000).await;
        events.sort_by_key(|event| event.timestamp);
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    /// Every built-in tool dispatches through the harness with one
    /// representative call, each audited once. The covered set must equal
    /// the installation manifest, so a new tool fails here until it has a
    /// harness backend and a smoke case.
    #[tokio::test]
    async fn every_builtin_tool_dispatches_through_the_harness() {
        let harness = ToolHarness::new().await;
        harness.write("notes/hello.txt", "hello harness\n");
        harness.write(
            "skills/greet/SKILL.md",
            "---\nname: greet\ndescription: Greets\n---\nHello $ARGUMENTS\n",
        );
        harness.write(
            "roles/writer.md",
            "---\nname: Writer\ndescription: Drafts prose\n---\nYou write.\n",
        );
        harness.fakes.subagents.register_agent(
            "writer",
            Arc::new(crate::agents::subagent_runtime_impl::AgentPrompt {
                name: "writer".into(),
                path: harness.workspace().join("roles/writer.md"),
                frontmatter: crate::agents::subagent_runtime_impl::AgentPromptFrontmatter {
                    name: Some("writer".into()),
                    description: None,
                    color: None,
                },
                body: "You write.".into(),
            }),
        );
        let channel = harness.channel(PRINCIPAL, "smoke").await.to_string();

        let succeeds: Vec<(&str, Value, &str)> = vec![
            ("Bash", json!({"command":"echo dispatched"}), "dispatched"),
            (
                "Read",
                json!({"file_path":"notes/hello.txt"}),
                "hello harness",
            ),
            (
                "Write",
                json!({"file_path":"notes/new.txt", "content":"new"}),
                "new.txt",
            ),
            (
                "Edit",
                json!({"file_path":"notes/hello.txt", "old_string":"hello", "new_string":"edited"}),
                "hello.txt",
            ),
            ("Glob", json!({"pattern":"notes/*.txt"}), "hello.txt"),
            ("Grep", json!({"pattern":"edited"}), "hello.txt"),
            (
                "Skill",
                json!({"name":"greet", "args":["world"]}),
                "Hello world",
            ),
            ("RoleCatalog", json!({}), "writer"),
            (
                "Agent",
                json!({"prompt":"draft", "role":"writer", "path":"drafts"}),
                "completed",
            ),
            ("Session", json!({"action":"list"}), ""),
            (
                "ChannelSend",
                json!({"channel": channel, "text":"ping"}),
                "",
            ),
            ("ChannelRead", json!({"channel": channel}), "ping"),
            ("ModelList", json!({}), ""),
            (
                "Task",
                json!({"action":"create", "subject":"smoke"}),
                "smoke",
            ),
            (
                "Plan",
                json!({"action":"create", "title":"smoke", "nodes":[{"step":"one"}]}),
                "smoke",
            ),
            (
                "Async",
                json!({"action":"spawn", "tool":"Read", "params":{"file_path":"notes/hello.txt"}}),
                "task_id",
            ),
            (
                "Cron",
                json!({"action":"create", "message":"tick", "delay":"5m"}),
                "registered",
            ),
        ];
        // Unbound in the harness: these need a PrincipalManager and must
        // fail closed rather than run unattributed.
        let fails_closed: Vec<(&str, Value, &str)> = vec![
            ("ModelCall", json!({"prompt":"hi"}), "rincipal"),
            ("Workflow", json!({"path":"flow.py"}), "rincipal"),
        ];

        let mut covered = BTreeSet::new();
        for (tool, params, needle) in &succeeds {
            let value = harness.call(tool, params.clone()).await.ok();
            assert!(
                value.to_string().contains(needle),
                "{tool}: result should mention {needle:?}: {value}"
            );
            covered.insert(*tool);
        }
        for (tool, params, needle) in &fails_closed {
            harness.call(tool, params.clone()).await.err(needle);
            covered.insert(*tool);
        }

        let inventory: BTreeSet<_> = crate::tools::installation::all_tool_names()
            .into_iter()
            .collect();
        assert_eq!(covered, inventory, "every built-in tool needs a smoke case");
        assert_eq!(
            std::fs::read_to_string(harness.workspace().join("notes/new.txt")).unwrap(),
            "new"
        );

        let events = harness.audit_events().await;
        let audited: Vec<_> = events
            .iter()
            .filter(|event| event.event_type == "tool.call")
            .map(|event| event.details["tool_name"].as_str().unwrap_or_default())
            .collect();
        for tool in &inventory {
            assert!(
                audited.iter().filter(|name| *name == tool).count() >= 1,
                "{tool} was not audited: {audited:?}"
            );
        }
        assert!(events
            .iter()
            .all(|event| event.details["principal_id"] == PRINCIPAL));
    }

    #[tokio::test]
    async fn principal_owned_state_is_scoped_to_the_calling_principal() {
        let harness = ToolHarness::new().await;
        let other = Caller::principal("did:peko:other");
        let job = harness
            .call(
                "Cron",
                json!({"action":"create", "tool":"Bash", "interval_ms":60000}),
            )
            .await
            .ok()["job_id"]
            .clone();
        let listed = harness
            .call_as(&other, "Cron", json!({"action":"list"}))
            .await
            .ok();
        assert_eq!(listed["count"], 0, "{listed}");
        harness
            .call_as(&other, "Cron", json!({"action":"delete", "id": job}))
            .await
            .err("not found");
        assert_eq!(harness.cron.jobs().len(), 1);

        let channel = harness.channel(PRINCIPAL, "members-only").await.to_string();
        let read = harness
            .call_as(&other, "ChannelRead", json!({"channel": channel}))
            .await;
        assert!(
            !read.success || read.value.get("error").is_some(),
            "non-members must not read: {}",
            read.value
        );
    }

    #[tokio::test]
    async fn invalid_arguments_are_rejected_before_any_tool_runs() {
        let harness = ToolHarness::new().await;
        harness
            .call("Write", json!({"file_path":"x.txt"}))
            .await
            .err("content");
        assert!(!harness.workspace().join("x.txt").exists());
        harness
            .call("Cron", json!({"action":"create", "message":"m"}))
            .await
            .err("Invalid arguments");
        assert!(harness.cron.jobs().is_empty());
        harness
            .call("NoSuchTool", json!({}))
            .await
            .err("NoSuchTool");
    }
}
