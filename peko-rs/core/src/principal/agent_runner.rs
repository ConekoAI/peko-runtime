use std::sync::Arc;

use anyhow::Context;
use tokio::sync::RwLock;

use crate::agents::agent_config::AgentConfig;
use crate::agents::Agent;
use crate::principal::context::PrincipalContext;
use crate::principal::router::AgentPromptSummary;
use peko_auth::Subject;
use peko_engine::AgenticEvent;
use peko_message::LlmMessage;
use peko_session::manager::{SessionManager, SessionManagerRotationSink};
use peko_session::SessionCreateOptions;

use crate::principal::agent_prompt::AgentPrompt;

/// Build an `AgentConfig` from a thin Markdown prompt + the Principal's
/// configured model.
///
/// `provider_hint` is the resolved configured-model id (the principal's
/// `preferred_model_id`, or a per-message `--model` override). Without a
/// non-`None` hint the root agent's `SubagentExecutor` raises the
/// actionable "no model configured for principal '{name}'" error
/// pointing the user at the principal.toml pin — there is no runtime
/// default model and no other code path that can recover a provider
/// for the root agent at run time.
///
pub fn build_agent_config(
    prompt: &AgentPrompt,
    // Model-first: the principal's pinned configured model id, or
    // `None`. The caller threads this to
    // `Agent::new_with_session_manager_resolver`, which forwards it to
    // `init_provider`.
    _provider_hint: Option<String>,
) -> AgentConfig {
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
/// session using the principal's shared `ToolingRuntime`.
///
/// Root and spawned agents share the principal's explicit tooling runtime.
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
    _available_agents: Vec<AgentPromptSummary>,
    ctx: &PrincipalContext,
    on_event: F,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> anyhow::Result<String>
where
    F: Fn(AgenticEvent) + Send + Sync + 'static,
{
    let _run_permit = ctx
        .tooling
        .agent_runs()
        .for_principal(ctx.principal_id())
        .try_acquire()?;
    let provider_hint = resolve_provider_hint(ctx).await;
    let config = build_agent_config(prompt, provider_hint);

    ctx.tooling().await;

    // Bind the root run to its persisted session store.
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
    // hooks fired by the `PromptRenderer` on every iteration, so a
    // one-shot fire here would be redundant and stale.

    // SessionStart hook was removed (per-turn rebuild refactor):
    // the bootstrap context is now produced by `SessionContextBuild`
    // hooks fired by the `PromptRenderer` on every iteration, so a
    // one-shot fire here would be redundant and stale.

    let history: Vec<LlmMessage> = session.read().await.load_history().await?;

    // Cold-start the root agent. After the Phase-2 redo there is one
    // daemon-global `ToolingRuntime`; the agent picks it up internally.
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
        Arc::clone(&ctx.tooling),
    )
    .await?
    // Bind workspace, identity, and principal services for child execution.
    .with_principal_workspace(ctx.workspace_path.clone())
    .with_principal_name(ctx.name().to_string())
    .with_principal_plan_port(Arc::clone(ctx.plan_port()))
    // Phase 2 of `feature/multi-model-subagents`: bind the
    // principal's `ModelCatalog` (sourced from the same `LlmResolver`
    // the agent already uses for model resolution) so
    // `init_run_builtins` can register the `ModelList` builtin.
    // `ctx.resolver` is `Option` because the CLI one-shot path
    // builds a stateless `Agent` without a resolver; in that case
    // the `ModelList` tool is intentionally omitted (`None` ⇒
    // `init_run_builtins` skips registration).
    .with_model_catalog(ctx.resolver.as_ref().map(|r| Arc::clone(r.catalog())))
    // ADR-045 (self-modification gate): bind caller DID so the
    // `send_peer` tool is registered. `None` ⇒ tool is intentionally
    // omitted (no caller identity to attribute sends to).
    .with_caller_principal_did(ctx.caller_principal_did().cloned())
    // The run's Agent binding uses this metered executor in its private overlay.
    .with_quota_meter(ctx.quota_meter().map(Arc::clone))
    .with_subagent_observability(ctx.observability().cloned());

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
                !set.lock()
                    .expect("seen_models mutex poisoned")
                    .contains(model_id)
            }) as Arc<dyn Fn(&str) -> bool + Send + Sync>)
        };
        agent.with_audit_sink(audit_sink, first_use_lookup)
    };

    // Phase 2 PR 2 (ADR-047 §2.3): bind the MCP context provider
    // so the `{{mcp_context}}` system-prompt section renders
    // Markdown describing the workspace's configured MCP servers.
    // The provider wraps the global `McpManager` (initialised by
    // the daemon at startup); a missing manager (CLI one-shot path
    // without the daemon wiring) leaves the loop on its default
    // `EmptyMcpPromptContextProvider`, which strips the
    // placeholder to empty via `remove_missing=true`.
    let agent = agent.with_mcp_context_provider(
        crate::extensions::mcp::global::global_mcp_manager().map(|mgr| {
            Arc::new(crate::extensions::mcp::workspace::McpManagerPromptContextProvider::new(mgr))
                as Arc<dyn peko_engine::McpPromptContextProvider>
        }),
    );

    // F19: quota meter no longer threaded through Agent. The
    // engine loop fetches the principal's meter directly via
    // `Principal.quota_meter` at run entrypoint and opens
    // `QuotaScope::with` around the run.

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
    // build a `PrincipalContext` at all.
    let quota_meter = ctx.quota_meter().map(Arc::clone);
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
        )
        .await
        .context("root agent execution failed")?;

    Ok(result.final_answer)
}
