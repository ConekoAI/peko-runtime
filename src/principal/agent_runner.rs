use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::RwLock;

use crate::agents::agent_config::{AgentConfig, PromptConfig, SystemFileConfig};
use crate::agents::Agent;
use crate::auth::Subject;
use crate::common::paths::PathResolver;
use crate::common::services::AgentService;
use crate::common::types::agent_legacy::ExtensionConfig;
use crate::common::types::message::LlmMessage;
use crate::extensions::agent::{register_agents_with_core, AgentAdapter};
use crate::extensions::builtin::BuiltinToolAdapter;
use crate::extensions::framework::core::ExtensionCore;
use crate::principal::memory::PrincipalMemory;
use crate::principal::router::AgentPromptSummary;
use crate::providers::LlmResolver;
use crate::session::manager::SessionManager;
use crate::session::SessionCreateOptions;
use crate::session::InboxRegistry;
use crate::tools::builtin::{
    AgentCatalogTool, AgentTool, DynamicSessionKeyProvider, PrincipalMemoryTool,
    PrincipalSessionsTool,
};

use super::{agent_prompt::AgentPrompt, config::PrincipalCapabilities};

/// Build an `AgentConfig` from a thin Markdown prompt + Principal capabilities.
pub fn build_agent_config(
    prompt: &AgentPrompt,
    capabilities: &PrincipalCapabilities,
) -> AgentConfig {
    let enabled_extensions: Vec<String> = capabilities
        .tools
        .iter()
        .chain(capabilities.skills.iter())
        .chain(capabilities.mcps.iter())
        .cloned()
        .collect();

    let mut extensions = ExtensionConfig::default();
    extensions.enabled = enabled_extensions;

    AgentConfig {
        name: prompt.name.clone(),
        description: prompt.frontmatter.description.clone(),
        prompt: Some(PromptConfig {
            system: Some(SystemFileConfig {
                max_chars_per_file: 200_000,
                files: Some(vec![prompt.path.to_string_lossy().to_string()]),
            }),
        }),
        extensions: Some(extensions),
        // Inherit sensible defaults for the rest.
        ..AgentConfig::default()
    }
}

/// Run the supervisor agent prompt in a peer-scoped session using a dedicated
/// `ExtensionCore`.
///
/// The supervisor core is isolated from the global core: it carries the
/// principal's own agents as `{{agents}}` hooks, an `Agent` tool that resolves
/// those agents, and principal-scoped session/memory/catalog tools.
pub async fn run_supervisor_prompt(
    prompt: &AgentPrompt,
    capabilities: &PrincipalCapabilities,
    peer: Subject,
    message: String,
    session_id: String,
    sessions_dir: PathBuf,
    resolver: Option<Arc<LlmResolver>>,
    workspace_path: PathBuf,
    available_agents: Vec<AgentPromptSummary>,
    memory: Arc<dyn PrincipalMemory>,
    inbox_registry: Arc<InboxRegistry>,
    session_creation_lock: Arc<tokio::sync::Mutex<()>>,
) -> anyhow::Result<String> {
    let mut config = build_agent_config(prompt, capabilities);

    // Supervisor-specific whitelist.  We include bare tool names so
    // `Agent::init_builtins_async` keeps the tools it registers, plus canonical
    // extension IDs so the core permission checks pass.
    let mut enabled: Vec<String> = vec![
        "Read".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "session".to_string(),
        "CronCreate".to_string(),
        "CronDelete".to_string(),
        "CronList".to_string(),
        "TaskCreate".to_string(),
        "TaskGet".to_string(),
        "TaskList".to_string(),
        "TaskUpdate".to_string(),
    ];
    enabled.extend(capabilities.tools.iter().cloned());
    enabled.extend(capabilities.skills.iter().cloned());
    enabled.extend(capabilities.mcps.iter().cloned());
    enabled.extend(capabilities.agents.iter().cloned());

    let canonical: Vec<String> = vec![
        "builtin:tool:Read",
        "builtin:tool:glob",
        "builtin:tool:grep",
        "builtin:tool:session",
        "builtin:tool:Agent",
        "builtin:tool:AsyncSpawn",
        "builtin:tool:AsyncOutput",
        "builtin:tool:AsyncStatus",
        "builtin:tool:AsyncList",
        "builtin:tool:AsyncStop",
        "builtin:tool:CronCreate",
        "builtin:tool:CronDelete",
        "builtin:tool:CronList",
        "builtin:tool:TaskCreate",
        "builtin:tool:TaskGet",
        "builtin:tool:TaskList",
        "builtin:tool:TaskUpdate",
        "builtin:tool:principal_sessions",
        "builtin:tool:principal_memory",
        "builtin:tool:agent_catalog",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    enabled.extend(canonical);

    config.extensions = Some(ExtensionConfig {
        enabled,
        ..config.extensions.unwrap_or_default()
    });

    // Dedicated ExtensionCore for this supervisor decision.
    let core = Arc::new(ExtensionCore::new());
    let path_resolver = PathResolver::new();
    crate::engine::tool_runtime::ToolRuntime::register_builtins(&core, &path_resolver).await?;

    // Register the principal's agents as `{{agents}}` hooks.
    let agents_dir = workspace_path.join("agents");
    if agents_dir.exists() {
        let adapter = AgentAdapter::new();
        let discovered = adapter.discover_agents(&agents_dir);
        let _ = register_agents_with_core(&core, discovered).await;
    }

    // Build a SessionManager scoped to the principal's sessions directory.
    let session_manager = SessionManager::new()
        .with_sessions_dir_internal(sessions_dir)
        .with_agent_name(&prompt.name)
        .with_peer_principal(peer.clone())
        .with_user(&peer.to_string());
    let session_manager = Arc::new(RwLock::new(session_manager));

    // Open or create the supervisor session.  Hold the per-principal
    // session-creation lock while touching the shared session index so
    // concurrent peers don't corrupt it.
    let session = {
        let _creation_guard = session_creation_lock.lock().await;
        let maybe_handle = {
            let mut mgr = session_manager.write().await;
            mgr.open_session(&session_id).await?
        };
        if let Some(handle) = maybe_handle {
            handle.base().clone()
        } else {
            let mut mgr = session_manager.write().await;
            let options = SessionCreateOptions::new().with_session_id(&session_id);
            let handle = mgr
                .create_session(&prompt.name, &peer, options)
                .await
                .context("failed to create supervisor session")?;
            handle.base().clone()
        }
    };

    let history: Vec<LlmMessage> = session.read().await.load_history().await?;

    // Cold-start the supervisor agent on the dedicated core, wiring it to the
    // same inbox registry the Principal boundary uses for steering messages.
    let agent = Agent::new_with_session_manager_resolver_and_core(
        config,
        Arc::clone(&session_manager),
        resolver,
        Arc::clone(&core),
        Some(inbox_registry),
    )
    .await?;

    // Register the principal-scoped tools after `Agent::new*` but before
    // execution so they are available on the supervisor's private core.
    let session_key_provider = Arc::new(DynamicSessionKeyProvider::new(format!(
        "agent:{}:cli:default",
        prompt.name
    )));

    let subagent_executor = Arc::new(
        crate::agents::subagent_executor::SubagentExecutor::new(
            Arc::clone(&session_manager),
            &prompt.name,
            5,
        )
        .with_provider(agent.provider_arc().ok_or_else(|| {
            anyhow::anyhow!("supervisor agent has no provider configured")
        })?)
        .with_agent_config(agent.config.clone()),
    );

    let agent_service = AgentService::for_principal(&workspace_path);
    let agent_tool = Arc::new(AgentTool::with_agent_service_and_session_provider(
        subagent_executor,
        agent_service,
        Box::new(session_key_provider.clone()),
    ));
    BuiltinToolAdapter::register_tool(&core, agent_tool).await?;

    BuiltinToolAdapter::register_tool(
        &core,
        Arc::new(PrincipalSessionsTool::new(Arc::clone(&memory))),
    )
    .await?;
    BuiltinToolAdapter::register_tool(
        &core,
        Arc::new(PrincipalMemoryTool::new(Arc::clone(&memory))),
    )
    .await?;
    BuiltinToolAdapter::register_tool(
        &core,
        Arc::new(AgentCatalogTool::new(available_agents)),
    )
    .await?;

    // Stamp the current session key so the Agent tool can auto-detect it.
    {
        let sid = session.read().await.id.clone();
        session_key_provider.set_session_key(sid);
    }

    // Run the agentic loop.
    let result = agent
        .execute_with_session(
            &message,
            session,
            Some(history),
            |_event| {
                // Non-streaming: events are ignored.
            },
        )
        .await
        .context("supervisor agent execution failed")?;

    Ok(result.final_answer)
}
