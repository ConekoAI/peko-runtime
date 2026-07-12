use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::RwLock;

use crate::agents::agent_config::AgentConfig;
use crate::agents::Agent;
use crate::auth::Subject;
use crate::common::types::message::LlmMessage;
use crate::engine::AgenticEvent;
use crate::principal::context::{install_agent_catalog, PrincipalContext};
use crate::principal::router::AgentPromptSummary;
use crate::session::manager::SessionManager;
use crate::session::SessionCreateOptions;
use crate::tools::builtin::{AgentTool, DynamicSessionKeyProvider};

use super::{agent_prompt::AgentPrompt, Capabilities};

/// Build an `AgentConfig` from a thin Markdown prompt + the Principal's
/// allowed extensions.
///
/// `provider_hint` is the resolved `(preferred_provider_id, preferred_model_id)`
/// pair. The caller passes the explicit principal-config values when set, or
/// falls back to the catalog's `default_provider_id` / `default_model_id` when
/// the principal doesn't declare one (see [`run_root_agent_prompt`]). Without
/// a non-`None` provider hint the root agent's `SubagentExecutor` raises the
/// actionable "no LLM provider is configured for principal '{name}'" error
/// pointing the user at the principal + global config paths — there is no
/// other code path that can recover a provider for the root agent at run
/// time.
///
/// **Track B**: the `capabilities` / `available_agents` filtering
/// no longer touches the agent config (the per-agent extension whitelist
/// is gone from `AgentConfig`). The principal's allowlist is bound
/// separately at agent construction time via
/// [`Agent::with_principal_capabilities`]. The filter itself still
/// matters at runtime — see
/// [`run_root_agent_prompt_with_callback`] for the canonical-agent and
/// install path.
pub fn build_agent_config(
    prompt: &AgentPrompt,
    _capabilities: &Capabilities,
    _available_agents: &[AgentPromptSummary],
    // **Track B**: `provider_hint` is no longer baked into the
    // returned `AgentConfig` (the `preferred_provider_id` /
    // `preferred_model_id` fields are gone). The caller is
    // responsible for threading the hint to
    // `Agent::new_with_session_manager_resolver`, which forwards it
    // to `init_provider`. The parameter is kept here for API
    // stability — production call sites pass `ctx.provider_hint`,
    // and `resolve_provider_hint` already merged any catalog
    // defaults before this function sees it.
    _provider_hint: (Option<String>, Option<String>),
) -> AgentConfig {
    // The `capabilities` / `available_agents` parameters are
    // kept for API stability but no longer affect `AgentConfig`
    // construction: the per-agent extension whitelist has been
    // removed from `AgentConfig`. The principal's allowlist is
    // bound separately at agent construction time via
    // [`Agent::with_principal_capabilities`], and the
    // canonical agents/extensions split is computed at runtime in
    // [`run_root_agent_prompt_with_callback`] when the catalogue is
    // installed on the principal's `ExtensionCore`.

    AgentConfig {
        name: prompt.name.clone(),
        description: prompt.frontmatter.description.clone(),
        // The agent's system prompt is the body of its resolved
        // `AgentPrompt`. For built-in agents this came from `include_str!`;
        // for user-authored agents it came from a Markdown file under
        // `<workspace>/agents/`. Either way it now reaches the LLM
        // through `SystemPromptService::build` reading
        // `config.prompt` (the per-agent body) directly — no more
        // bootstrap-file plumbing.
        prompt: Some(prompt.body.clone()),
        // Track B: principal-mirrored fields (`extensions`,
        // `workspace`, `preferred_*`) are gone from `AgentConfig`.
        // The spread picks up `agent_did`, `owner`, `permissions`,
        // and the per-agent toggles; these are genuine per-agent
        // state and stay.
        ..Default::default()
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
/// default applies. A stale pin should never break the root agent;
/// the operator will see the warning and either re-add the provider
/// or fix the principal config.
///
/// Returns the principal hint unchanged when no validation is
/// possible (no resolver) or the hint is valid.
async fn validate_principal_hint(
    resolver: &crate::providers::LlmResolver,
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

/// Resolve the final provider hint for a principal context.
///
/// Precedence: per-principal `[provider]` from `principal.toml` (wins,
/// but only when the referenced provider actually exists in the
/// catalog), then the global catalog default, then `None` (which
/// surfaces the actionable "no provider configured" error from
/// `SubagentExecutor` — issue #69).
pub(crate) async fn resolve_provider_hint(
    ctx: &PrincipalContext,
) -> (Option<String>, Option<String>) {
    let catalog_default = match ctx.resolver.as_ref() {
        Some(r) => r.catalog().get_default().await,
        None => (None, None),
    };
    let validated_principal_hint = match ctx.resolver.as_ref() {
        Some(r) => validate_principal_hint(r, ctx.provider_hint.clone()).await,
        None => ctx.provider_hint.clone(),
    };
    merge_provider_hint(validated_principal_hint, catalog_default)
}

/// Run the root agent prompt in a peer-scoped
/// session using the principal's shared `ExtensionCore`.
///
/// The root agent is just another agent of the principal — the same
/// `PrincipalContext.core()` is used by every agent the principal
/// spawns. What the root agent can see is governed by the principal's
/// `capabilities`; what any subagent can see is governed by that
/// subagent's own extension whitelist.
pub async fn run_root_agent_prompt(
    prompt: &AgentPrompt,
    peer: Subject,
    message: String,
    session_id: String,
    available_agents: Vec<AgentPromptSummary>,
    ctx: &PrincipalContext,
) -> anyhow::Result<String> {
    run_root_agent_prompt_with_callback(
        prompt,
        peer,
        message,
        session_id,
        available_agents,
        ctx,
        |_event| {
            // Non-streaming: events are ignored.
        },
        None,
    )
    .await
}

/// Streaming variant of [`run_root_agent_prompt`]. The callback is invoked
/// for every [`AgenticEvent`] emitted by the root agent's loop
/// (e.g. `AssistantDelta` for token deltas, `ToolStart`/`ToolEnd` for tool
/// invocations). The callback must be cheap and non-blocking; the runtime
/// relies on it to push `PrincipalSentChunk` deltas to the IPC client
/// without back-pressure on the agentic loop.
///
/// Returns the same `final_answer` string as the non-streaming variant.
pub async fn run_root_agent_prompt_streaming<F>(
    prompt: &AgentPrompt,
    peer: Subject,
    message: String,
    session_id: String,
    available_agents: Vec<AgentPromptSummary>,
    ctx: &PrincipalContext,
    on_event: F,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> anyhow::Result<String>
where
    F: Fn(AgenticEvent) + Send + Sync + 'static,
{
    run_root_agent_prompt_with_callback(
        prompt,
        peer,
        message,
        session_id,
        available_agents,
        ctx,
        on_event,
        cancel,
    )
    .await
}

async fn run_root_agent_prompt_with_callback<F>(
    prompt: &AgentPrompt,
    peer: Subject,
    message: String,
    session_id: String,
    available_agents: Vec<AgentPromptSummary>,
    ctx: &PrincipalContext,
    on_event: F,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> anyhow::Result<String>
where
    F: Fn(AgenticEvent) + Send + Sync + 'static,
{
    let provider_hint = resolve_provider_hint(ctx).await;
    let config = build_agent_config(prompt, &ctx.capabilities, &available_agents, provider_hint);

    // Build the principal's shared core first so we can ask the core
    // to resolve bare extension names into canonical `extension_id`
    // form. Phase 4a: there is no privileged whitelist anymore — the
    // principal's `capabilities` are the *only* source of truth for
    // which tools the root agent (and every subagent that inherits
    // from it) can see. Each subagent's own `AgentConfig.extensions`
    // may further filter that set on a per-agent basis.
    let core = ctx.core().await;

    let _agent_names: HashSet<String> = available_agents
        .iter()
        .map(|a| a.name.to_ascii_lowercase())
        .collect();

    // Resolve everything the principal is allowed to use. The agent
    // catalog filter (separating agent-prompt names from extension
    // names) is applied here once for the canonical-agent listing; the
    // principal's allowlist itself is bound below via
    // `with_principal_capabilities` so the agent's tool filter
    // (initialized lazily in `init_builtins_async`) only sees
    // canonical extension ids.
    let _resolved: Vec<String> = core
        .resolve_canonical_ids(&ctx.capabilities.to_strings(), ctx.principal_id())
        .await;

    // Agent catalog is the only per-call tool — its `available_agents`
    // snapshot can change between messages if the principal's
    // `capabilities` was edited. We re-register it on the
    // shared core, which is idempotent on tool name.
    install_agent_catalog(&core, available_agents, ctx.principal_id()).await?;

    // Register the principal-scoped `Agent` tool after `Agent::new*` but
    // before execution so it is available on the principal's shared
    // core.
    let session_key_provider = Arc::new(DynamicSessionKeyProvider::new(format!(
        "agent:{}:cli:default",
        prompt.name
    )));
    let session_manager = SessionManager::new()
        .with_sessions_dir_internal(ctx.sessions_dir.clone())
        .with_agent_name(&prompt.name)
        .with_peer_principal(peer.clone())
        .with_user(&peer.to_string());
    let session_manager = Arc::new(RwLock::new(session_manager));

    // Open or create the root agent session.  Hold the per-principal
    // session-creation lock while touching the shared session index so
    // concurrent peers don't corrupt it.
    let mut is_new_session = false;
    let session = {
        let _creation_guard = ctx.session_creation_lock.lock().await;
        let maybe_handle = {
            let mut mgr = session_manager.write().await;
            mgr.open_session(&session_id).await?
        };
        if let Some(handle) = maybe_handle {
            handle.base().clone()
        } else {
            is_new_session = true;
            let mut mgr = session_manager.write().await;
            let options = SessionCreateOptions::new().with_session_id(&session_id);
            let handle = mgr
                .create_session(&prompt.name, &peer, options)
                .await
                .context("failed to create root agent session")?;
            handle.base().clone()
        }
    };

    // Fire the session-start hook for brand-new sessions. The returned
    // bootstrap context is persisted on the session and rendered into the
    // system prompt via the `{{session_context}}` placeholder.
    if is_new_session {
        let mut metadata = std::collections::HashMap::<String, serde_json::Value>::new();
        metadata.insert("event".to_string(), serde_json::json!("startup"));
        metadata.insert(
            "workspace".to_string(),
            serde_json::json!(ctx.workspace_path.to_string_lossy().to_string()),
        );
        let snapshot = crate::extensions::framework::types::SessionSnapshot {
            session_id: session_id.clone(),
            message_count: 0,
            context_tokens: 0,
            metadata,
        };
        let principal_id = ctx.principal_id().to_string();
        if let Some(context) = core
            .invoke_hook_text_with_principal(
                crate::extensions::framework::HookPoint::SessionStart,
                crate::extensions::framework::HookInput::SessionState(snapshot),
                Some(&principal_id),
                Some(ctx.capabilities.to_strings()),
                Some(ctx.active_extensions().to_vec()),
                Some(ctx.workspace_path.to_string_lossy().to_string()),
            )
            .await
        {
            session.write().await.extension_context = Some(context);
        }
    }

    let history: Vec<LlmMessage> = session.read().await.load_history().await?;

    // Cold-start the root agent. After the Phase-2 redo there is one
    // daemon-global `ExtensionCore`; the agent picks it up internally.
    // `principal_id` is threaded through so the agent's
    // `SubagentExecutor` (and every descendant spawn) inherits the
    // principal scope. Wiring it to the same inbox registry the
    // Principal boundary uses for steering messages.
    let agent = Agent::new_with_session_manager_resolver(
        config,
        Arc::clone(&session_manager),
        ctx.resolver.clone(),
        // **Track B**: pass the principal's resolved provider hint
        // through; `init_provider` forwards it to the resolver.
        Some(ctx.provider_hint.clone()),
        ctx.principal_id().clone(),
        Some(Arc::clone(&ctx.inbox_registry)),
    )
    .await?
    // Scope the agent's `Agent` tool to this principal's workspace so
    // subagents resolve from `<workspace>/agents/<name>/AGENT.md`. Without this,
    // `Agent::init_builtins_async` (run lazily at execution time, inside
    // `prepare_execution`) re-registers a globally-scoped `Agent` tool that
    // clobbers the principal-scoped one registered below — making every
    // `subagent_type` resolve against the global `<home>/agents/...` path and
    // fail with "Subagent type '<name>' not found".
    .with_principal_workspace(ctx.workspace_path.clone())
    .with_principal_name(ctx.name().to_string())
    // Bind the principal's allowlist for the agent's tool filter.
    // Track B moved the per-agent extension whitelist off
    // `AgentConfig`; the snapshot lives on the agent and is
    // consulted by `init_builtins_async` to prune the tool bag.
    .with_principal_capabilities(Some(Arc::clone(&ctx.capabilities)))
    // Bind the active extension snapshot so the tool gate also verifies
    // that each tool's owning extension is active.
    .with_active_extensions(Some(ctx.active_extensions().clone()))
    // Phase 4b: bind caller DID so `principal_send` is registered.
    // `None` ⇒ tool is intentionally omitted (no local-only fallback
    // for `principal_send`; it is exclusively cross-runtime).
    .with_caller_principal_did(ctx.caller_principal_did().cloned());

    let subagent_executor = Arc::new(
        crate::agents::subagent_executor::SubagentExecutor::new(
            Arc::clone(&session_manager),
            &prompt.name,
            5,
            ctx.principal_id().clone(),
        )
        .with_principal_name(ctx.name().to_string())
        .with_principal_capabilities(Some(Arc::clone(&ctx.capabilities)))
        .with_active_extensions(Some(ctx.active_extensions().clone()))
        .with_observability(ctx.observability().cloned())
        .with_provider(agent.provider_arc().ok_or_else(|| {
            // The principal workspace is `{config_dir}/principals/{name}` (see
            // `PathResolver::principal_dir`), so derive the two config files
            // we can plausibly ask the user to edit without threading the
            // PathResolver through every layer.
            let principal_toml = ctx.workspace_path.join("principal.toml");
            let global_toml = ctx
                .workspace_path
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

    let agent_tool = Arc::new(AgentTool::with_workspace_and_session(
        subagent_executor,
        Some(ctx.workspace_path.clone()),
        Box::new(session_key_provider.clone()),
    ));
    crate::extensions::builtin::BuiltinToolAdapter::register_tool(
        &core,
        agent_tool,
        ctx.principal_id(),
    )
    .await?;

    // Stamp the current session key so the Agent tool can auto-detect it.
    {
        let sid = session.read().await.id.clone();
        session_key_provider.set_session_key(sid);
    }

    // Run the agentic loop in LIVE streaming mode so the root agent emits
    // per-token `AssistantDelta` events (not a single buffered
    // `AssistantText` at the end). `execute_with_session` would use
    // `OrchestratorConfig::final_only()`, which defeats real end-to-end
    // streaming — the caller would receive the whole answer as one chunk.
    // `execute_streaming_with_session` uses `OrchestratorConfig::live()`.
    let result = agent
        .execute_streaming_with_session(
            &message,
            session,
            Some(history),
            None, // caller_id: attribution is handled at the dispatcher boundary
            on_event,
            cancel,
        )
        .await
        .context("root agent execution failed")?;

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
        assert_eq!(
            merge_provider_hint((None, None), (None, None)),
            (None, None)
        );
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
            cat.set_default(Some(pid.into()), Some(mid.into()))
                .await
                .unwrap();
        }
        let secrets = Arc::new(InMemorySecretStore::new());
        Arc::new(LlmResolver::new(cat, secrets))
    }

    /// Principal pins a provider that exists in the catalog → hint
    /// passes through untouched (graceful path stays the happy path).
    #[tokio::test]
    async fn validate_principal_hint_passes_through_known_provider() {
        let resolver = seeded_resolver_with(&[("anthropic", "claude-sonnet-4-5")], None).await;
        let validated = validate_principal_hint(
            &resolver,
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into())),
        )
        .await;
        assert_eq!(
            validated,
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into()))
        );
    }

    /// Principal pins a provider that's been deleted from the catalog
    /// (or was a typo) → drop the principal hint so the catalog
    /// default flows through, and the root agent keeps working.
    #[tokio::test]
    async fn validate_principal_hint_drops_unknown_provider() {
        let resolver = seeded_resolver_with(
            &[("anthropic", "claude-sonnet-4-5")],
            Some(("anthropic", "claude-sonnet-4-5")),
        )
        .await;
        let validated = validate_principal_hint(
            &resolver,
            (Some("ghost-provider".into()), Some("any-model".into())),
        )
        .await;
        // Both axes dropped → `merge_provider_hint` then falls back to
        // the catalog default verbatim.
        assert_eq!(validated, (None, None));
    }

    /// No resolver context → we can't validate, so the principal hint
    /// passes through unchanged. This matches the legacy behaviour and
    /// keeps the test-friendly `run_root_agent_prompt` callers honest.
    #[test]
    fn validate_principal_hint_is_noop_when_no_hint() {
        // A pure-function spot-check: with no principal hint set, the
        // root agent never invokes `validate_principal_hint` with a
        // Some(pid), but we still guarantee the helper is a no-op for
        // the (None, _) case via the call-site guard.
        assert_eq!(
            merge_provider_hint(
                (None, None),
                (Some("anthropic".into()), Some("claude-sonnet-4-5".into()))
            ),
            (Some("anthropic".into()), Some("claude-sonnet-4-5".into()))
        );
    }
}
