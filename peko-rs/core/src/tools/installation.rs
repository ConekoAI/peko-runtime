//! Built-in composition: runtime defaults, daemon services, principal
//! services, and private run bindings. Implementations stay in their domains;
//! registration lifetimes and the inventory live here.

use crate::common::paths::PathResolver;
use crate::tools::builtin::{
    channel, messaging, AgentCatalogTool, AsyncListTool, AsyncOutputTool, AsyncSpawnTool,
    AsyncStatusTool, AsyncStopTool, BashTool, CallerAwareSessionTool, ChannelReadTool, EditTool,
    GlobTool, GrepTool, ModelCallTool, ModelListTool, PlanAddStepTool, PlanCloseTool,
    PlanCreateTool, PlanGetTool, PlanListTool, PlanMarkStepTool, PlanRecordEvidenceTool, ReadTool,
    SkillTool, TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool, WorkflowTool, WriteTool,
};
use crate::tools::catalog::ToolCatalog;
use crate::tools::metadata::ToolSource;
use crate::tools::runtime::ToolingRuntime;
use anyhow::Result;
use peko_channel::ChannelPort;
use peko_cron::tools::{
    CronCreateTool, CronDeleteTool, CronHistoryTool, CronListTool, CronTriggerTool, CronUpdateTool,
};
use peko_subject::PrincipalId;
use peko_tools_core::Tool;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinScope {
    Runtime,
    Principal,
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationPhase {
    Runtime,
    Daemon,
    Workspace,
    PrincipalServices,
    Async,
    Run,
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinInstallation {
    pub name: &'static str,
    pub scope: BuiltinScope,
    pub phase: InstallationPhase,
    pub run_binding: bool,
}

macro_rules! builtin_manifest {
    ($($names:ident: $scope:ident, $phase:ident, $run_binding:literal => [$($name:literal),* $(,)?];)*) => {
        $(pub const $names: &[&str] = &[$($name),*];)*
        pub const BUILTIN_INSTALLATIONS: &[BuiltinInstallation] = &[
            $($(BuiltinInstallation { name: $name, scope: BuiltinScope::$scope, phase: InstallationPhase::$phase, run_binding: $run_binding },)*)*
        ];
    };
}

builtin_manifest! {
    RUNTIME_TOOL_NAMES: Runtime, Runtime, false => [
        "Bash", "Read", "Write", "Edit", "Glob", "Grep",
        "CronCreate", "CronDelete", "CronList", "CronUpdate", "CronTrigger", "CronHistory", "ChannelRead"
    ];
    DAEMON_TOOL_NAMES: Runtime, Daemon, false => ["ModelCall", "Workflow"];
    DAEMON_CALLER_TOOL_NAMES: Runtime, Daemon, true => ["Session", "Agent"];
    WORKSPACE_TOOL_NAMES: Principal, Workspace, false => ["Skill", "RoleCatalog"];
    PRINCIPAL_SERVICE_TOOL_NAMES: Principal, PrincipalServices, false => [
        "TaskCreate", "TaskGet", "TaskList", "TaskUpdate", "PlanCreate", "PlanList", "PlanGet",
        "PlanAddStep", "PlanMarkStep", "PlanRecordEvidence", "PlanClose", "ChannelSend"
    ];
    ASYNC_TOOL_NAMES: Principal, Async, false => ["AsyncSpawn", "AsyncOutput", "AsyncStatus", "AsyncList", "AsyncStop"];
    RUN_TOOL_NAMES: Run, Run, true => ["ModelList"];
}

// Preserve the public inventory constants while deriving them from the
// installation manifest. A daemon fallback may also have a private run binding.
const fn count_names(run_bindings: bool) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < BUILTIN_INSTALLATIONS.len() {
        let entry = &BUILTIN_INSTALLATIONS[i];
        if if run_bindings {
            entry.run_binding
        } else {
            matches!(entry.scope, BuiltinScope::Runtime)
        } {
            count += 1;
        }
        i += 1;
    }
    count
}

const fn collect_names<const N: usize>(run_bindings: bool) -> [&'static str; N] {
    let mut names = [""; N];
    let mut i = 0;
    let mut count = 0;
    while i < BUILTIN_INSTALLATIONS.len() {
        let entry = &BUILTIN_INSTALLATIONS[i];
        if if run_bindings {
            entry.run_binding
        } else {
            matches!(entry.scope, BuiltinScope::Runtime)
        } {
            names[count] = entry.name;
            count += 1;
        }
        i += 1;
    }
    names
}

/// Runtime defaults, including caller-aware daemon fallbacks.
pub const GLOBAL_TOOL_NAMES: &[&str] = &collect_names::<{ count_names(false) }>(false);
/// Tools with private run bindings, including Session and Agent overrides.
pub const AGENT_SPECIFIC_TOOL_NAMES: &[&str] = &collect_names::<{ count_names(true) }>(true);

pub fn names_for_scope(scope: BuiltinScope) -> Vec<&'static str> {
    BUILTIN_INSTALLATIONS
        .iter()
        .filter(|entry| entry.scope == scope)
        .map(|entry| entry.name)
        .collect()
}

async fn install_defaults(
    catalog: &ToolCatalog,
    principal: &PrincipalId,
    phase: InstallationPhase,
    tools: Vec<Arc<dyn Tool>>,
) -> Result<()> {
    for tool in tools {
        anyhow::ensure!(
            BUILTIN_INSTALLATIONS
                .iter()
                .any(|entry| entry.name == tool.name() && entry.phase == phase),
            "tool '{}' is not declared for {phase:?} installation",
            tool.name()
        );
        catalog
            .register_default(tool, ToolSource::BuiltIn, principal)
            .await;
    }
    Ok(())
}

pub async fn install_runtime(
    catalog: &ToolCatalog,
    path_resolver: &PathResolver,
    channel_port: Arc<dyn ChannelPort>,
) -> Result<()> {
    let workspace = path_resolver
        .agent_workspace(".")
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    // F42 — Bash/Read/Write/Edit/Glob/Grep's default cwd is `<data_dir>/workspaces`,
    // but PathResolver::ensure_dirs doesn't create it (it's lazy-created per-agent).
    // Without this, every Bash call on a fresh principal fails with a context-less
    // "Failed to execute Bash command" because `cmd.current_dir(<missing>)` chdir's
    // before execve. Best-effort matches the daemon-init convention in
    // `daemon/state.rs:771`. See scripts/e2e/reports/2026-08-01-bash-tool-cwd-bug.md.
    let _ = tokio::fs::create_dir_all(&workspace).await;

    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(BashTool::new().with_workspace(workspace.clone())),
        Arc::new(ReadTool::new().with_workspace(workspace.clone())),
        // ADR-065: Write/Edit acquire a fail-fast per-file lock
        // keyed on the canonical target path, so agents sharing
        // this runtime cannot silently clobber each other's edits
        // (the F33 ParallelGate only serializes within one agent).
        Arc::new(
            WriteTool::new()
                .with_workspace(workspace.clone())
                .with_lock_dir(peko_tools_core::default_data_dir().join("locks")),
        ),
        Arc::new(GlobTool::new().with_workspace(workspace.clone())),
        Arc::new(GrepTool::new().with_workspace(workspace.clone())),
        Arc::new(
            EditTool::new()
                .with_workspace(workspace.clone())
                .with_lock_dir(peko_tools_core::default_data_dir().join("locks")),
        ),
        Arc::new(CronCreateTool::new()),
        Arc::new(CronDeleteTool::new()),
        Arc::new(CronListTool::new()),
        Arc::new(CronUpdateTool::new()),
        Arc::new(CronTriggerTool::new()),
        Arc::new(CronHistoryTool::new()),
        // PR-4a — channel reading as a tool. The principal's
        // agentic loop calls this on demand; the daemon-side
        // audit ring buffer (PR-3c) observes every channel event
        // regardless of whether the tool fires. The principal
        // boundary is preserved because the principal invokes the
        // tool itself.
        Arc::new(ChannelReadTool::new(channel_port.clone())),
    ];

    install_defaults(
        catalog,
        PrincipalId::system(),
        InstallationPhase::Runtime,
        tools,
    )
    .await
}

pub(crate) async fn install_daemon(
    catalog: &ToolCatalog,
    manager: Weak<crate::principal::manager::PrincipalManager>,
    inbox: Arc<peko_session::InboxRegistry>,
    tokens: Arc<crate::ipc::run_tokens::RunTokenRegistry>,
    observability: Arc<peko_observability::Observability>,
) -> Result<()> {
    install_agent_fallback(catalog, manager.clone(), observability).await?;
    install_defaults(
        catalog,
        PrincipalId::system(),
        InstallationPhase::Daemon,
        vec![
            Arc::new(ModelCallTool::new(manager.clone())),
            Arc::new(WorkflowTool::new(manager.clone(), tokens)),
            Arc::new(CallerAwareSessionTool::for_daemon(manager, inbox)),
        ],
    )
    .await
}

/// Also used by standalone cron owners which have a manager but no daemon
/// composition root. Installs the same stable adapter, never a captured run.
pub(crate) async fn install_agent_fallback(
    catalog: &ToolCatalog,
    manager: Weak<crate::principal::manager::PrincipalManager>,
    observability: Arc<peko_observability::Observability>,
) -> Result<()> {
    install_defaults(
        catalog,
        PrincipalId::system(),
        InstallationPhase::Daemon,
        vec![Arc::new(
            messaging::caller_aware::CallerAwareAgentTool::new(manager, observability),
        )],
    )
    .await
}

pub(crate) async fn install_workspace(
    catalog: &ToolCatalog,
    workspace: &Path,
    principal: &PrincipalId,
) -> Result<()> {
    install_defaults(
        catalog,
        principal,
        InstallationPhase::Workspace,
        vec![
            Arc::new(SkillTool::new(Arc::new(
                crate::extensions::skill::WorkspaceSkillRuntime::new(workspace.join("skills")),
            ))),
            Arc::new(AgentCatalogTool::from_workspace(workspace.to_path_buf())),
        ],
    )
    .await
}

pub(crate) struct PrincipalBindings<'a> {
    pub sessions_dir: Option<PathBuf>,
    pub plan: Option<Arc<dyn peko_plan::PlanPort>>,
    pub caller_did: Option<&'a str>,
}

pub(crate) async fn install_principal_services(
    tooling: &Arc<ToolingRuntime>,
    principal: &PrincipalId,
    bindings: PrincipalBindings<'_>,
) -> Result<()> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if let Some(dir) = bindings.sessions_dir {
        let runtime = Arc::new(crate::session::todo_runtime_impl::TodoStorageRuntime::new(
            Arc::new(peko_session::todos::TodoStorage::new(dir)),
        ));
        tools.extend([
            Arc::new(TaskCreateTool::new(runtime.clone())) as Arc<dyn Tool>,
            Arc::new(TaskGetTool::new(runtime.clone())),
            Arc::new(TaskListTool::new(runtime.clone())),
            Arc::new(TaskUpdateTool::new(runtime)),
        ]);
    }
    if let Some(port) = bindings.plan {
        tools.extend([
            Arc::new(PlanCreateTool::new(port.clone())) as Arc<dyn Tool>,
            Arc::new(PlanListTool::new(port.clone())),
            Arc::new(PlanGetTool::new(port.clone())),
            Arc::new(PlanAddStepTool::new(port.clone())),
            Arc::new(PlanMarkStepTool::new(port.clone())),
            Arc::new(PlanRecordEvidenceTool::new(port.clone())),
            Arc::new(PlanCloseTool::new(port)),
        ]);
    }
    if let Some(did) = bindings.caller_did {
        if let Some(tool) = channel::build_channel_send_tool(tooling, did) {
            tools.push(tool);
        }
    }
    install_defaults(
        tooling.catalog(),
        principal,
        InstallationPhase::PrincipalServices,
        tools,
    )
    .await
}

pub(crate) async fn install_async(
    tooling: &Arc<ToolingRuntime>,
    principal: &PrincipalId,
    inbox: Arc<peko_session::InboxRegistry>,
) -> Result<()> {
    let executor = tooling.async_executor_for(principal, inbox).await;
    let runtime = Arc::new(crate::async_exec::executor::AsyncExecutorRuntime::new(
        executor,
        Arc::downgrade(tooling),
        None,
        principal.clone(),
    ))
    .as_shared();
    install_defaults(
        tooling.catalog(),
        principal,
        InstallationPhase::Async,
        vec![
            Arc::new(AsyncSpawnTool::new(runtime.clone())),
            Arc::new(AsyncOutputTool::new(runtime.clone())),
            Arc::new(AsyncStatusTool::new(runtime.clone())),
            Arc::new(AsyncListTool::new(runtime.clone())),
            Arc::new(AsyncStopTool::new(runtime)),
        ],
    )
    .await
}

pub(crate) async fn install_run(
    tooling: &ToolingRuntime,
    principal: &PrincipalId,
    executor: Arc<crate::agents::subagent_executor::SubagentExecutor>,
    sessions: crate::session::session_runtime_impl::SessionManagerRuntime,
    session_manager: Arc<tokio::sync::RwLock<peko_session::SessionManager>>,
    model_catalog: Option<Arc<peko_providers::catalog::ModelCatalog>>,
) -> Result<()> {
    let surface = tooling.services().channel_port().map(|port| {
        Arc::new(crate::principal::child_turns::PeerTurnSurfaceImpl::new(
            session_manager,
            port,
            principal.clone(),
        )) as Arc<dyn crate::agents::subagent_executor::PeerTurnSurface>
    });
    let executor = Arc::new((*executor).clone().with_peer_turn_surface(surface));
    tooling
        .catalog()
        .register(
            Arc::new(messaging::new_agent_tool(executor)),
            ToolSource::BuiltIn,
            principal,
        )
        .await;
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if let Some(catalog) = model_catalog {
        tools.push(Arc::new(ModelListTool::new(Arc::downgrade(&catalog))));
    }
    install_defaults(tooling.catalog(), principal, InstallationPhase::Run, tools).await?;
    // The offline adapter overrides the daemon's caller-aware Session default
    // only inside this run. Both resolve the caller from ToolContext.
    tooling
        .catalog()
        .register(
            Arc::new(CallerAwareSessionTool::for_agent(sessions)),
            ToolSource::BuiltIn,
            principal,
        )
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn partial_runtime_install_fills_gaps_and_preserves_existing_instances() {
        let catalog = ToolCatalog::new();
        let custom: Arc<dyn Tool> = Arc::new(BashTool::new().with_workspace("/custom"));
        catalog
            .register_system(Arc::clone(&custom), ToolSource::BuiltIn)
            .await;
        install_runtime(
            &catalog,
            &PathResolver::new(),
            Arc::new(peko_channel::NoopChannelPort),
        )
        .await
        .unwrap();
        assert!(Arc::ptr_eq(
            &custom,
            &catalog.get("Bash", PrincipalId::system()).await.unwrap().0
        ));
        let read = catalog.get("Read", PrincipalId::system()).await.unwrap().0;
        install_runtime(
            &catalog,
            &PathResolver::new(),
            Arc::new(peko_channel::NoopChannelPort),
        )
        .await
        .unwrap();
        assert!(Arc::ptr_eq(
            &read,
            &catalog.get("Read", PrincipalId::system()).await.unwrap().0
        ));
        let expected: std::collections::HashSet<_> = RUNTIME_TOOL_NAMES.iter().copied().collect();
        let actual: std::collections::HashSet<_> = catalog
            .list_tool_names(PrincipalId::system())
            .await
            .into_iter()
            .collect();
        assert_eq!(actual, expected.into_iter().map(String::from).collect());
    }

    #[tokio::test]
    async fn principal_services_are_stable_and_role_catalog_refreshes_without_registration() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("roles")).unwrap();
        let tooling = ToolingRuntime::standalone();
        let principal = PrincipalId::generate();
        install_workspace(tooling.catalog(), workspace.path(), &principal)
            .await
            .unwrap();
        let roles = tooling
            .catalog()
            .get("RoleCatalog", &principal)
            .await
            .unwrap()
            .0;
        assert_eq!(
            roles.execute(serde_json::json!({})).await.unwrap()["total"],
            0
        );
        let file = workspace.path().join("roles/worker.md");
        std::fs::write(&file, "---\nname: Worker\ndescription: Original\n---\nWork").unwrap();
        assert_eq!(
            roles.execute(serde_json::json!({})).await.unwrap()["agents"][0]["description"],
            "Original"
        );
        std::fs::write(&file, "---\nname: Worker\ndescription: Updated\n---\nWork").unwrap();
        assert_eq!(
            roles.execute(serde_json::json!({})).await.unwrap()["agents"][0]["description"],
            "Updated"
        );
        std::fs::remove_file(file).unwrap();
        assert_eq!(
            roles.execute(serde_json::json!({})).await.unwrap()["total"],
            0
        );
        install_workspace(tooling.catalog(), workspace.path(), &principal)
            .await
            .unwrap();
        assert!(Arc::ptr_eq(
            &roles,
            &tooling
                .catalog()
                .get("RoleCatalog", &principal)
                .await
                .unwrap()
                .0
        ));
        assert!(tooling
            .catalog()
            .get("Skill", PrincipalId::system())
            .await
            .is_none());
        assert!(tooling
            .catalog()
            .get("RoleCatalog", &PrincipalId::generate())
            .await
            .is_none());
    }
}
