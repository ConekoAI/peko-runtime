use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::RwLock;

use crate::agents::agent_config::AgentConfig;
use crate::agents::Agent;
use crate::principal::context::{install_agent_catalog, PrincipalContext};
use crate::principal::router::AgentPromptSummary;
use crate::tools::builtin::DynamicSessionKeyProvider;
use peko_auth::Subject;
use peko_engine::AgenticEvent;
use peko_message::LlmMessage;
use peko_session::manager::{SessionManager, SessionManagerRotationSink};
use peko_session::SessionCreateOptions;

use crate::principal::agent_prompt::AgentPrompt;
use peko_extension_api::Capabilities;

/// Build an `AgentConfig` from a thin Markdown prompt + the Principal's
/// allowed extensions.
///
/// `provider_hint` is the resolved configured-model id (the principal's
/// `preferred_model_id`, or a per-message `--model` override). Without a
/// non-`None` hint the root agent's `SubagentExecutor` raises the
/// actionable "no model configured for principal '{name}'" error
/// pointing the user at the principal.toml pin — there is no runtime
/// default model and no other code path that can recover a provider
/// for the root agent at run time.
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
    // Model-first: the principal's pinned configured model id, or
    // `None`. The caller threads this to
    // `Agent::new_with_session_manager_resolver`, which forwards it to
    // `init_provider`.
    _provider_hint: Option<String>,
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

/// Validate a principal's configured model hint against the live catalog.
///
/// If the principal pins a `preferred_model_id` that doesn't exist in
/// the catalog — typical after `peko model remove` or a hand-edit typo
/// — drop the hint and log a warning. A stale pin should never break
/// the root agent; the operator will see the warning and either
/// re-add the model or fix the principal config.
async fn validate_principal_hint(
    resolver: &peko_providers::LlmResolver,
    principal_hint: Option<String>,
) -> Option<String> {
    let Some(ref id) = principal_hint else {
        return principal_hint;
    };
    if resolver.catalog().get(id).await.is_some() {
        return principal_hint;
    }
    tracing::warn!(
        "principal prefers model '{id}' but it is not in the catalog. \
         Re-add it with `peko model add ...` or clear the principal's \
         `preferred_model_id` in principal.toml."
    );
    None
}

/// Resolve the final configured model hint for a principal context.
///
/// Returns the principal's pinned model id when it exists in the
/// catalog; otherwise returns `None` (which surfaces the actionable
/// "no model configured" error from `LlmResolver`).
pub(crate) async fn resolve_provider_hint(ctx: &PrincipalContext) -> Option<String> {
    match ctx.resolver.as_ref() {
        Some(r) => validate_principal_hint(r, ctx.provider_hint.clone()).await,
        None => ctx.provider_hint.clone(),
    }
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
    user_text: String,
    pre_user_messages: Vec<LlmMessage>,
    session_id: String,
    available_agents: Vec<AgentPromptSummary>,
    ctx: &PrincipalContext,
) -> anyhow::Result<String> {
    run_root_agent_prompt_with_callback(
        prompt,
        peer,
        user_text,
        pre_user_messages,
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
    user_text: String,
    pre_user_messages: Vec<LlmMessage>,
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
        user_text,
        pre_user_messages,
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
    user_text: String,
    pre_user_messages: Vec<LlmMessage>,
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

    // WS2 (implicit session management): startup sweep pages any
    // over-threshold live-id JSONL the daemon inherited (e.g. user
    // re-opens a principal after a deploy that catches a session
    // mid-life above the auto-paging threshold). Idempotent: after
    // paging, `<id>.jsonl` no longer exists, so subsequent boots find
    // nothing to page. Best-effort — log and continue on failure
    // rather than blocking the boot path.
    {
        let mut mgr = session_manager.write().await;
        match mgr.rotate_oversized_sessions().await {
            Ok(n) if n > 0 => tracing::info!("WS2 startup sweep: rotated {n} oversize session(s)"),
            Ok(_) => {}
            Err(e) => tracing::warn!("WS2 startup sweep failed: {e}"),
        }
    }

    // Phase 2 (standing named children): ensure every `[children]`
    // declaration in `principal.toml` exists as a standing session
    // under the owner root. Best-effort: unreadable/corrupt config or
    // storage failures warn and continue (the `seen_models.json`
    // tolerated-corruption precedent) — a broken declaration must
    // never block the root agent.
    if let Err(e) = crate::principal::children::ensure_declared_children(
        &ctx.workspace_path,
        &prompt.name,
        &session_manager,
    )
    .await
    {
        tracing::warn!("failed to ensure declared standing children: {e}");
    }

    // Open or create the root agent session.  Hold the per-principal
    // session-creation lock while touching the shared session index so
    // concurrent peers don't corrupt it.
    let session = {
        let _creation_guard = ctx.session_creation_lock.lock().await;

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
                .context("failed to create root agent session")?;
            handle.base().clone()
        }
    };
    // WS2 (implicit session management): install the rotation sink so
    // every `add_*` on this session auto-pages when its JSONL crosses
    // `test_config::rotate_bytes()` (10 MiB default). Paging renames
    // `<S>.jsonl` → `<S>.<n>.jsonl` in place — the session id, index
    // entry, and peers routing stay untouched; only the context cache
    // for `S` is invalidated. Safe to skip on cold paths (tests,
    // recovery).
    {
        let mut session_guard = session.write().await;
        session_guard.set_rotation_sink(Arc::new(SessionManagerRotationSink::new(
            Arc::clone(&session_manager),
            prompt.name.clone(),
            peer.clone(),
        )));
    }
    // SessionStart hook was removed (per-turn rebuild refactor):
    // the bootstrap context is now produced by `SessionContextBuild`
    // hooks fired by `PromptRenderer::render_for_iteration` on every
    // iteration, so a one-shot fire here would be redundant and stale.

    // SessionStart hook was removed (per-turn rebuild refactor):
    // the bootstrap context is now produced by `SessionContextBuild`
    // hooks fired by `PromptRenderer::render_for_iteration` on every
    // iteration, so a one-shot fire here would be redundant and stale.

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
        // Model-first: pass the principal's pinned configured model id
        // through; `init_provider` forwards it to the resolver.
        ctx.provider_hint.clone(),
        ctx.principal_id().clone(),
        Some(Arc::clone(&ctx.inbox_registry)),
        // Per-message configured model override (mirrored from
        // `RouterContext`). `init_provider` populates
        // `ResolveRequest::override_model` so the resolver classifies
        // the resolution as `ExplicitOverride` when set.
        ctx.message_override.clone(),
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
    // PR #2 wiring: bind the principal's plan DAG port so the seven
    // `Plan*` built-in tools are registered by `init_builtins_async`.
    // `ctx.plan_port()` returns the per-Principal handle from
    // `PrincipalContext::plan_port` (set by the factory in PR #1).
    .with_principal_plan_port(Arc::clone(ctx.plan_port()))
    // Phase 2 of `feature/multi-model-subagents`: bind the
    // principal's `ModelCatalog` (sourced from the same `LlmResolver`
    // the agent already uses for model resolution) so
    // `init_builtins_async` can register the `model_list` builtin.
    // `ctx.resolver` is `Option` because the CLI one-shot path
    // builds a stateless `Agent` without a resolver; in that case
    // the `model_list` tool is intentionally omitted (`None` ⇒
    // `init_builtins_async` skips registration).
    .with_model_catalog(
        ctx.resolver.as_ref().map(|r| Arc::clone(r.catalog())),
    )
    // ADR-045 (self-modification gate): bind caller DID so the
    // `send_peer` tool is registered. `None` ⇒ tool is intentionally
    // omitted (no caller identity to attribute sends to).
    .with_caller_principal_did(ctx.caller_principal_did().cloned());

    // Phase 4 of `feature/multi-model-subagents`: bind the audit
    // sink + first-use lookup so the engine loop emits a
    // `model.selected` event on every successful LLM call. The
    // sink wraps the principal's `Observability` hub; the lookup
    // closure projects `PrincipalContext::seen_models` (a
    // `BTreeSet<String>` keyed by model id, wrapped in
    // `Arc<Mutex<_>>`) so the engine stays decoupled from
    // `peko-principal`. The closure captures a cloned `Arc` so it
    // outlives the borrowed `&PrincipalContext`; any updates made
    // by `PrincipalContext::mark_model_seen` after the agent is
    // constructed are visible because they mutate the same shared
    // set. When either half is missing (CLI one-shot path without
    // an Observability hub; principal with no `seen_models.json`
    // yet), the corresponding side is passed as `None` and the
    // loop falls back to `Info` severity.
    //
    // Applied as a separate statement (not chained) because the
    // closure needs to capture a value computed from `ctx` that
    // outlives the `&PrincipalContext` borrow.
    let agent = {
        let seen_set = ctx.seen_models_handle();
        let audit_sink: Option<Arc<dyn peko_engine::audit_sink::AuditSink>> =
            ctx.observability().map(|obs| {
                Arc::new(crate::observability::ObservabilityAuditSink::new(
                    Arc::clone(obs),
                )) as Arc<dyn peko_engine::audit_sink::AuditSink>
            });
        let first_use_lookup: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>> = {
            let set = Arc::clone(&seen_set);
            Some(Arc::new(move |model_id: &str| {
                !set
                    .lock()
                    .expect("seen_models mutex poisoned")
                    .contains(model_id)
            }) as Arc<dyn Fn(&str) -> bool + Send + Sync>)
        };
        agent.with_audit_sink(audit_sink, first_use_lookup)
    };

    // F19: quota meter no longer threaded through Agent. The
    // engine loop fetches the principal's meter directly via
    // `Principal.quota_meter` at run entrypoint and opens
    // `QuotaScope::with` around the run.

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
        // PR #2 wiring: same plan_port as the agent, so depth-1+
        // children inherit the per-Principal handle and register their
        // own seven `Plan*` tools.
        .with_principal_plan_port(Arc::clone(ctx.plan_port()))
        .with_observability(ctx.observability().cloned())
        // F39: chain the principal's quota meters so subagent LLM
        // calls charge against the parent principal instead of
        // falling open to `QuotaMeter::unlimited()`. Pre-F39
        // wiring left these `None`, so subagent LLM traffic was
        // unattributed. `ctx.quota_meter()` / `ctx.peer_meter()`
        // return `Option` — principals with no quota config keep
        // the old behavior.
        .with_quota_meter(ctx.quota_meter().map(Arc::clone))
        .with_peer_meter(ctx.peer_meter().map(Arc::clone))
        .with_provider(agent.provider_arc().ok_or_else(|| {
            // The principal workspace is `{config_dir}/principals/{name}` (see
            // `PathResolver::principal_dir`), so the principal.toml path is
            // derivable without threading the PathResolver through every
            // layer.
            let principal_toml = ctx.workspace_path.join("principal.toml");
            anyhow::anyhow!(
                "no model configured for principal '{name}'.\n\
                 \n\
                 Pin a configured model in {principal}:\n\
                   preferred_model_id = \"<configured-model-id>\"\n\
                 \n\
                 Add one with: peko model add --template <anthropic|openai|ollama|...> --model <wire-id>\n\
                 List configured models: peko model list",
                name = prompt.name,
                principal = principal_toml.display(),
            )
        })?)
        .with_agent_config(agent.config.clone()),
    );

    let agent_tool = Arc::new(
        crate::tools::builtin::messaging::agent_tool_with_workspace_and_session(
            subagent_executor,
            Some(ctx.workspace_path.clone()),
            Box::new(session_key_provider.clone()),
        ),
    );
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
    //
    // Bug A (2026-08-01 v2): read the meters off `ctx` (bound by
    // `RootRouter::build_context` from the dispatcher-populated
    // `RouterContext`). When unbound, fall through to `None` (which
    // `QuotaMeter::unlimited()` semantics) — the CLI one-shot
    // `stateless_service.rs:667` path stays `None` because it doesn't
    // build a `PrincipalContext` at all. F20 peer metering remains
    // `None` until the dispatcher populates `RouterContext::peer_meter`.
    let quota_meter = ctx.quota_meter().map(Arc::clone);
    let peer_meter = ctx.peer_meter().map(Arc::clone);
    let result = agent
        .execute_streaming_with_session(
            &user_text,
            pre_user_messages,
            session,
            Some(history),
            None, // caller_id: attribution is handled at the dispatcher boundary
            on_event,
            cancel,
            quota_meter,
            peer_meter,
        )
        .await
        .context("root agent execution failed")?;

    Ok(result.final_answer)
}
