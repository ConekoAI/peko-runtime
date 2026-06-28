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
///
/// `provider_hint` is the resolved `(preferred_provider_id, preferred_model_id)`
/// pair. The caller passes the explicit principal-config values when set, or
/// falls back to the catalog's `default_provider_id` / `default_model_id` when
/// the principal doesn't declare one (see [`run_supervisor_prompt`]). Without
/// a non-`None` provider hint the supervisor's `SubagentExecutor` raises the
/// actionable "no LLM provider is configured for principal '{name}'" error
/// pointing the user at the principal + global config paths — there is no
/// other code path that can recover a provider for the supervisor at run
/// time.
pub fn build_agent_config(
    prompt: &AgentPrompt,
    capabilities: &PrincipalCapabilities,
    provider_hint: (Option<String>, Option<String>),
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

    let (preferred_provider_id, preferred_model_id) = provider_hint;

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
        preferred_provider_id,
        preferred_model_id,
        // Inherit sensible defaults for the rest.
        ..AgentConfig::default()
    }
}

/// Merge two `(provider_id, model_id)` hints with the principal-level
/// hint taking precedence on each axis. The principal may pin only a
/// provider (leaving the model to the catalog default) or only a model
/// (rare but supported for routing to a non-default model on the
/// catalog's default provider).
fn merge_provider_hint(
    principal: (Option<String>, Option<String>),
    catalog_default: (Option<String>, Option<String>),
) -> (Option<String>, Option<String>) {
    let pid = principal.0.or(catalog_default.0);
    let mid = principal.1.or(catalog_default.1);
    (pid, mid)
}

/// Validate a principal's provider hint against the live catalog.
///
/// If the principal pins a `preferred_provider_id` that doesn't exist
/// in the catalog — typical after `peko provider remove` or a
/// hand-edit typo — drop the principal's hint entirely so the catalog
/// default applies. A stale pin should never break the supervisor;
/// the operator will see the warning and either re-add the provider
/// or fix the principal config.
///
/// Returns the principal hint unchanged when no validation is
/// possible (no resolver) or the hint is valid.
async fn validate_principal_hint(
    resolver: &LlmResolver,
    principal_hint: (Option<String>, Option<String>),
) -> (Option<String>, Option<String>) {
    let Some(ref pid) = principal_hint.0 else {
        return principal_hint;
    };
    if resolver.catalog().get(pid).await.is_some() {
        return principal_hint;
    }
    tracing::warn!(
        "principal prefers provider '{pid}' but it is not in the catalog. \
         Falling back to the catalog default. \
         Re-add it with `peko provider add --template {pid}` or clear the \
         principal's `preferred_provider_id` in principal.toml."
    );
    (None, None)
}

/// Run the supervisor agent prompt in a peer-scoped session using a dedicated
/// `ExtensionCore`.
///
/// The supervisor core is isolated from the global core: it carries the
/// principal's own agents as `{{agents}}` hooks, an `Agent` tool that resolves
/// those agents, and principal-scoped session/memory/catalog tools.
///
/// `principal_provider_hint` is the `(preferred_provider_id, preferred_model_id)`
/// pair from the principal's own `principal.toml`. It wins over the global
/// catalog default so a Principal can pin itself to a specific provider
/// without affecting siblings. When both elements are `None`, the catalog
/// default applies.
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
    principal_provider_hint: (Option<String>, Option<String>),
) -> anyhow::Result<String> {
    // Provider-hint precedence:
    //   1. Per-principal `[provider]` from `principal.toml` (wins) —
    //      but only when the referenced provider actually exists in
    //      the catalog. A stale or mistyped id (e.g. user deleted the
    //      provider) gracefully falls back to the catalog default
    //      rather than failing the supervisor.
    //   2. Global catalog default (`peko provider set-default`).
    //   3. None — the SubagentExecutor surfaces the actionable "no
    //      provider configured" error (issue #69).
    let catalog_default = match resolver.as_ref() {
        Some(r) => r.catalog().get_default().await,
        None => (None, None),
    };
    let validated_principal_hint = match resolver.as_ref() {
        Some(r) => validate_principal_hint(r, principal_provider_hint).await,
        None => principal_provider_hint,
    };
    let provider_hint = merge_provider_hint(validated_principal_hint, catalog_default);
    let mut config = build_agent_config(prompt, capabilities, provider_hint);

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
    .await?
    // Scope the supervisor's `Agent` tool to this principal's workspace so
    // subagents resolve from `<workspace>/agents/<name>/AGENT.md`. Without this,
    // `Agent::init_builtins_async` (run lazily at execution time, inside
    // `prepare_execution`) re-registers a globally-scoped `Agent` tool that
    // clobbers the principal-scoped one registered below — making every
    // `subagent_type` resolve against the global `<home>/agents/...` path and
    // fail with "Subagent type '<name>' not found".
    .with_principal_workspace(workspace_path.clone());

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
            // The principal workspace is `{config_dir}/principals/{name}` (see
            // `PathResolver::principal_dir`), so derive the two config files
            // we can plausibly ask the user to edit without threading the
            // PathResolver through every layer.
            let principal_toml = workspace_path.join("principal.toml");
            let global_toml = workspace_path
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("config.toml"));
            let global_hint = global_toml
                .as_ref()
                .map(|p| format!("\n  • {}", p.display()))
                .unwrap_or_default();
            anyhow::anyhow!(
                "no LLM provider is configured for principal '{name}'.\n\
                 \n\
                 Add a [provider] block to one of:\n\
                   • {principal}{global_hint}\n\
                 \n\
                 Example:\n\
                   [provider]\n\
                   type = \"ollama\"\n\
                   model = \"llama3\"\n\
                 \n\
                 Or run: peko provider add",
                name = prompt.name,
                principal = principal_toml.display(),
                global_hint = global_hint,
            )
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

#[cfg(test)]
mod tests {
    use super::{merge_provider_hint, validate_principal_hint};
    use crate::common::secret_store::InMemorySecretStore;
    use crate::providers::catalog::{ModelInfo, ProviderCatalogEntry};
    use crate::providers::templates;
    use crate::providers::LlmResolver;
    use std::sync::Arc;

    /// Per-principal hint wins outright when both axes are set: this is
    /// the headline behaviour for "principals automatically use default
    /// provider unless configured otherwise".
    #[test]
    fn principal_hint_wins_over_catalog_default_when_both_set() {
        let merged = merge_provider_hint(
            (Some("ollama".into()), Some("llama3.1".into())),
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into())),
        );
        assert_eq!(merged, (Some("ollama".into()), Some("llama3.1".into())));
    }

    /// Principal pins the provider but leaves the model — the catalog
    /// default's model should still apply for that axis.
    #[test]
    fn principal_provider_only_falls_back_to_catalog_model() {
        let merged = merge_provider_hint(
            (Some("ollama".into()), None),
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into())),
        );
        assert_eq!(
            merged,
            (Some("ollama".into()), Some("claude-sonnet-4-5".into()))
        );
    }

    /// No principal hint → catalog default flows through verbatim.
    /// This is the path 99% of Principals take.
    #[test]
    fn no_principal_hint_inherits_catalog_default() {
        let merged = merge_provider_hint(
            (None, None),
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into())),
        );
        assert_eq!(
            merged,
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into()))
        );
    }

    /// Neither side has a hint → both axes stay None, and the
    /// SubagentExecutor raises the actionable "no provider configured"
    /// error pointing at the config paths.
    #[test]
    fn no_hint_anywhere_yields_none() {
        assert_eq!(merge_provider_hint((None, None), (None, None)), (None, None));
    }

    async fn seeded_resolver_with(
        providers: &[(&str, &str)],
        default: Option<(&str, &str)>,
    ) -> Arc<LlmResolver> {
        let dir = tempfile::tempdir().unwrap();
        let cat = crate::providers::catalog::ProviderCatalog::load_or_init(
            dir.path().join("providers.toml"),
        )
        .await
        .unwrap();
        for (id, model) in providers {
            let tmpl = templates::find_template(id).unwrap_or_else(|| {
                templates::find_template("ollama").unwrap() // unreachable in tests
            });
            let entry = ProviderCatalogEntry {
                id: (*id).to_string(),
                display_name: tmpl.display_name.to_string(),
                template_id: Some(tmpl.id.to_string()),
                api_format: tmpl.api_format,
                base_url: tmpl.base_url.to_string(),
                models: vec![ModelInfo::new((*model).to_string())],
                default_model_id: (*model).to_string(),
                headers: Default::default(),
                requires_key: tmpl.requires_key,
                enabled: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            cat.upsert(entry).await.unwrap();
        }
        if let Some((pid, mid)) = default {
            cat.set_default(Some(pid.into()), Some(mid.into())).await.unwrap();
        }
        let secrets = Arc::new(InMemorySecretStore::new());
        Arc::new(LlmResolver::new(cat, secrets))
    }

    /// Principal pins a provider that exists in the catalog → hint
    /// passes through untouched (graceful path stays the happy path).
    #[tokio::test]
    async fn validate_principal_hint_passes_through_known_provider() {
        let resolver = seeded_resolver_with(&[("anthropic", "claude-sonnet-4-5")], None).await;
        let validated =
            validate_principal_hint(&resolver, (Some("anthropic".into()), Some("claude-sonnet-4-5".into()))).await;
        assert_eq!(
            validated,
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into()))
        );
    }

    /// Principal pins a provider that's been deleted from the catalog
    /// (or was a typo) → drop the principal hint so the catalog
    /// default flows through, and the supervisor keeps working.
    #[tokio::test]
    async fn validate_principal_hint_drops_unknown_provider() {
        let resolver =
            seeded_resolver_with(&[("anthropic", "claude-sonnet-4-5")], Some(("anthropic", "claude-sonnet-4-5"))).await;
        let validated =
            validate_principal_hint(&resolver, (Some("ghost-provider".into()), Some("any-model".into()))).await;
        // Both axes dropped → `merge_provider_hint` then falls back to
        // the catalog default verbatim.
        assert_eq!(validated, (None, None));
    }

    /// No resolver context → we can't validate, so the principal hint
    /// passes through unchanged. This matches the legacy behaviour and
    /// keeps the test-friendly `run_supervisor_prompt` callers honest.
    #[test]
    fn validate_principal_hint_is_noop_when_no_hint() {
        // A pure-function spot-check: with no principal hint set, the
        // supervisor never invokes `validate_principal_hint` with a
        // Some(pid), but we still guarantee the helper is a no-op for
        // the (None, _) case via the call-site guard.
        assert_eq!(
            merge_provider_hint((None, None), (Some("anthropic".into()), Some("claude-sonnet-4-5".into()))),
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into()))
        );
    }
}
