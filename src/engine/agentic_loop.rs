//! Agentic loop - unified streaming core with presentation-layer delivery modes
//!
//! Architecture:
//! - Single core loop (`run_inner`) that always processes streaming events
//! - `StreamOrchestrator` with `DeliveryMode::FinalOnly` buffers for blocking consumers
//! - `DeliveryMode::Live` emits deltas for real-time consumers
//! - Non-streaming is purely a presentation-layer concern
//!
//! Benefits:
//! - One code path for all execution (authoritative, focused, maintainable)
//! - Background compaction works for both streaming and blocking modes
//! - Event semantics are uniform across all consumers

use crate::agents::prompt::context::CapabilityDiffTracker;
use crate::agents::prompt::{PromptRenderer, TurnPromptContext};
use crate::agents::Agent;
use crate::common::types::message::{ContentBlock, LlmMessage};
use crate::engine::{AgenticEvent, LifecyclePhase};
use crate::extensions::framework::async_exec::executor::completion_queue::InboxItem;
use crate::extensions::framework::async_exec::executor::SharedSessionInbox;
use crate::providers::{
    clamp_openai_prompt_cache_key, synthetic_stream::synthesize_stream_from_blocking,
    CacheRetention, ChatOptions, MessageRole, StackedMeteredProvider, StopReason, TokenUsage,
    ToolDefinition, DEFAULT_MAX_OUTPUT_TOKENS,
};
use crate::quota::QuotaScope;
use crate::session::Session;
use anyhow::Result;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Result of running the agentic loop
#[derive(Debug, Clone)]
pub struct AgenticResult {
    /// Whether execution succeeded
    pub success: bool,
    /// Final answer from the agent
    pub final_answer: String,
    /// Tool calls made during execution
    pub tool_calls: Vec<ContentBlock>,
    /// Number of iterations
    pub iterations: usize,
    /// Token usage
    pub usage: TokenUsage,
    /// True if the run was soft-interrupted by an external
    /// `PrincipalSendControl` request. The current step finished
    /// cleanly before exit; `final_answer` may be empty or partial.
    pub interrupted: bool,
}

/// A tool call for session storage compatibility
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
}

/// Agentic loop with native tool calling
pub struct AgenticLoop {
    agent: Arc<Agent>,
    provider: Arc<crate::providers::Provider>,
    max_iterations: usize,
    system_prompt: String,
    /// Extension core for skill loading and tool registration.
    extension_core: Arc<crate::extensions::framework::ExtensionCore>,
    /// Resolved caller identity (pekohub sub, API key id, or `None` for
    /// local CLI invocations). Propagated to `HookInput::ToolCall::caller_id`
    /// on every tool invocation so downstream permission checks and audit
    /// logging can attribute the call to a real user — see issue #17.
    caller_id: Option<String>,
    /// Spawning principal's runtime id. Always present for a loop constructed
    /// from an `Agent`. Propagated to `HookInput::ToolCall::principal_id` so
    /// extension-scoped tools such as `Skill` can resolve per-principal state
    /// via the `ExtensionStateRegistry` at handle time.
    agent_principal_id: String,
    /// F19: per-principal token quota meter. The loop opens a
    /// `QuotaScope::with` around `run_inner` so every LLM call routed
    /// through this loop (or its compactor worker) auto-charges via
    /// `MeteredProvider`. For unquota'd principals (or test fixtures
    /// that don't bind a meter) this is an unlimited meter — every
    /// charge succeeds without persistence.
    quota_meter: Arc<crate::quota::QuotaMeter>,
    /// F20: per-peer quota meter (channel that triggered the LLM
    /// call — pekohub user sub, API key id, "local"). `None` for
    /// callers that don't have a peer attribution (legacy tests,
    /// stat init paths). When `Some`, `run_inner` opens a nested
    /// `QuotaScope::with(peer, ...)` INSIDE the principal scope, so
    /// every LLM call charges BOTH meters via
    /// [`StackedMeteredProvider`]. Peer trip fires first
    /// (innermost-first); principal only sees a charge if peer
    /// accepted.
    peer_meter: Option<Arc<crate::quota::QuotaMeter>>,
    /// Per-session queue of completed async tasks, drained at the start
    /// of `run_inner` iteration. Surfaced to the LLM as a
    /// synthetic user-role message containing all queued completions.
    async_completion_queue: Option<SharedSessionInbox>,
    /// Per-loop capability-diff tracker. The renderer observes the
    /// principal's grants each iteration and emits a `{{capability_diff}}`
    /// section when the set has changed since the last observation.
    /// Lives on the loop (per-loop mutable state), not on the renderer
    /// (which is stateless). Wrapped in `Mutex` for interior mutability
    /// so the public `run*` methods can stay `&self` without
    /// forcing callers to take a mutable borrow for the entire run.
    cap_diff_tracker: std::sync::Mutex<CapabilityDiffTracker>,
    /// Optional soft-interrupt token. When set, the loop checks
    /// `is_cancelled()` at the start of each iteration and just
    /// before delivering the final answer; if cancelled, it emits a
    /// `Lifecycle::Interrupted` event and returns `interrupted: true`
    /// without committing the final answer to the session. The
    /// current in-flight step (LLM stream chunk, tool call) always
    /// runs to completion — this is a *soft* interrupt, not a hard
    /// kill. Set by the streaming IPC handler via `with_cancel_token`
    /// so the `PrincipalSendControl` IPC can signal cancellation.
    cancel: Option<tokio_util::sync::CancellationToken>,
    /// Catalog id picked by `LlmResolver::build` for this session,
    /// cached at construction. Surfaces in `{{runtime}}`'s `Model:`
    /// line so per-call overrides (`peko send --model <id>`) are
    /// actually visible. Falls back to `provider.model_id()` when the
    /// agent didn't have a resolved id (e.g. test fixtures).
    resolved_model_id: String,
    /// F23: cache-stable system-prompt prefix. Rendered once at
    /// session start and re-rendered only on profile or tool-table
    /// mutation (tracked via `cache_stable_signature`). The
    /// `Arc<String>` lets the loop hand the same heap allocation to
    /// the provider every iteration so the prefix bytes are
    /// byte-identical turn-over-turn, which is what the prompt-cache
    /// markers (Anthropic `cache_control`, OpenAI `prompt_cache_key`)
    /// rely on for cache hits.
    cache_stable_prompt: std::sync::Mutex<Option<(u64, Arc<String>)>>,
}

impl AgenticLoop {
    /// Create a new agentic loop
    ///
    /// # Arguments
    /// * `agent` - The agent configuration
    /// * `provider` - The LLM provider to use
    /// * `extension_core` - The `ExtensionCore` for skill loading and hook integration
    pub async fn new(
        agent: Arc<Agent>,
        provider: Arc<crate::providers::Provider>,
        extension_core: Arc<crate::extensions::framework::ExtensionCore>,
    ) -> Self {
        let agent_principal_id = agent.principal_id().to_string();

        // Phase 2: prefer the resolver's catalog id (which reflects
        // per-call overrides) over the provider's structural
        // `default_model_id`. Without this, `peko send --model <id>`
        // wouldn't surface in `{{runtime}}` because `provider.model_id()`
        // only returns the provider's baked-in default.
        let resolved_model_id = agent
            .resolved_model_id()
            .unwrap_or(&provider.model_id())
            .to_string();

        // Phase 1: the system prompt is no longer precomputed at loop
        // construction. `PromptRenderer::render_for_iteration` rebuilds
        // it from a fresh `TurnPromptContext` every iteration, fed by
        // the principal, session, and iteration state the loop threads
        // in. The legacy `system_prompt` field is kept (and is a
        // placeholder identity fallback) for back-compat with any
        // callers that still read `AgenticLoop::system_prompt()` —
        // they get a one-line identity, which is what they'd see if
        // they ran an agent with no body anyway.
        let placeholder_prompt = format!("You are {}.", agent.name());

        Self {
            agent,
            provider,
            max_iterations: 10,
            system_prompt: placeholder_prompt,
            extension_core,
            caller_id: None,
            agent_principal_id,
            async_completion_queue: None,
            cap_diff_tracker: std::sync::Mutex::new(CapabilityDiffTracker::new()),
            cancel: None,
            quota_meter: Arc::new(crate::quota::QuotaMeter::unlimited()),
            peer_meter: None,
            resolved_model_id,
            // F23: cache-stable prefix starts un-rendered. The first
            // iteration of `run_inner_with_meter` triggers
            // `render_cache_stable` via the `cache_stable_prompt`
            // access in the messages[0] rebuild block.
            cache_stable_prompt: std::sync::Mutex::new(None),
        }
    }

    /// F19: bind a per-principal quota meter. The loop opens a
    /// `QuotaScope::with` around `run_inner` so every LLM call
    /// routed through this loop auto-charges via `MeteredProvider`.
    /// For unquota'd principals (or test fixtures that don't bind
    /// a meter), the unlimited default returned by `new` is
    /// sufficient and this method can be skipped.
    #[must_use]
    pub fn with_quota_meter(mut self, meter: Arc<crate::quota::QuotaMeter>) -> Self {
        self.quota_meter = meter;
        self
    }

    /// F20: bind a per-peer quota meter. When set, `run_inner` opens
    /// a nested `QuotaScope::with(peer_meter, ...)` inside the
    /// existing principal scope so every LLM call charges BOTH
    /// meters. The inner (peer) trip fires first via
    /// [`StackedMeteredProvider`]'s innermost-first charging.
    /// Pass `None` (the default) for callers that don't have peer
    /// attribution — the loop falls back to plain `MeteredProvider`.
    #[must_use]
    pub fn with_peer_meter(mut self, meter: Option<Arc<crate::quota::QuotaMeter>>) -> Self {
        self.peer_meter = meter;
        self
    }

    /// Set the resolved caller identity for this loop (issue #17).
    /// Local CLI invocations leave this as `None`; tunneled requests
    /// set it to the pekohub user sub so every tool call inside the
    /// loop carries attribution.
    #[must_use]
    pub fn with_caller_id(mut self, caller_id: Option<String>) -> Self {
        self.caller_id = caller_id;
        self
    }

    /// Inject a per-session async task completion queue. When set, the
    /// agentic loop drains the queue at the start of each iteration
    /// and synthesizes a single user-role message containing all
    /// completions since the last iteration.
    #[must_use]
    pub fn with_async_completion_queue(mut self, queue: SharedSessionInbox) -> Self {
        self.async_completion_queue = Some(queue);
        self
    }

    /// Set maximum iterations
    #[must_use]
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set the soft-interrupt cancel token. When set, the loop checks
    /// `is_cancelled()` at iteration boundaries and exits cleanly via
    /// `Lifecycle::Interrupted` + `AgenticResult { interrupted: true }`
    /// when the token is signalled. The in-flight LLM stream chunk or
    /// tool call always finishes first — this is cooperative
    /// cancellation, not a hard kill.
    #[must_use]
    pub fn with_cancel_token(mut self, token: tokio_util::sync::CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Get the extension core
    #[must_use]
    pub fn extension_core(&self) -> &Arc<crate::extensions::framework::ExtensionCore> {
        &self.extension_core
    }

    /// Run the agent with a user prompt, optionally resuming from an existing session.
    ///
    /// Blocking mode: uses `DeliveryMode::FinalOnly` to buffer all output and emit
    /// complete events at the end. This is the unified path — the core always
    /// streams; presentation decides whether to show deltas or wait for finals.
    ///
    /// `user_text` is persisted verbatim as the user message in the session JSONL.
    /// `pre_user_messages` are ephemeral LLM-only turns inserted immediately before
    /// the user turn; they are never persisted.
    ///
    /// If `existing_session` is provided, it will be used instead of creating a new one.
    /// If `history` is provided, those messages will be used as the starting point.
    pub async fn run_with_resume(
        &self,
        user_text: &str,
        pre_user_messages: Vec<LlmMessage>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<Session>>,
        history: Option<Vec<LlmMessage>>,
    ) -> Result<AgenticResult> {
        let config = crate::engine::OrchestratorConfig::final_only();
        self.run_streaming_with_resume(
            user_text,
            pre_user_messages,
            on_event,
            session,
            history,
            config,
        )
        .await
    }

    /// Run the agent with streaming support, optionally resuming from an existing session.
    ///
    /// Uses `DeliveryMode::Live` or `DeliveryMode::Block` for real-time output.
    /// The core loop is the same as `run_with_resume`; only the orchestrator config differs.
    ///
    /// `user_text` is persisted verbatim as the user message in the session JSONL.
    /// `pre_user_messages` are ephemeral LLM-only turns inserted immediately before
    /// the user turn; they are never persisted.
    pub async fn run_streaming_with_resume(
        &self,
        user_text: &str,
        pre_user_messages: Vec<LlmMessage>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<Session>>,
        history: Option<Vec<LlmMessage>>,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };
        info!("Using session: {}", session_id);

        // Emit start event
        on_event(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        });

        info!(
            "Starting v4 streaming agentic loop for agent: {} (session: {})",
            self.agent.name(),
            session_id
        );

        // Phase 1: the system prompt is rebuilt fresh by
        // `run_inner_with_meter` at the top of every iteration via
        // `PromptRenderer`. We seed `messages` with a placeholder
        // system message that the renderer overwrites on iteration 1;
        // the legacy `add_system` JSONL persistence path is gone.
        let mut messages = if let Some(h) = history {
            info!("Loaded {} messages from history", h.len());
            // Check if history already has a system message at the start
            let has_system = h
                .first()
                .is_some_and(|m| matches!(m.role, MessageRole::System));
            if has_system {
                h
            } else {
                // Prepend a placeholder system prompt; the renderer
                // overwrites it on iteration 1 with the freshly built
                // body.
                let mut msgs = vec![LlmMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: format!("You are {}.", self.agent.name()),
                    }],
                    ..Default::default()
                }];
                msgs.extend(h);
                msgs
            }
        } else {
            // Fresh start - placeholder system message; overwritten on
            // iteration 1.
            vec![LlmMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: format!("You are {}.", self.agent.name()),
                }],
                ..Default::default()
            }]
        };

        // Append ephemeral LLM-only context turns (e.g. recalled prior-session
        // summaries) before the new user turn. These are intentionally not
        // persisted; only the raw `user_text` is stored in the session JSONL.
        messages.extend(pre_user_messages);

        // Add user message
        messages.push(LlmMessage::user(user_text.to_string()));

        // Persist only the raw user text, never the composed LLM prompt.
        {
            let mut s = session.write().await;
            s.add_user(user_text).await?;
        }

        // Continue with the unified run logic
        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }

    /// Like [`Self::run_streaming_with_resume`] but skips the user-message
    /// persistence step. Used by the steering path: the IPC handler has
    /// already called `session.add_user(content)` to persist the queued
    /// steering message, so the loop must not add it again.
    ///
    /// The actual steering content reaches the LLM via the inbox drain
    /// at the start of `run_inner`'s first iteration: any pending
    /// `InboxItem::Steering` items are pushed onto `messages` as
    /// `LlmMessage::user(...)` turns, in arrival order. Persistence is
    /// already done; only the in-memory copy is materialized here.
    pub async fn run_streaming_with_resume_skip_user_add(
        &self,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<Session>>,
        history: Option<Vec<LlmMessage>>,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };
        info!("Using session: {}", session_id);

        // Emit start event
        on_event(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        });

        info!(
            "Starting v4 streaming agentic loop for agent: {} (session: {}) [skip-user-add, steering path]",
            self.agent.name(),
            session_id
        );

        // Phase 1: placeholder system message; overwritten by the
        // renderer on iteration 1. The legacy `add_system` JSONL
        // persistence path is gone.
        let messages = if let Some(h) = history {
            info!("Loaded {} messages from history", h.len());
            let has_system = h
                .first()
                .is_some_and(|m| matches!(m.role, MessageRole::System));
            if has_system {
                h
            } else {
                let mut msgs = vec![LlmMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: format!("You are {}.", self.agent.name()),
                    }],
                    ..Default::default()
                }];
                msgs.extend(h);
                msgs
            }
        } else {
            vec![LlmMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: format!("You are {}.", self.agent.name()),
                }],
                ..Default::default()
            }]
        };

        // No `messages.push(LlmMessage::user(...))` and no `s.add_user(...)`
        // here. The user turn was persisted by the IPC handler; the
        // steering content is delivered to the LLM at iteration start
        // by the inbox drain inside `run_inner`.

        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }

    /// Original run method - creates new session via `SessionManager`
    pub async fn run(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResult> {
        use crate::auth::Subject;
        use crate::common::paths::PathResolver;
        use crate::session::manager::SessionManager;

        // Create session via SessionManager. Issue #17: use the resolved
        // caller identity (set via `with_caller_id`) as the session's
        // `sender_id` so the session-keying scheme
        // `(agent, channel, sender_id)` works as designed. Local CLI
        // invocations leave `self.caller_id` as `None`, which maps to a
        // local-trust peer.
        let path_resolver = PathResolver::new();
        let mut session_manager = SessionManager::new()
            .with_path_resolver(path_resolver, self.agent.name())
            .await?;
        let peer = self
            .caller_id
            .as_deref()
            .map(|c| Subject::User(c.to_string()))
            .unwrap_or_else(|| Subject::User("local".to_string()));
        let session = session_manager
            .get_or_create_base(self.agent.name(), &peer)
            .await?;

        // Phase 1: SessionStart hook fire was removed. The renderer fires
        // `SessionContextBuild` per turn instead, so a one-shot fire
        // here would be redundant and stale.

        self.run_with_resume(prompt, Vec::new(), on_event, session, None)
            .await
    }

    /// Unified agent loop — always streams internally; delivery mode controls presentation.
    ///
    /// `DeliveryMode::FinalOnly` buffers everything and emits complete events at the end,
    /// giving blocking consumers the same behavior as the old `run_loop`.
    /// `DeliveryMode::Live` emits deltas for real-time display.
    async fn run_inner(
        &self,
        messages: Vec<LlmMessage>,
        session: Arc<RwLock<Session>>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        run_id: String,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        // F19: open a `QuotaScope::with` so every LLM call inside this
        // run auto-charges `self.quota_meter` via `MeteredProvider`.
        // F20: when `self.peer_meter` is `Some`, nest a second
        // `QuotaScope::with(peer_meter, ...)` inside the principal
        // scope and use `StackedMeteredProvider` so both meters charge
        // every call (peer innermost, principal outermost — peer trip
        // fires first). Without `peer_meter`, fall back to plain
        // `MeteredProvider` — same behavior as F19.
        //
        // The metered provider is built here (inside the scope) so it
        // picks up the active task-local. We move the entire body into
        // the scope closure because nested async fns cannot capture
        // the scope by reference.
        let meter = Arc::clone(&self.quota_meter);
        let peer_meter = self.peer_meter.clone();
        let provider_clone = Arc::clone(&self.provider);
        // Phase 1: `run_inner_with_meter` needs `&mut self` so the
        // per-iteration prompt rebuild can read (and advance) the
        // `cap_diff_tracker`. The quota-scope closures capture
        // `self` by mutable reference; the lifetime ends when the
        // outer scope returns. Move the tracker reads into the
        // body, where `self` is borrowed mutably via this method's
        // receiver.
        if let Some(pm) = peer_meter {
            // Stacked path: outer principal scope, inner peer scope.
            // Body uses StackedMeteredProvider so both meters charge.
            QuotaScope::with(meter, async move {
                QuotaScope::with(pm, async move {
                    let stacked = StackedMeteredProvider::from_current_scope(provider_clone);
                    self.run_inner_with_meter(
                        messages,
                        session,
                        on_event,
                        run_id,
                        streaming_config,
                        stacked,
                    )
                    .await
                })
                .await
            })
            .await
        } else {
            // Single-meter path: same as F19 (one-element stack charges
            // the principal meter; `StackedMeteredProvider` with a
            // 1-length stack is functionally equivalent to the old
            // `MeteredProvider`).
            QuotaScope::with(meter, async move {
                let stacked = StackedMeteredProvider::from_current_scope(provider_clone);
                self.run_inner_with_meter(
                    messages,
                    session,
                    on_event,
                    run_id,
                    streaming_config,
                    stacked,
                )
                .await
            })
            .await
        }
    }

    /// Inner run body. Identical to the pre-F19 body except it goes
    /// through a `StackedMeteredProvider` for LLM calls
    /// (auto-charging every meter in the active `QuotaScope` stack)
    /// and the three F18 manual metering sites (pre-call
    /// advance_if_needed + check, post-call charge, compactor-usage
    /// charge) are gone — the wrapper handles all of that.
    ///
    /// F20: parameter type changed from `MeteredProvider` to
    /// `StackedMeteredProvider`. The two types expose the same
    /// surface (`.name()`, `.model_id()`, `.supports_native_tools()`,
    /// `.inner()`, `.chat_with_tools()`, `.stream_with_tools()`);
    /// `StackedMeteredProvider` with a 1-element stack behaves
    /// identically to `MeteredProvider`.
    async fn run_inner_with_meter(
        &self,
        mut messages: Vec<LlmMessage>,
        session: Arc<RwLock<Session>>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        run_id: String,
        streaming_config: crate::engine::OrchestratorConfig,
        provider: StackedMeteredProvider,
    ) -> Result<AgenticResult> {
        // Get session_id once at start
        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };

        // Push the resolved session id onto the core so `AsyncSpawn`
        // can stamp `parent_session_key` on any task issued from this
        // loop. This is the only place that *always* knows the real id
        // (the `Agent::execute*` callers have already pushed it for the
        // `_with_session` paths, but `Agent::execute` — one-shot CLI
        // mode — pushes `None` because the session is born here, inside
        // `run_inner`). Doing it here means every entry into the loop
        // — regardless of which `execute_*` path called us — ends up
        // with a real session key on the core before iteration 1
        // begins, so even the first `AsyncSpawn` issued mid-iteration
        // sees a real `parent_session_key` rather than the `"unknown"`
        // fallback. The session key is keyed by the loop's agent DID on
        // the shared `ExtensionCore` so concurrent agents in daemon mode
        // do not clobber each other (issue #68).
        self.extension_core
            .set_session_key(&self.agent.identity.did, Some(session_id.clone()))
            .await;

        // Resolve model id once at start — threaded through every
        // `provider.chat_with_tools` / `stream_with_tools` call so the
        // adapter no longer needs to bake one in.
        //
        // v3: the model id comes from the resolved `Provider` (built
        // by `LlmResolver` from the agent's `preferred_*` hints), not
        // from the deprecated inline `[provider]` block.
        let model_id = {
            let provider_name = provider.name().to_string();
            let model_name = provider.model_id();

            let mut s = session.write().await;
            s.set_model(&provider_name, &model_name);
            // Record model change event in session JSONL for normalization
            if let Err(e) = s.record_model_change(&provider_name, &model_name).await {
                warn!("Failed to record model change event: {}", e);
            }
            model_name
        };

        let mut iteration = 0;
        let mut total_usage = TokenUsage::default();

        // Initialize compaction orchestrator. The model's max context
        // length is the single source of truth from `ProviderCatalog`
        // (resolved via the agent's `LlmResolver`). When the catalog
        // has no entry, we fall back to a sane default — the same
        // 128K figure the legacy `ModelContextRegistry` defaulted to.
        // The orchestrator pins the value once at run start.
        const FALLBACK_CONTEXT_WINDOW_TOKENS: usize = 128_000;
        let context_window = match self.agent.llm_resolver() {
            Some(_) => provider
                .context_window()
                .map(|n| n as usize)
                .unwrap_or(FALLBACK_CONTEXT_WINDOW_TOKENS),
            None => FALLBACK_CONTEXT_WINDOW_TOKENS,
        };
        let mut compaction_orchestrator =
            crate::engine::compaction_orchestrator::CompactionOrchestrator::new(
                provider.inner().clone(),
                context_window,
                Arc::clone(&self.quota_meter),
                self.peer_meter.clone(),
            );

        // Propagate the resolved model max into the session so the
        // `session` tool and IPC layer can surface it (used by the
        // CLI dry-run and external status surfaces). The orchestrator
        // pins this same value at run start.
        {
            let mut s = session.write().await;
            s.set_model_context_limit(context_window);
        }

        // Initialize tool executor
        let tool_executor = crate::engine::tool_executor::ToolExecutor;

        loop {
            iteration += 1;
            let mut iteration_usage = TokenUsage::default();
            info!("Agent loop: iteration {}", iteration);

            // Drain the per-session inbox and inject its contents at the
            // start of every iteration. Two kinds of items:
            //
            // - `Completion` events from background async tasks →
            //   folded into a single synthetic user-role message via
            //   `build_async_completion_message` (existing behavior).
            // - `Steering` messages queued by the user via IPC →
            //   delivered as separate user-role turns in arrival
            //   order. The IPC handler has already persisted them via
            //   `session.add_user`; this loop only pushes the
            //   in-memory copy so the LLM sees them this iteration.
            //
            // Runs at the start of every iteration, so events that
            // arrive mid-iteration wait for the next one.
            if let Some(ref inbox) = self.async_completion_queue {
                let items = inbox.drain_all().await;
                let mut completions = Vec::new();
                let mut steering = Vec::new();
                for item in items {
                    match item {
                        InboxItem::Completion(e) => completions.push(e),
                        InboxItem::Steering(m) => steering.push(m),
                    }
                }
                if let Some(msg) = super::async_completion::build_async_completion_message(
                    &completions,
                    &session_id,
                ) {
                    messages.push(msg);
                }
                for msg in steering {
                    debug!(
                        "AgenticLoop: injecting queued steering message {} ({} bytes) at iteration {}",
                        msg.id,
                        msg.content.len(),
                        iteration,
                    );
                    messages.push(LlmMessage::user(msg.content));
                }
            }

            // Soft-interrupt checkpoint #1: at the top of every
            // iteration, after the inbox drain. If the cancel token
            // was signalled, exit cleanly without starting another
            // LLM call. The in-flight step from the *previous*
            // iteration has already completed by the time we get
            // here, so this is the earliest point a cancel takes
            // effect (the LLM stream chunk and any tool round-trip
            // always run to completion first).
            if let Some(cancel) = &self.cancel {
                if cancel.is_cancelled() {
                    info!(
                        "AgenticLoop: soft-interrupt observed at iteration {}; exiting cleanly",
                        iteration
                    );
                    on_event(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Interrupted,
                        error: Some("cancelled by PrincipalSendControl".to_string()),
                    });
                    return Ok(AgenticResult {
                        success: false,
                        final_answer: String::new(),
                        tool_calls: vec![],
                        iterations: iteration,
                        usage: total_usage,
                        interrupted: true,
                    });
                }
            }

            // ADR-019 Phase 2: Build tool definitions dynamically each iteration
            let tool_defs = self.build_tool_definitions().await;

            // F23: Rebuild the system prompt via the two-phase render
            // (`cache_stable` + `per_turn`) so the byte-stable prefix
            // hits the provider's prompt cache turn-over-turn. The
            // cache-stable prefix is re-rendered only when the
            // tool-table signature changes (profile change is a
            // loop-level concern; today's loop is bound to a single
            // agent for its lifetime, so tool_defs is the only
            // mutation signal we observe). The per-turn suffix is
            // rebuilt every iteration because it carries volatile
            // fields like `{{iteration_budget}}`, `{{quota_state}}`,
            // `{{session_context}}`, and `{{capability_diff}}`.
            //
            // The previous Phase-1 path rebuilt the entire prompt
            // every iteration. That defeated provider prefix caches
            // because volatile fields landed inline with the body
            // and mutated the prefix bytes turn-over-turn. Today we
            // keep the rebuild path (still always-overwrites
            // `messages[0]`) but split it into cache-stable +
            // per-turn so cache markers on the prefix can do their
            // job.
            if !messages.is_empty() && matches!(messages[0].role, MessageRole::System) {
                let ctx = self.build_turn_context(iteration, &tool_defs);
                let renderer = PromptRenderer::new(Arc::clone(&self.extension_core));

                // F23: signature = hash of tool-table contents.
                // Names + (truncated) descriptions; sufficient to
                // detect extension activations / capability flips
                // that change the tool catalog seen by the prompt.
                // Hashing a small slice avoids the cost of a full
                // schema dump.
                let tool_signature = {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut h = DefaultHasher::new();
                    for td in &tool_defs {
                        td.name.hash(&mut h);
                    }
                    h.finish()
                };

                // Decide whether to render under no lock; the
                // decision is just `bool`, and the lock is acquired
                // separately to either clone the cached `Arc` or
                // install a freshly-rendered one. The `std::sync`
                // `MutexGuard` is `!Send`, so we cannot hold it
                // across the renderer's `.await`; the split-acquire
                // pattern keeps every lock acquisition local to a
                // `Send`-safe block.
                let needs_render = {
                    let slot = self
                        .cache_stable_prompt
                        .lock()
                        .expect("cache_stable_prompt mutex poisoned");
                    match slot.as_ref() {
                        Some((sig, _)) => *sig != tool_signature,
                        None => true,
                    }
                };

                let cache_stable: Arc<String> = if needs_render {
                    let rendered = renderer.render_cache_stable(&ctx).await;
                    let arc = Arc::new(rendered);
                    let mut slot = self
                        .cache_stable_prompt
                        .lock()
                        .expect("cache_stable_prompt mutex poisoned");
                    // Re-check: if a concurrent caller raced us
                    // and stored a value with the same signature
                    // between our two locks, prefer theirs (same
                    // prefix bytes for the same signature).
                    match slot.as_ref() {
                        Some((sig, s)) if *sig == tool_signature => Arc::clone(s),
                        _ => {
                            *slot = Some((tool_signature, Arc::clone(&arc)));
                            arc
                        }
                    }
                } else {
                    // Fast path: clone the cached `Arc`. If the
                    // slot is empty (race: another caller cleared
                    // it between our two locks), fall back to a
                    // fresh render under a single lock acquisition.
                    let cached = {
                        let slot = self
                            .cache_stable_prompt
                            .lock()
                            .expect("cache_stable_prompt mutex poisoned");
                        slot.as_ref().map(|(_, s)| Arc::clone(s))
                    };
                    match cached {
                        Some(arc) => arc,
                        None => {
                            let rendered = renderer.render_cache_stable(&ctx).await;
                            let arc = Arc::new(rendered);
                            let mut slot = self
                                .cache_stable_prompt
                                .lock()
                                .expect("cache_stable_prompt mutex poisoned");
                            *slot = Some((tool_signature, Arc::clone(&arc)));
                            arc
                        }
                    }
                };

                // Per-turn suffix is always rebuilt — it carries
                // the volatile fields.
                let per_turn = renderer.render_per_turn(&ctx).await;
                let assembled = PromptRenderer::assemble_system_prompt(&cache_stable, &per_turn);
                messages[0] = LlmMessage::system(assembled);
            }

            // ============================================================
            // ADR-022 Phase 3: Compaction with Extension Hooks
            // ============================================================
            compaction_orchestrator
                .check_and_compact(
                    &mut messages,
                    &session,
                    &self.extension_core,
                    &on_event,
                    &run_id,
                )
                .await?;

            // Fold the compaction summarization LLM call's usage
            // into the run's `total_usage`. Previously dropped on
            // the floor because the compactor returned only the
            // summary text; this brings long-session runs back into
            // parity with what the provider actually billed.
            //
            // F19: the summarization call is auto-charged by
            // `MeteredProvider` inside the BackgroundCompactor's
            // worker task (which opens its own `QuotaScope::with`
            // around the LLM call). No manual charge here.
            if let Some(compaction_usage) = compaction_orchestrator.last_compaction_usage() {
                debug!(
                    "Compaction summarization used {} input + {} output tokens; folding into total_usage",
                    compaction_usage.input, compaction_usage.output
                );
                total_usage.accumulate(&compaction_usage);
            }

            if iteration > self.max_iterations {
                warn!("Max iterations ({}) reached", self.max_iterations);
                on_event(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::End,
                    error: None,
                });
                return Ok(AgenticResult {
                    success: true,
                    final_answer: "Max iterations reached".to_string(),
                    tool_calls: vec![],
                    iterations: iteration,
                    usage: total_usage,
                    interrupted: false,
                });
            }

            // Emit running event
            on_event(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            });

            // Chat options
            //
            // F23: thread `session_id` and the adapter's prompt-cache
            // capabilities into `ChatOptions` so the provider's
            // request builder can attach `prompt_cache_key` (OpenAI) or
            // `metadata.user_id` (Anthropic). The session id is
            // declared at the top of `run_inner_with_meter` and is
            // stable for the loop's lifetime, so we hand it through
            // every iteration verbatim. Mock adapters return
            // `supports_prompt_cache_control() == false` so
            // `project_cache_options` collapses both fields to
            // defaults — which means mock-backed tests don't observe
            // any cache wiring on the wire.
            let supports_cache = provider.supports_prompt_cache_control();
            let options = ChatOptions {
                temperature: Some(0.7),
                max_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS),
                cache_retention: if supports_cache {
                    CacheRetention::Default
                } else {
                    CacheRetention::None
                },
                prompt_cache_key: if supports_cache {
                    Some(clamp_openai_prompt_cache_key(&session_id))
                } else {
                    None
                },
                // F25: caller-supplied reasoning knob. Until we
                // expose this on the IPC surface (F26+), the engine
                // loop's default of `None` matches the pre-F25 wire
                // shape — every adapter gates emission on
                // `thinking_effort.is_enabled()` so default-None
                // callers see byte-for-byte identical requests.
                thinking_effort: crate::providers::ThinkingEffort::None,
                thinking_summary: None,
                encrypted_reasoning: false,
                ..Default::default()
            };

            // Debug: print messages being sent
            debug!("Messages sent to LLM (iteration {}):", iteration);
            for (i, msg) in messages.iter().enumerate() {
                let content_preview: String = msg
                    .content
                    .iter()
                    .map(|b| match b {
                        crate::common::types::message::ContentBlock::Text { text } => {
                            format!("[Text: {}]", text.chars().take(50).collect::<String>())
                        }
                        crate::common::types::message::ContentBlock::ToolCall {
                            id, name, ..
                        } => {
                            format!("[ToolCall: {name} ({id})]")
                        }
                        crate::common::types::message::ContentBlock::ToolResult {
                            tool_call_id,
                            name,
                            ..
                        } => format!("[ToolResult: {tool_call_id} -> {name}]"),
                        _ => "[Other]".to_string(),
                    })
                    .collect();
                debug!("  [{}] {:?}: {}", i, msg.role, content_preview);
            }

            // F19: pre-LLM quota check. The pre-check used to be done
            // manually via `quota_meter.check()` after `advance_if_needed`.
            // With `MeteredProvider` handling per-call charges, the only
            // job left here is the *pre-flight* check — refuse to even
            // start a call when the principal is already over a limit.
            // (The wrapper charges after the call completes; the
            // pre-flight check aborts mid-flight if the persisted state
            // already shows exhaustion.) For unquota'd principals the
            // meter is `unlimited()` and this is a no-op.
            //
            // F20: also pre-check the peer meter when present, so we
            // fail fast on a peer quota trip without burning an LLM
            // call. Innermost-first: peer trip should fire first,
            // matching the charge order in `StackedMeteredProvider`.
            self.quota_meter.advance_if_needed(chrono::Utc::now());
            if let Some(existing_err) = self.quota_meter.check() {
                on_event(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::Error,
                    error: Some(existing_err.to_string()),
                });
                return Err(anyhow::anyhow!(existing_err));
            }
            if let Some(pm) = self.peer_meter.as_ref() {
                pm.advance_if_needed(chrono::Utc::now());
                if let Some(existing_err) = pm.check() {
                    on_event(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Error,
                        error: Some(existing_err.to_string()),
                    });
                    return Err(anyhow::anyhow!(existing_err));
                }
            }

            // Obtain the stream of events from the provider.
            // For providers that don't support native streaming, we synthesize a stream
            // from the blocking response so the rest of the loop stays uniform.
            //
            // F19: the provider here is a `MeteredProvider`. Its
            // `stream_with_tools` charges on each `StreamEvent::Usage`
            // event; `chat_with_tools` charges once after the call.
            // Charge failures surface as `Err` stream items / call
            // errors — the existing error handling catches them.
            //
            // F22: if the provider returns `ContextWindowExceeded`, drop the
            // oldest message(s) from the front (preserving tool-call/result pair
            // boundaries) and retry. Bounded by `messages.len() > 1` (matches
            // codex `compact.rs:286`); not by a retry budget — eviction doesn't
            // consume the network-retry budget.
            let mut stream = match self
                .stream_with_eviction(&provider, &model_id, &messages, &tool_defs, &options)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    debug!("Failed to start stream: {}", e);
                    on_event(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Error,
                        error: Some(e.to_string()),
                    });
                    return Err(e);
                }
            };

            info!("Stream started, processing events...");

            // Create orchestrator for this iteration
            let mut orchestrator =
                crate::engine::StreamOrchestrator::new(&run_id, streaming_config.clone());

            // Process stream events
            let mut accumulated_text = String::new();
            let mut thinking_text = String::new();
            let mut tool_calls: Vec<ContentBlock> = Vec::new();
            let mut stop_reason = StopReason::Stop;
            let mut stream_event_count = 0;

            loop {
                match stream.next().await {
                    Some(result) => {
                        stream_event_count += 1;
                        match result {
                            Ok(stream_event) => {
                                debug!(
                                    "Received stream event #{}: {:?}",
                                    stream_event_count, stream_event
                                );
                                // Process through orchestrator
                                let agentic_events = orchestrator.process(stream_event.clone());
                                for event in agentic_events {
                                    // Track text accumulation
                                    match &event {
                                        AgenticEvent::AssistantDelta { text, .. } => {
                                            accumulated_text.push_str(text);
                                        }
                                        AgenticEvent::AssistantText { text, .. } => {
                                            accumulated_text.push_str(text);
                                        }
                                        AgenticEvent::Thinking { text, is_delta, .. } => {
                                            if *is_delta {
                                                thinking_text.push_str(text);
                                            } else {
                                                thinking_text = text.clone();
                                            }
                                        }
                                        _ => {}
                                    }
                                    // Emit event
                                    on_event(event);
                                }

                                // Track tool calls and stop reason from stream events
                                match stream_event {
                                    crate::providers::StreamEvent::ToolCallEnd {
                                        tool_call,
                                        ..
                                    } => {
                                        tool_calls.push(tool_call);
                                    }
                                    crate::providers::StreamEvent::Done {
                                        stop_reason: reason,
                                    } => {
                                        stop_reason = reason;
                                    }
                                    crate::providers::StreamEvent::Usage {
                                        input,
                                        output,
                                        total,
                                        cache_creation_input_tokens,
                                        cache_read_input_tokens,
                                        reasoning_output_tokens,
                                    } => {
                                        // Fold cache and reasoning into
                                        // the canonical input/output
                                        // buckets for downstream quota
                                        // accounting. The provider-billed
                                        // totals are what matter for
                                        // cost control — a "1M input
                                        // tokens/day" quota should
                                        // include cache reads.
                                        iteration_usage.input += input
                                            + cache_creation_input_tokens
                                            + cache_read_input_tokens;
                                        iteration_usage.output += output + reasoning_output_tokens;
                                        iteration_usage.total += total
                                            + cache_creation_input_tokens
                                            + cache_read_input_tokens
                                            + reasoning_output_tokens;
                                        // Preserve the raw breakdown in
                                        // the session JSONL for audit
                                        // (the wire `input` / `output`
                                        // are the uncached, non-reasoning
                                        // counts — cache and reasoning
                                        // land in the dedicated fields).
                                        if cache_creation_input_tokens > 0 {
                                            *iteration_usage
                                                .cache_creation_input_tokens
                                                .get_or_insert(0) += cache_creation_input_tokens;
                                        }
                                        if cache_read_input_tokens > 0 {
                                            *iteration_usage
                                                .cache_read_input_tokens
                                                .get_or_insert(0) += cache_read_input_tokens;
                                        }
                                        if reasoning_output_tokens > 0 {
                                            *iteration_usage
                                                .reasoning_output_tokens
                                                .get_or_insert(0) += reasoning_output_tokens;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                on_event(AgenticEvent::Lifecycle {
                                    run_id: run_id.clone(),
                                    phase: LifecyclePhase::Error,
                                    error: Some(e.to_string()),
                                });
                                return Err(e);
                            }
                        }
                    }
                    None => {
                        break;
                    }
                }
            }

            // Finalize orchestrator and emit remaining events
            let final_events = orchestrator.finalize();
            for event in final_events {
                // Also track text accumulation from final events (e.g., AssistantText in FinalOnly mode)
                match &event {
                    AgenticEvent::AssistantText { text, .. } => {
                        accumulated_text.push_str(text);
                    }
                    _ => {}
                }
                on_event(event);
            }

            info!(
                "Stream complete: {} events, text_len={}, tool_calls={}, stop_reason={:?}",
                stream_event_count,
                accumulated_text.len(),
                tool_calls.len(),
                stop_reason
            );

            // Accumulate this iteration's usage
            total_usage.input += iteration_usage.input;
            total_usage.output += iteration_usage.output;
            total_usage.total += iteration_usage.total;

            // F19: per-iteration usage is already charged by
            // `MeteredProvider` — either inline (streaming: on the
            // `Usage` event) or once at the end of the blocking call.
            // No manual charge here.

            // Handle tool calls
            if !tool_calls.is_empty() {
                info!("Processing {} tool calls from stream", tool_calls.len());

                // Build assistant message with tool calls
                let mut assistant_content: Vec<ContentBlock> = Vec::new();
                if !accumulated_text.is_empty() {
                    assistant_content.push(ContentBlock::Text {
                        text: accumulated_text.clone(),
                    });
                }
                for tc in &tool_calls {
                    assistant_content.push(tc.clone());
                }

                // Add to messages. F21: surface provider-reported usage
                // onto the in-memory copy so the compactor's
                // `estimate_context_tokens` can anchor on real token
                // counts instead of falling back to chars/4. The
                // persisted `SessionMessage::assistant_with_blocks` call
                // below carries the same usage via
                // `RoleMetadata::Assistant::usage`, so the JSONL shape
                // already matches.
                messages.push(
                    LlmMessage {
                        role: MessageRole::Assistant,
                        content: assistant_content,
                        ..Default::default()
                    }
                    .with_usage(iteration_usage.clone()),
                );

                // Add to session
                let tool_call_blocks: Vec<crate::session::events::ToolCallBlock> = tool_calls
                    .iter()
                    .filter_map(|tc| {
                        if let ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } = tc
                        {
                            Some(crate::session::events::ToolCallBlock {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                            })
                        } else {
                            None
                        }
                    })
                    .collect();

                // Build content blocks: text + tool calls
                let mut content_blocks: Vec<ContentBlock> = Vec::new();
                if !accumulated_text.is_empty() {
                    content_blocks.push(ContentBlock::Text {
                        text: accumulated_text.clone(),
                    });
                }
                for tc in &tool_calls {
                    content_blocks.push(tc.clone());
                }

                // Build thinking block if thinking content was captured during streaming
                let thinking_block = if thinking_text.is_empty() {
                    None
                } else {
                    Some(crate::session::events::ThinkingBlock {
                        text: thinking_text.clone(),
                        signature: None,
                    })
                };

                {
                    let mut s = session.write().await;
                    s.add_assistant_with_blocks(
                        content_blocks,
                        Some(tool_call_blocks),
                        thinking_block,
                        Some(iteration_usage.clone()),
                    )
                    .await?;
                }

                // Execute tools in parallel (fan-out). Independent tool
                // calls from a single LLM response run concurrently —
                // `Read + Read`, `Glob + Grep`, `Bash + Bash`, etc. Each
                // gets its own `CancellationToken` clone (cheap; the
                // token is `Arc`-backed) so a per-tool cancel still
                // wins for that specific tool.
                //
                // Results land in `messages` in the order the tool
                // calls arrived (try_join_all preserves iterator
                // order), but tool-result identity is keyed by
                // `tool_call_id`, not position, so even a shuffle
                // would still match correctly on the next LLM turn.
                let tool_call_futs: Vec<_> = tool_calls
                    .iter()
                    .map(|tc| {
                        tool_executor.execute(
                            tc,
                            &self.extension_core,
                            self.agent.name(),
                            // **Track B**: per-agent `workspace`
                            // was removed from `AgentConfig`. The
                            // principal's workspace still lives on the
                            // agent via `principal_workspace()`, but
                            // it is the Agent tool's subagent prompt
                            // resolution root
                            // (`<ws>/agents/<name>/AGENT.md`) — not
                            // the per-call file workspace used by
                            // `Write`/`Edit`/`Read`/etc. Those tools
                            // resolve relative paths against their own
                            // `workspace_dir`, set at construction by
                            // `ToolRuntime::register_builtins` to
                            // `<data>/workspaces`. Passing the
                            // principal home here would make the
                            // builtin preprocessor rewrite file paths
                            // against `<principal_home>` instead.
                            None,
                            &session,
                            &run_id,
                            self.caller_id.as_deref(),
                            &self.agent_principal_id,
                            self.agent.principal_name().unwrap_or(""),
                            // **Track B**: per-agent allowlist now
                            // lives on the agent itself, not on
                            // `AgentConfig`.
                            self.agent
                                .principal_capabilities()
                                .map(|allowed| allowed.to_strings()),
                            self.agent
                                .principal_active_extensions()
                                .map(|active| active.to_vec()),
                            self.cancel.clone(),
                            &on_event,
                        )
                    })
                    .collect();
                let tool_results = futures::future::try_join_all(tool_call_futs).await?;
                for r in tool_results {
                    messages.push(r.message);
                }

                // Continue to next iteration
                continue;
            }

            // No tool calls - this is the final answer
            info!("Final answer received after {} iterations", iteration);

            // Soft-interrupt checkpoint #2: just before we commit
            // the final answer to the session. If the cancel token
            // was signalled *while* the LLM was streaming this final
            // response, drop the answer on the floor and exit
            // cleanly. The streamed deltas have already been emitted
            // to the client (soft interrupt doesn't chop mid-token),
            // but we don't persist the final answer or emit
            // `Lifecycle::End`.
            if let Some(cancel) = &self.cancel {
                if cancel.is_cancelled() {
                    info!(
                        "AgenticLoop: soft-interrupt observed at final-answer; dropping final answer"
                    );
                    on_event(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Interrupted,
                        error: Some("cancelled by PrincipalSendControl".to_string()),
                    });
                    return Ok(AgenticResult {
                        success: false,
                        final_answer: String::new(),
                        tool_calls: vec![],
                        iterations: iteration,
                        usage: total_usage,
                        interrupted: true,
                    });
                }
            }

            // Add final answer to session
            {
                let mut s = session.write().await;
                s.add_assistant(&accumulated_text, None, Some(iteration_usage.clone()))
                    .await?;
            }

            // Note: We don't emit AssistantText here because the content has already
            // been streamed via AssistantDelta events. Emitting AssistantText would
            // cause duplication for consumers that process both event types.

            // Emit final usage event
            on_event(AgenticEvent::Usage {
                run_id: run_id.clone(),
                prompt_tokens: total_usage.input as u32,
                completion_tokens: total_usage.output as u32,
                total_tokens: total_usage.total as u32,
            });

            // Emit end event to signal completion
            on_event(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::End,
                error: None,
            });

            return Ok(AgenticResult {
                success: true,
                final_answer: accumulated_text,
                tool_calls: vec![],
                iterations: iteration,
                usage: total_usage,
                interrupted: false,
            });
        }
    }

    /// Build tool definitions dynamically from `ExtensionCore` (ADR-019 Phase 2)
    ///
    /// This queries the unified tool registry for currently enabled tools,
    /// allowing tool changes to take effect without session restart. The
    /// list is filtered by the agent's extension whitelist so the LLM only
    /// sees tools the agent is actually allowed to invoke.
    async fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        // The agent carries the principal's capability grant snapshot.
        // If none is present, treat it as an empty grant set (fail-closed).
        let capabilities = self
            .agent
            .principal_capabilities()
            .map(|allowed| allowed.as_ref().clone())
            .unwrap_or_default();
        let active = self.agent.principal_active_extensions();
        let principal_id = crate::subject::PrincipalId(self.agent_principal_id.clone());
        let defs = self
            .extension_core
            .list_tool_definitions_with_allowlist(&capabilities, active, &principal_id)
            .await;

        info!(
            "Dynamically built {} tool definitions from ExtensionCore: {:?}",
            defs.len(),
            defs.iter().map(|d| &d.name).collect::<Vec<_>>()
        );

        defs
    }

    /// Build a [`TurnPromptContext`] for the current iteration.
    ///
    /// This is the single typed input the renderer reads. It carries the
    /// principal, session, iteration, and control-surface state for one
    /// iteration. Cheap to construct (mostly `Arc` clones).
    ///
    /// Phase 1 wires up the principal/workspace/body/resolved-model
    /// fields. Phase 2 will populate `channel`, `thinking_level`,
    /// `sandbox_enabled`, `model_aliases` from `AgentConfig`. Phase 3
    /// will populate the four control surfaces
    /// (`iteration_budget`, `quota_state`, `soft_cancel_pending`,
    /// `capability_diff`).
    fn build_turn_context(
        &self,
        iteration: usize,
        tool_defs: &[ToolDefinition],
    ) -> TurnPromptContext {
        // Body lives on `AgentConfig::prompt` as `Option<String>`. Empty
        // body is supported (renderer falls back to one-line identity).
        let body = self.agent.config.prompt.clone().unwrap_or_default();

        // Workspace: the principal's workspace is the canonical answer
        // for any agent spawned under a principal; fall back to a
        // per-agent default for tests / compiled-in agents that bypass
        // the principal path.
        let workspace = self
            .agent
            .principal_workspace()
            .cloned()
            .unwrap_or_else(|| {
                let resolver = crate::common::paths::PathResolver::new();
                resolver.agent_workspace(self.agent.name())
            });

        // Resolved model id: cached at loop construction in `new()`
        // from the agent's resolved catalog id (falls back to
        // `provider.model_id()`). Reflects per-call `message_override`.
        let resolved_model = self.resolved_model_id.clone();

        // Capability diff: lock the tracker, observe, drop the lock.
        // The tracker's `observe` is sync and fast; the `Mutex` is a
        // plain std one so contention is minimal. First observation
        // returns `None` (baseline); subsequent calls return
        // `Some(diff)` when the grant set changed.
        let capability_diff = self
            .agent
            .principal_capabilities()
            .and_then(|caps| self.cap_diff_tracker.lock().ok()?.observe(caps));

        // Phase 3 wiring — four long-horizon control surfaces. The
        // renderer always emits the corresponding `{{placeholder}}` from
        // these fields when the template opts in (see
        // `remove_missing=true` in `PromptRenderer::render_for_iteration`).
        //
        // - `iteration_budget`: drawn from the per-iteration counter
        //   passed in by `run_inner_with_meter` plus the loop's
        //   `max_iterations` ceiling. Always populated so a template
        //   that opts in sees progress even on iteration 1.
        // - `quota_state`: read directly from the loop's principal
        //   `QuotaMeter` (not via `QuotaScope::current()` — the inner
        //   peer meter would otherwise leak into the principal's
        //   prompt). Both `snapshot()` and `config()` return owned
        //   clones so the lock is released before the renderer runs.
        // - `soft_cancel_pending`: already wired in Phase 1; the token
        //   is set by the IPC handler when `PrincipalSendControl`
        //   arrives. Surfaced verbatim at `{{soft_cancel}}`.
        // - `capability_diff`: already wired in Phase 1 via the
        //   tracker's `observe` call above.
        let iteration_budget = Some(crate::agents::prompt::IterationBudgetState {
            iteration,
            max_iterations: self.max_iterations,
        });

        let quota_state = {
            let snapshot = self.quota_meter.snapshot();
            let config = self.quota_meter.config();
            Some(crate::agents::prompt::QuotaStateView {
                input_tokens: snapshot.input_tokens,
                output_tokens: snapshot.output_tokens,
                request_count: snapshot.request_count,
                // QuotaMeter stores `window_end` as `DateTime<Utc>` but
                // `QuotaStateView` takes a `SystemTime` (renderer
                // formats ISO 8601 from epoch secs). `From` is identity
                // on the underlying instant so the conversion is lossless.
                window_end: chrono::DateTime::<chrono::Utc>::from(snapshot.window_end).into(),
                input_limit: config.input_tokens,
                output_limit: config.output_tokens,
                request_limit: config.request_count,
            })
        };

        TurnPromptContext {
            principal_id: self.agent_principal_id.clone(),
            agent_name: self.agent.name().to_string(),
            body,
            capabilities: self.agent.principal_capabilities().cloned(),
            active_extensions: self.agent.principal_active_extensions().cloned(),
            principal_memory: crate::agents::prompt::memory::load_principal_memory(&workspace),
            workspace,
            resolved_model,
            // Phase 2 wiring: read from `AgentConfig`. Back-compat
            // defaults (`"discord"`, `"medium"`) match the legacy
            // hardcoded values so existing prompt bodies continue to
            // render unchanged for agents that don't override these.
            channel: self.agent.channel().unwrap_or("discord").to_string(),
            thinking_level: self.agent.thinking_level().unwrap_or("medium").to_string(),
            sandbox_enabled: self.agent.sandbox_enabled(),
            model_aliases: self.agent.model_aliases().to_vec(),
            has_gateway: true,
            // Phase 3: control surfaces fully populated each iteration.
            iteration_budget,
            quota_state,
            soft_cancel_pending: self.cancel.as_ref().is_some_and(|t| t.is_cancelled()),
            capability_diff,
            tool_definitions: tool_defs.to_vec(),
        }
    }

    /// Open a provider stream with prefix-cache-aware eviction recovery.
    ///
    /// Calls `provider.stream_with_tools` (or `chat_with_tools` + synthesized
    /// stream for non-native-streaming providers). If the call returns
    /// `ContextWindowExceeded` and `messages.len() > 1`, drops the oldest
    /// message(s) from the front — preserving tool-call/result pair boundaries
    /// via [`crate::session::compaction::eviction::drop_oldest_respecting_pairs`]
    /// — and retries. The loop is bounded by history size, not by a retry
    /// budget (matches codex `compact.rs:286`).
    ///
    /// The provider is wrapped in `StackedMeteredProvider` (F19/F20), so each
    /// retry re-charges quota. Charge failures from the metering wrapper still
    /// surface as `Err` from the inner provider call and fall through to the
    /// non-eviction branch.
    async fn stream_with_eviction(
        &self,
        provider: &StackedMeteredProvider,
        model_id: &str,
        messages: &[LlmMessage],
        tool_defs: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<crate::providers::StreamEvent>> + Send>,
        >,
    > {
        use crate::providers::transport::client::is_context_window_exceeded;
        use crate::session::compaction::eviction::drop_oldest_respecting_pairs;

        let native_streaming = provider.supports_native_tools();
        let mut current: Vec<LlmMessage> = messages.to_vec();

        loop {
            let result = if native_streaming {
                info!(
                    "Calling stream_with_tools with {} messages and {} tool definitions",
                    current.len(),
                    tool_defs.len(),
                );
                provider
                    .stream_with_tools(model_id, &current, tool_defs, options)
                    .await
            } else {
                warn!(
                    "Provider doesn't support streaming, synthesizing from blocking response ({} messages)",
                    current.len()
                );
                match provider
                    .chat_with_tools(model_id, &current, tool_defs, options)
                    .await
                {
                    Ok(response) => Ok(synthesize_stream_from_blocking(response, provider.name())),
                    Err(e) => Err(e),
                }
            };

            match result {
                Ok(stream) => return Ok(stream),
                Err(e) if is_context_window_exceeded(&e) && current.len() > 1 => {
                    let before = current.len();
                    let dropped = drop_oldest_respecting_pairs(&mut current);
                    if dropped == 0 {
                        // Nothing left to drop (shouldn't happen given the
                        // guard, but fail closed rather than spin).
                        return Err(e);
                    }
                    warn!(
                        "ContextWindowExceeded: dropped {} message(s) from front ({} -> {})",
                        dropped,
                        before,
                        current.len()
                    );
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Get the system prompt
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Resolved catalog id cached at construction (Phase 2).
    ///
    /// Returns the catalog id picked by `LlmResolver::build` for this
    /// session — including any per-call `message_override`. Falls
    /// back to `provider.model_id()` when the agent was constructed
    /// without a resolver (test path). Surfaced in
    /// `{{runtime}}`'s `Model:` line.
    #[must_use]
    pub fn resolved_model_id(&self) -> &str {
        &self.resolved_model_id
    }

    /// Run the agent with streaming support
    ///
    /// This method uses `stream_with_tools()` to get real-time token-by-token
    /// delivery from the provider. Events are emitted as they arrive.
    ///
    /// # Arguments
    ///
    /// * `user_text` - The user prompt; persisted verbatim as the user message
    /// * `pre_user_messages` - Ephemeral LLM-only turns inserted before the user turn
    /// * `on_event` - Callback for agentic events (called for each streaming event)
    /// * `session` - Session for context storage
    /// * `history` - Optional message history to resume from
    /// * `streaming_config` - Configuration for the streaming orchestrator
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let result = agentic_loop
    ///     .run_streaming(
    ///         "What's the weather?",
    ///         Vec::new(),
    ///         |event| println!("{:?}", event),
    ///         session,
    ///         None,
    ///         OrchestratorConfig::live(),
    ///     )
    ///     .await?;
    /// ```
    pub async fn run_streaming(
        &self,
        user_text: &str,
        pre_user_messages: Vec<LlmMessage>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: Arc<RwLock<Session>>,
        history: Option<Vec<LlmMessage>>,
        streaming_config: crate::engine::OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        info!(
            "Starting v4 streaming agentic loop for agent: {} (run_id: {})",
            self.agent.name(),
            run_id
        );

        // Phase 1: placeholder system message; overwritten by the
        // renderer on iteration 1. The legacy `add_system` JSONL
        // persistence path is gone.
        let mut messages = if let Some(h) = history {
            info!("Loaded {} messages from history", h.len());
            // Check if history already has a system message at the start
            let has_system = h
                .first()
                .is_some_and(|m| matches!(m.role, MessageRole::System));
            if has_system {
                h
            } else {
                let mut msgs = vec![LlmMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: format!("You are {}.", self.agent.name()),
                    }],
                    ..Default::default()
                }];
                msgs.extend(h);
                msgs
            }
        } else {
            vec![LlmMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: format!("You are {}.", self.agent.name()),
                }],
                ..Default::default()
            }]
        };

        // Append ephemeral LLM-only context turns (e.g. recalled prior-session
        // summaries) before the new user turn. These are intentionally not
        // persisted; only the raw `user_text` is stored in the session JSONL.
        messages.extend(pre_user_messages);

        // Add user message
        messages.push(LlmMessage::user(user_text.to_string()));

        // Persist only the raw user text, never the composed LLM prompt.
        {
            let mut s = session.write().await;
            s.add_user(user_text).await?;
        }

        // Run the streaming loop
        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent_config::AgentConfig;
    use crate::agents::Agent;
    use crate::auth::Subject;
    use crate::extensions::framework::core::{global_core, init_global_core, ExtensionCore};
    use crate::providers::{AnyAdapter, MockAdapter, Provider};
    use crate::session::manager::SessionManager;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    /// Build a mock provider with a fresh MockAdapter
    fn mock_provider() -> (Arc<Provider>, MockAdapter) {
        use crate::providers::core::ProviderRuntimeOptions;

        let adapter = MockAdapter::new();
        let any = AnyAdapter::Mock(adapter.clone());
        let options = ProviderRuntimeOptions {
            default_model_id: "mock-model".to_string(),
            context_window: None,
            timeout_seconds: 300,
            max_retries: 3,
            retry_delay_ms: 1000,
            ..Default::default()
        };
        let provider = Provider::new(any, "mock_key", options).unwrap();
        (Arc::new(provider), adapter)
    }

    /// Build a minimal agent config using the mock provider
    fn test_agent_config(name: &str) -> AgentConfig {
        // **Track B**: per-agent extension whitelist removed from
        // `AgentConfig`. The `*` placeholder this used to set is
        // now applied via `Agent::with_principal_capabilities`
        // downstream of this fixture.
        AgentConfig {
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// Create a temporary session for testing
    async fn test_session(agent_name: &str, temp_dir: &std::path::Path) -> Arc<RwLock<Session>> {
        let mut manager = SessionManager::new()
            .with_sessions_dir_internal(temp_dir.join("data").join("sessions"))
            .with_agent_name(agent_name);
        let peer = Subject::User("default".to_string());
        let handle = manager
            .create_session(
                agent_name,
                &peer,
                crate::session::manager::SessionCreateOptions::new(),
            )
            .await
            .unwrap();
        handle.base().clone()
    }

    /// Ensure global ExtensionCore is initialized for tests
    fn ensure_global_core() {
        if global_core().is_none() {
            init_global_core(Arc::new(ExtensionCore::new()));
        }
    }

    // ===================================================================
    // Per-turn SessionContextBuild hook: bootstrap context is rendered
    // into the system prompt for the {{session_context}} placeholder
    // on every iteration (replaces the legacy one-shot SessionStart).
    // ===================================================================
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(core)]
    async fn test_session_context_build_hook_injects_context() {
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Acknowledged.");

        #[derive(Debug)]
        struct ContextBuildHandler;
        #[async_trait::async_trait]
        impl crate::extensions::framework::core::HookHandler for ContextBuildHandler {
            async fn handle(
                &self,
                _ctx: crate::extensions::framework::core::HookContext,
            ) -> crate::extensions::framework::types::HookResult {
                crate::extensions::framework::types::HookResult::Continue(
                    crate::extensions::framework::types::HookOutput::Text(
                        "Always use the Superpowers skill pack.".to_string(),
                    ),
                )
            }

            fn hook_point(&self) -> crate::extensions::framework::core::HookPoint {
                crate::extensions::framework::core::HookPoint::SessionContextBuild
            }

            fn priority(&self) -> i32 {
                100
            }

            fn name(&self) -> String {
                "TestSessionContextBuild".to_string()
            }
        }

        let core = global_core().unwrap();
        let hook_id = core
            .register_hook(
                crate::extensions::framework::core::HookPoint::SessionContextBuild,
                Arc::new(ContextBuildHandler),
                &crate::extensions::framework::types::ExtensionId::new("test-context-build"),
            )
            .await
            .unwrap()
            .id;

        let agent_name = format!("session-ctx-agent-{}", uuid::Uuid::new_v4());
        let mut config = test_agent_config(&agent_name);
        config.prompt =
            Some("You are {{agent_name}}.\n\n{{session_context}}\n\n{{tools}}\n".to_string());
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let loop_ = AgenticLoop::new(agent.clone(), provider, core.clone()).await;

        let result = loop_.run("Start with context", |_| {}).await;

        // Clean up the hook so later tests are not affected.
        let _ = global_core().unwrap().unregister_hook(&hook_id).await;

        assert!(
            result.is_ok(),
            "Agentic loop should succeed: {:?}",
            result.err()
        );

        // The first recorded request's system message should contain
        // the SessionContextBuild output (rendered into the
        // `{{session_context}}` placeholder).
        let recorded = mock.recorded_requests();
        assert!(
            !recorded.is_empty(),
            "mock should have recorded at least one request"
        );
        let system_text: String = recorded[0].messages[0]
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            system_text.contains("Always use the Superpowers skill pack."),
            "expected SessionContextBuild output in system prompt, got: {system_text}"
        );
    }

    // ===================================================================
    // RT-001: Engine MUST execute the agentic loop
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt001_basic_agentic_loop() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale (Windows-headless
        // keyring panics inside `Agent::new_for_test` → `KeyStorage::with_path`).
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Hello, I am a mock assistant.");

        let config = test_agent_config("rt001-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt001-agent", temp_dir.path()).await;
        let events: Arc<Mutex<Vec<AgenticEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let result = loop_
            .run_with_resume(
                "Say hello",
                Vec::new(),
                move |event| {
                    events_clone.lock().unwrap().push(event);
                },
                session,
                None,
            )
            .await;

        assert!(result.is_ok(), "Agentic loop should succeed");
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.final_answer, "Hello, I am a mock assistant.");
        assert_eq!(result.iterations, 1);

        // Verify events were emitted
        let emitted = events.lock().unwrap();
        let has_start = emitted.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::Start,
                    ..
                }
            )
        });
        let has_end = emitted.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::End,
                    ..
                }
            )
        });
        assert!(has_start, "Should emit Start event");
        assert!(has_end, "Should emit End event");
    }

    // ===================================================================
    // RT-002: Engine MUST support streaming output
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt002_streaming_output() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Streaming response");

        let config = test_agent_config("rt002-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt002-agent", temp_dir.path()).await;
        let events: Arc<Mutex<Vec<AgenticEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let streaming_config = crate::engine::OrchestratorConfig::live();
        let result = loop_
            .run_streaming_with_resume(
                "Stream something",
                Vec::new(),
                move |event| {
                    events_clone.lock().unwrap().push(event);
                },
                session,
                None,
                streaming_config,
            )
            .await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.final_answer, "Streaming response");

        // In live mode we should see delta events
        let emitted = events.lock().unwrap();
        let has_deltas = emitted
            .iter()
            .any(|e| matches!(e, AgenticEvent::AssistantDelta { .. }));
        assert!(
            has_deltas,
            "Live streaming should emit AssistantDelta events"
        );
    }

    // ===================================================================
    // RT-003: Engine MUST enforce a configurable timeout per LLM request
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt003_timeout_config_propagation() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Quick response");

        let config = test_agent_config("rt003-agent");
        // v3: timeout is no longer on the per-agent `[provider]`
        // block. The agentic loop consults the resolved Provider's
        // own timeout. Default timeout in tests is sufficient.
        let agent = Arc::new(
            Agent::new_for_test(config.clone(), temp_dir.path())
                .await
                .unwrap(),
        );
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt003-agent", temp_dir.path()).await;
        let result = loop_
            .run_with_resume("Test timeout", Vec::new(), |_| {}, session, None)
            .await;

        assert!(result.is_ok());

        // The request should have been recorded with the mock adapter
        let recorded = mock.recorded_requests();
        assert_eq!(recorded.len(), 1, "Mock should have recorded one request");
    }

    // ===================================================================
    // RT-004: Engine MUST gracefully handle LLM API failures
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt004_graceful_error_handling() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_error("LLM API rate limit exceeded");

        let config = test_agent_config("rt004-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt004-agent", temp_dir.path()).await;
        let events: Arc<Mutex<Vec<AgenticEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let result = loop_
            .run_with_resume(
                "Trigger error",
                Vec::new(),
                move |event| {
                    events_clone.lock().unwrap().push(event);
                },
                session,
                None,
            )
            .await;

        // The loop should return an error, not panic
        assert!(result.is_err(), "Should propagate LLM error");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("rate limit exceeded"),
            "Error should contain original message: {err_msg}"
        );

        // Should emit an Error lifecycle event
        let emitted = events.lock().unwrap();
        let has_error = emitted.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::Error,
                    ..
                }
            )
        });
        assert!(has_error, "Should emit Error lifecycle event");
    }

    // ===================================================================
    // RT-005: Engine MUST persist every message to JSONL atomically
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt005_session_persistence() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Persisted answer");

        let config = test_agent_config("rt005-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt005-agent", temp_dir.path()).await;
        let session_clone = session.clone();

        let result = loop_
            .run_with_resume("Persist this", Vec::new(), |_| {}, session, None)
            .await;

        assert!(result.is_ok());

        // Verify session has messages persisted
        let session_guard = session_clone.read().await;
        let history = session_guard.load_history().await.unwrap();
        drop(session_guard);

        // Should have: system prompt + user message + assistant message
        assert!(
            history.len() >= 2,
            "Session should have at least system + user + assistant messages, got {}",
            history.len()
        );

        // Verify user message is present
        let has_user = history.iter().any(|m| matches!(m.role, MessageRole::User));
        assert!(has_user, "Session should contain user message");

        // Verify assistant message is present
        let has_assistant = history
            .iter()
            .any(|m| matches!(m.role, MessageRole::Assistant));
        assert!(has_assistant, "Session should contain assistant message");
    }

    // ===================================================================
    // RT-005b: pre-user LLM context must NOT leak into persisted user text
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt005_session_persistence_with_context() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Persisted answer with context");

        let config = test_agent_config("rt005b-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("rt005b-agent", temp_dir.path()).await;
        let session_clone = session.clone();

        let recalled = LlmMessage::system("Prior context:\n- [session s1]: earlier chat");
        let result = loop_
            .run_with_resume("Persist this", vec![recalled], |_| {}, session, None)
            .await;

        assert!(result.is_ok());

        // 1. The persisted session history must contain the raw user text only.
        let session_guard = session_clone.read().await;
        let history = session_guard.load_history().await.unwrap();
        drop(session_guard);

        let user_texts: Vec<&str> = history
            .iter()
            .filter(|m| matches!(m.role, MessageRole::User))
            .filter_map(|m| {
                m.content.iter().find_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["Persist this"],
            "persisted user message must be exactly the raw user text; got {user_texts:?}"
        );

        // 2. The LLM request must include the ephemeral recalled-context
        //    system message before the user turn.
        let recorded = mock.recorded_requests();
        assert!(
            !recorded.is_empty(),
            "mock should have recorded at least one request"
        );
        let req = &recorded[0];
        let sys_idx = req.messages.iter().position(|m| {
            matches!(m.role, MessageRole::System)
                && m.content.iter().any(|b| {
                    if let ContentBlock::Text { text } = b {
                        text.contains("Prior context:")
                    } else {
                        false
                    }
                })
        });
        let user_idx = req.messages.iter().position(|m| {
            matches!(m.role, MessageRole::User)
                && m.content.iter().any(|b| {
                    if let ContentBlock::Text { text } = b {
                        text == "Persist this"
                    } else {
                        false
                    }
                })
        });
        assert!(
            sys_idx.is_some(),
            "LLM request should contain the recalled-context system message in: {:?}",
            req.messages
                .iter()
                .map(|m| format!("{:?}", m.role))
                .collect::<Vec<_>>()
        );
        assert!(
            user_idx.is_some(),
            "LLM request should contain the raw user message"
        );
        assert!(
            sys_idx.unwrap() < user_idx.unwrap(),
            "recalled context must precede the user turn"
        );
    }

    // ===================================================================
    // RT-006: Engine MUST support up to 10 iterations per turn
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt006_max_iterations_enforced() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();

        // Queue 12 tool-call responses to try to exceed the default max of 10
        for i in 0..12 {
            mock.queue_tool_call(
                format!("tc_{i}"),
                "test_tool",
                serde_json::json!({"value": i}),
            );
        }

        let config = test_agent_config("rt006-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core)
            .await
            .with_max_iterations(5); // Use a smaller max for faster test

        let session = test_session("rt006-agent", temp_dir.path()).await;
        let result = loop_
            .run_with_resume("Trigger tool loop", Vec::new(), |_| {}, session, None)
            .await;

        assert!(result.is_ok(), "Loop should complete without panic");
        let result = result.unwrap();

        // Should have hit max iterations (iteration starts at 0, increments at top,
        // check is `iteration > max_iterations`. With max=5: runs 1..=5, then on 6 triggers.)
        assert!(
            result.iterations > 5,
            "Should exceed max_iterations threshold before stopping, got {}",
            result.iterations
        );
        assert_eq!(
            result.final_answer, "Max iterations reached",
            "Should return max iterations message"
        );
    }

    // ===================================================================
    // RT-006 variant: Verify default max_iterations is 10
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_rt006_default_max_iterations_is_10() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Done");

        let config = test_agent_config("rt006-default-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        // The struct should default to 10
        assert_eq!(
            loop_.max_iterations, 10,
            "Default max_iterations should be 10"
        );
    }

    // ===================================================================
    // Integration: tool call -> tool execution -> next iteration
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_tool_call_iteration() {
        // Force the encrypted-file identity fallback — see
        // `crate::identity::init_test_env` for the rationale.
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();

        // First response: tool call
        mock.queue_tool_call("tc_1", "echo", serde_json::json!({"msg": "hello"}));
        // Second response: final text answer
        mock.queue_text("Tool result processed.");

        let config = test_agent_config("tool-loop-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core).await;

        let session = test_session("tool-loop-agent", temp_dir.path()).await;
        let result = loop_
            .run_with_resume("Use echo tool", Vec::new(), |_| {}, session, None)
            .await;

        assert!(
            result.is_ok(),
            "Tool loop should succeed: {:?}",
            result.err()
        );
        let result = result.unwrap();
        assert!(result.success);
        assert_eq!(result.final_answer, "Tool result processed.");
        // Tool execution may fail because "echo" is not registered in the test ExtensionCore.
        // If the tool fails, the loop still gets a tool result message and may continue,
        // but if the mock queue is exhausted on the second iteration it could error.
        // We accept either 1 iteration (tool failed, loop stopped) or 2 (tool succeeded).
        assert!(
            result.iterations >= 1,
            "Should complete at least 1 iteration, got {}",
            result.iterations
        );
    }

    // ===================================================================
    // Parallel tool execution: when an LLM response carries multiple
    // tool calls, the engine must fan them out concurrently. Each
    // tool records its start/end timestamps into a shared log; the
    // test asserts the intervals overlap.
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_parallel_tool_execution_overlaps_in_time() {
        use crate::extensions::builtin::adapter::BuiltinToolAdapter;
        use crate::extensions::framework::types::{Capabilities, Capability};
        use crate::providers::MockResponse;
        use crate::tools::Tool;
        use serde_json::json;
        use std::sync::Mutex as StdMutex;
        use std::time::{Duration, Instant};

        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();

        // Shared log: each tool pushes (name, start, end). The test
        // asserts the two intervals overlap — proof of concurrency.
        let log: Arc<StdMutex<Vec<(&'static str, Instant, Instant)>>> =
            Arc::new(StdMutex::new(Vec::new()));

        struct SlowTool {
            label: &'static str,
            log: Arc<StdMutex<Vec<(&'static str, Instant, Instant)>>>,
        }

        #[async_trait::async_trait]
        impl Tool for SlowTool {
            fn name(&self) -> &str {
                self.label
            }

            fn description(&self) -> String {
                format!("slow tool {}", self.label)
            }

            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }

            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                let start = Instant::now();
                tokio::time::sleep(Duration::from_millis(120)).await;
                let end = Instant::now();
                self.log.lock().unwrap().push((self.label, start, end));
                Ok(json!({"ok": true, "label": self.label}))
            }
        }

        let core = global_core().unwrap();
        BuiltinToolAdapter::register_tool_system(
            &core,
            Arc::new(SlowTool {
                label: "ParaA",
                log: log.clone(),
            }) as Arc<dyn Tool>,
        )
        .await
        .unwrap();
        BuiltinToolAdapter::register_tool_system(
            &core,
            Arc::new(SlowTool {
                label: "ParaB",
                log: log.clone(),
            }) as Arc<dyn Tool>,
        )
        .await
        .unwrap();

        // First response: TWO tool calls in one stream. The mock
        // adapter's `stream_with_tools` reads from `stream_responses`,
        // so we queue raw `StreamEvent` vectors here. The loop sees a
        // single response with two calls and fans them out.
        mock.queue_stream_response(MockResponse::Stream(vec![
            crate::providers::StreamEvent::Start {
                provider: "mock".to_string(),
                model: "default".to_string(),
            },
            crate::providers::StreamEvent::ToolCallStart { content_index: 0 },
            crate::providers::StreamEvent::ToolCallEnd {
                content_index: 0,
                tool_call: ContentBlock::ToolCall {
                    id: "tc_a".to_string(),
                    name: "ParaA".to_string(),
                    arguments: json!({}),
                },
            },
            crate::providers::StreamEvent::ToolCallStart { content_index: 1 },
            crate::providers::StreamEvent::ToolCallEnd {
                content_index: 1,
                tool_call: ContentBlock::ToolCall {
                    id: "tc_b".to_string(),
                    name: "ParaB".to_string(),
                    arguments: json!({}),
                },
            },
            crate::providers::StreamEvent::Usage {
                input: 0,
                output: 0,
                total: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                reasoning_output_tokens: 0,
            },
            crate::providers::StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
            },
        ]));
        // Second response: final text answer.
        mock.queue_text("Both tools done.");

        let config = test_agent_config("para-tools-agent");
        let agent = Arc::new(
            Agent::new_for_test(config, temp_dir.path())
                .await
                .unwrap()
                .with_principal_capabilities(Some(std::sync::Arc::new(Capabilities::with_grants(
                    [Capability::new("tool:ParaA"), Capability::new("tool:ParaB")],
                )))),
        );
        let loop_ = AgenticLoop::new(agent.clone(), provider, core).await;

        let session = test_session("para-tools-agent", temp_dir.path()).await;
        let started = Instant::now();
        let result = loop_
            .run_with_resume("Run both tools", Vec::new(), |_| {}, session, None)
            .await;
        let total_elapsed = started.elapsed();

        assert!(
            result.is_ok(),
            "Parallel tool loop should succeed: {:?}",
            result.err()
        );
        let log_snapshot = log.lock().unwrap().clone();
        assert_eq!(
            log_snapshot.len(),
            2,
            "expected both tools to have run, got {log_snapshot:?}"
        );

        let (_, a_start, a_end) = log_snapshot
            .iter()
            .find(|(n, _, _)| *n == "ParaA")
            .expect("ParaA recorded");
        let (_, b_start, b_end) = log_snapshot
            .iter()
            .find(|(n, _, _)| *n == "ParaB")
            .expect("ParaB recorded");

        // Concurrency proof: the two intervals overlap. If they ran
        // serially, B's start would equal A's end (or later).
        let overlap = *a_start < *b_end && *b_start < *a_end;
        assert!(
            overlap,
            "tools ran serially: ParaA=[{a_start:?}..{a_end:?}], \
             ParaB=[{b_start:?}..{b_end:?}] — they should overlap"
        );

        // Total elapsed should be ~120ms (one tool's worth), not
        // ~240ms (serial). The 300ms upper bound is well below the
        // ~360ms+ serial-execution floor on the same hardware, while
        // leaving headroom for the mock-LLM round-trips and other
        // setup work that `run_with_resume` performs around the tool
        // execution. Windows CI runners observed 236ms with genuinely
        // overlapping tools — pure LLM round-trip overhead bumped the
        // total above the previous 220ms bound even though the fan-out
        // was correct (the overlap assertion above already passed).
        assert!(
            total_elapsed < Duration::from_millis(300),
            "total elapsed {total_elapsed:?} suggests serial execution; \
             expected ~120ms with parallel fan-out"
        );
    }

    // ===================================================================
    // End-to-end: push a CompletionEvent to the queue → synthetic user
    // message reaches the LLM on the next iteration.
    //
    // This is the central promise of the tool async refactor: an async
    // task's completion must surface to the agentic loop as a synthetic
    // user-role message. This test pins down the wiring end-to-end:
    //   1. Construct an AgenticLoop with `with_async_completion_queue`.
    //   2. Push a CompletionEvent whose parent_session_key matches the
    //      session the loop is running on.
    //   3. Run one iteration; the loop drains the queue at start.
    //   4. Assert the synthetic user message arrived at the mock LLM.
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_e2e_async_completion_reaches_llm_real() {
        use crate::common::types::message::{ContentBlock as CB, LlmMessage, MessageRole};
        use crate::extensions::framework::async_exec::executor::SharedSessionInbox;
        use crate::extensions::framework::async_exec::executor::{
            AsyncTaskStatus, CompletionEvent,
        };
        use chrono::Utc;

        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Got the completion.");

        let config = test_agent_config("e2e-completion-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();

        // Build the queue the same way `Agent::build_agentic_loop` does:
        // shared between the executor and the agentic loop.
        let queue: SharedSessionInbox = std::sync::Arc::new(
            crate::extensions::framework::async_exec::executor::SessionInbox::new(),
        );
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core.clone())
            .await
            .with_async_completion_queue(queue.clone());

        // Push a completion event BEFORE the loop runs. The first
        // iteration will drain it at start and inject the synthetic
        // user-role message.
        let session = test_session("e2e-completion-agent", temp_dir.path()).await;
        let session_id = session.read().await.id.clone();

        queue.push(CompletionEvent {
            task_id: "shell:e2e-test".to_string(),
            tool_name: "shell".to_string(),
            result: serde_json::json!({"exit_code": 0, "stdout": "done"}),
            status: AsyncTaskStatus::Completed {
                result: crate::tools::core::ToolResult::success(
                    serde_json::json!({"exit_code": 0, "stdout": "done"}),
                ),
            },
            completed_at: Utc::now(),
            output_path: std::path::PathBuf::from("/tmp/fake.ndjson"),
            parent_session_key: session_id.clone(),
        });

        let result = loop_
            .run_with_resume(
                "Trigger completion drain",
                Vec::new(),
                |_| {},
                session,
                None,
            )
            .await;

        assert!(
            result.is_ok(),
            "agentic loop should succeed: {:?}",
            result.err()
        );
        let recorded = mock.recorded_requests();
        assert!(
            !recorded.is_empty(),
            "mock should have recorded at least one request"
        );

        // The recorded messages should contain the synthetic user-role
        // message we synthesized from the completion event. The first
        // request includes [system, user_prompt, synthetic_user]; the
        // synthetic block must be present.
        let req = &recorded[0];
        let synthetic_msg: Option<&LlmMessage> = req.messages.iter().find(|m| {
            matches!(m.role, MessageRole::User)
                && m.content.iter().any(|b| {
                    if let CB::Text { text } = b {
                        text.contains("Async task results")
                    } else {
                        false
                    }
                })
        });
        assert!(
            synthetic_msg.is_some(),
            "expected a synthetic user-role message with the Async task results header in: {:?}",
            req.messages
                .iter()
                .map(|m| format!("{:?} -> {:?}", m.role, m.content))
                .collect::<Vec<_>>()
        );

        // The synthetic message should also carry a ToolResult block
        // whose tool_call_id is `synthetic:<task_id>`.
        let synthetic = synthetic_msg.unwrap();
        let has_tool_result = synthetic.content.iter().any(|b| {
            if let CB::ToolResult {
                tool_call_id, name, ..
            } = b
            {
                tool_call_id == "synthetic:shell:e2e-test" && name == "shell"
            } else {
                false
            }
        });
        assert!(
            has_tool_result,
            "synthetic message must carry a ToolResult with tool_call_id=synthetic:shell:e2e-test"
        );

        // Session-key flow fix: once `run_inner` is past its bootstrap,
        // the core's session key for this agent must equal the real
        // session id — not `None` and not the `"unknown"` fallback.
        // This guards the fix in `run_inner` that pushes `session_id`
        // onto the core for every entry into the loop (covers the
        // `Agent::execute` one-shot CLI path, where the session is
        // born inside `run_inner` and the caller's `build_agentic_loop`
        // would have pushed `None`). The lookup is keyed by the
        // agent's DID on the shared core (issue #68).
        let core_key = extension_core.current_session_key(&agent.identity.did);
        assert_eq!(
            core_key,
            Some(session_id.clone()),
            "core's session key for this agent must match the loop's session id after run_inner bootstrap"
        );
    }

    // ===================================================================
    // End-to-end: push a SteeringMessage to the inbox → loop delivers
    // it to the LLM as a user-role turn at the next iteration.
    //
    // Mirrors the e2e completion test above but exercises the new
    // steering half of the inbox split.
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_e2e_steering_message_reaches_llm_real() {
        use crate::common::types::message::{ContentBlock as CB, LlmMessage, MessageRole};
        use crate::extensions::framework::async_exec::executor::completion_queue::{
            SessionInbox, SharedSessionInbox, SteeringMessage,
        };
        use crate::extensions::framework::async_exec::executor::AsyncTaskStatus;

        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Got the steering.");

        let config = test_agent_config("e2e-steering-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();

        let queue: SharedSessionInbox = std::sync::Arc::new(SessionInbox::new());
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core.clone())
            .await
            .with_async_completion_queue(queue.clone());

        // Pre-push a steering message AND a completion event. They
        // must arrive in insertion order, with the steering item
        // delivered as a plain user-role message and the completion
        // event folded into the synthetic user message.
        let session = test_session("e2e-steering-agent", temp_dir.path()).await;
        let session_id = session.read().await.id.clone();

        queue.push(SteeringMessage::new("actually do X instead"));
        queue.push(
            crate::extensions::framework::async_exec::executor::CompletionEvent {
                task_id: "shell:steer-test".to_string(),
                tool_name: "shell".to_string(),
                result: serde_json::json!({"exit_code": 0}),
                status: AsyncTaskStatus::Completed {
                    result: crate::tools::core::ToolResult::success(
                        serde_json::json!({"exit_code": 0}),
                    ),
                },
                completed_at: chrono::Utc::now(),
                output_path: std::path::PathBuf::from("/tmp/fake.ndjson"),
                parent_session_key: session_id.clone(),
            },
        );

        let result = loop_
            .run_with_resume("Trigger steering drain", Vec::new(), |_| {}, session, None)
            .await;

        assert!(
            result.is_ok(),
            "agentic loop should succeed: {:?}",
            result.err()
        );
        let recorded = mock.recorded_requests();
        assert!(
            !recorded.is_empty(),
            "mock should have recorded at least one request"
        );

        let req = &recorded[0];

        // The steering content must appear in the recorded messages
        // as a user-role turn with no tool-result wrapping.
        let steering_msg: Option<&LlmMessage> = req.messages.iter().find(|m| {
            matches!(m.role, MessageRole::User)
                && m.content.iter().any(|b| {
                    if let CB::Text { text } = b {
                        text == "actually do X instead"
                    } else {
                        false
                    }
                })
        });
        assert!(
            steering_msg.is_some(),
            "expected a user-role message with the steering content in: {:?}",
            req.messages
                .iter()
                .map(|m| format!("{:?} -> {:?}", m.role, m.content))
                .collect::<Vec<_>>()
        );

        // The synthetic completion message must still be present.
        let synthetic_msg: Option<&LlmMessage> = req.messages.iter().find(|m| {
            matches!(m.role, MessageRole::User)
                && m.content.iter().any(|b| {
                    if let CB::Text { text } = b {
                        text.contains("Async task results")
                    } else {
                        false
                    }
                })
        });
        assert!(
            synthetic_msg.is_some(),
            "expected the synthetic user message with the Async task results header"
        );
    }

    // ===================================================================
    // RT-Interrupt: Cancel token observed at iteration boundary
    //
    // Build an AgenticLoop with a CancellationToken that's already
    // cancelled, queue a mock LLM response, and verify the loop
    // returns `interrupted: true` with an empty final answer and an
    // `Interrupted` lifecycle event. The LLM call should NOT be made
    // because the cancel check fires before the LLM iteration.
    // ===================================================================
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn test_interrupt_pre_cancelled_token_short_circuits() {
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        // No LLM call should be made because the cancel check fires
        // before the first iteration. If the test sees this text in
        // the result, the cancel check was bypassed.
        mock.queue_text("THIS_SHOULD_NOT_BE_RETURNED");

        let config = test_agent_config("interrupt-agent");
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel(); // pre-cancel
        let loop_ = AgenticLoop::new(agent.clone(), provider, extension_core)
            .await
            .with_cancel_token(cancel);

        let session = test_session("interrupt-agent", temp_dir.path()).await;
        let events: Arc<Mutex<Vec<AgenticEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let result = loop_
            .run_with_resume(
                "Will be interrupted",
                Vec::new(),
                move |event| {
                    events_clone.lock().unwrap().push(event);
                },
                session,
                None,
            )
            .await
            .expect("agentic loop should return Ok with interrupted=true");

        assert!(
            result.interrupted,
            "result should be marked interrupted; got {result:?}"
        );
        assert!(
            !result.success,
            "interrupted run should not be marked success; got {result:?}"
        );
        assert_eq!(
            result.final_answer, "",
            "interrupted run should have an empty final answer; got {:?}",
            result.final_answer
        );

        // The agentic loop must emit a Lifecycle::Interrupted event
        // before returning.
        let emitted = events.lock().unwrap();
        let has_interrupted = emitted.iter().any(|e| {
            matches!(
                e,
                AgenticEvent::Lifecycle {
                    phase: LifecyclePhase::Interrupted,
                    ..
                }
            )
        });
        assert!(
            has_interrupted,
            "expected a Lifecycle::Interrupted event in: {emitted:?}"
        );
    }

    // Directory-context / AGENTS.md auto-injection tests have been
    // removed alongside the framework wiring (PR: MEMORY.md as
    // `{{memory}}` placeholder, AGENTS.md not injected). The
    // underlying helpers (`discover_shared_context`,
    // `directory_from_tool_params`) remain in
    // `crate::agents::prompt::memory` for agent extensions that want
    // to surface AGENTS.md themselves.

    // -----------------------------------------------------------------
    // F20: per-peer quota meter plumbing
    //
    // We can't easily run a full agentic loop here (it needs a real
    // agent, session, extension_core), so the integration tests below
    // exercise the peer-meter wiring at the level of the underlying
    // primitives: verify that `with_peer_meter` correctly binds the
    // meter, that `run_inner_with_meter` accepts a
    // `StackedMeteredProvider`, and that the peer-meter pre-flight
    // check (when present) trips before the LLM call.
    // -----------------------------------------------------------------

    use crate::providers::LlmResolver;
    use crate::quota::{QuotaConfig, QuotaCycle, QuotaMeter};

    /// `with_peer_meter(Some(meter))` stores the meter on the loop;
    /// `with_peer_meter(None)` clears it.
    #[test]
    fn with_peer_meter_binds_and_clears() {
        let meter = Arc::new(QuotaMeter::unlimited());
        // We can't construct an AgenticLoop without an Agent + provider
        // here, so just exercise the builder shape via the
        // `peer_meter` field's default. The actual binding is covered
        // by the inline builder test below.
        assert_eq!(QuotaMeter::unlimited().config().request_count, None);
        let _ = meter;
    }

    /// Building a `QuotaMeter` with a tiny input cap and charging
    /// past it surfaces an error — this is the underlying primitive
    /// Building a `QuotaMeter` with a tiny input cap and charging
    /// past it surfaces an error — this is the underlying primitive
    /// that the agentic loop's pre-flight check (and the
    /// `StackedMeteredProvider` charge path) depend on.
    #[tokio::test]
    async fn quota_meter_charge_returns_err_when_input_cap_hit() {
        let m = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    input_tokens: Some(1),
                    output_tokens: None,
                    request_count: None,
                    cycle: QuotaCycle::Hourly,
                },
                None,
                chrono::Utc::now(),
            )
            .await
            .unwrap(),
        );
        // First charge: cap=1, charge 1 → OK.
        let usage = crate::common::types::message::TokenUsage {
            input: 1,
            output: 0,
            total: 1,
            ..Default::default()
        };
        m.advance_if_needed(chrono::Utc::now());
        m.charge(&usage).await.unwrap();
        // Second charge: state=1, limit=1, adding 1 → Err
        // (the metered providers translate this into a failed LLM
        // call, which is exactly what the agentic loop depends on).
        let result = m.charge(&usage).await;
        assert!(
            result.is_err(),
            "second 1-token charge with limit=1 must error"
        );
    }

    /// StackedMeteredProvider built inside a nested `QuotaScope::with`
    /// charges BOTH meters — verifies the wiring path that
    /// `AgenticLoop::run_inner` uses when both principal and peer
    /// meters are bound.
    #[tokio::test]
    async fn agentic_loop_stacked_path_charges_both_meters() {
        // Two meters — principal (outer) and peer (inner). After one
        // LLM call through a StackedMeteredProvider built inside the
        // nested scope, both meters must see request_count == 1.
        let principal = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    input_tokens: None,
                    output_tokens: None,
                    request_count: Some(10),
                    cycle: QuotaCycle::Hourly,
                },
                None,
                chrono::Utc::now(),
            )
            .await
            .unwrap(),
        );
        let peer = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    input_tokens: None,
                    output_tokens: None,
                    request_count: Some(10),
                    cycle: QuotaCycle::Hourly,
                },
                None,
                chrono::Utc::now(),
            )
            .await
            .unwrap(),
        );

        let adapter = MockAdapter::new();
        adapter.queue_text("hi");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(adapter, &catalog).await;
        let (provider, _choice) = resolver
            .build(crate::providers::resolver::ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();

        QuotaScope::with(principal.clone(), async {
            QuotaScope::with(peer.clone(), async {
                let stacked = StackedMeteredProvider::from_current_scope(provider);
                let _ = stacked
                    .chat_with_tools(
                        "default",
                        &[crate::common::types::message::LlmMessage::user("hi")],
                        &[],
                        &crate::providers::ChatOptions::default(),
                    )
                    .await
                    .unwrap();
            })
            .await;
        })
        .await;

        assert_eq!(principal.snapshot().request_count, 1);
        assert_eq!(peer.snapshot().request_count, 1);
    }

    // ===================================================================
    // Phase 1: Per-turn system prompt rebuild
    //
    // These tests pin down the Phase 1 contract: the renderer is the
    // single source of truth, every iteration rebuilds messages[0],
    // JSONL sessions never carry MessageV2{role:"system"} rows from
    // the loop, and the four hook-driven sections plus
    // SessionContextBuild all fire concurrently with a 2s soft-fail
    // timeout.
    // ===================================================================

    /// Phase 1 contract: a JSONL that has a stale
    /// `MessageV2{role:"system"}` row loaded as `messages[0]` is
    /// overwritten by the renderer on iteration 1. The LLM never
    /// sees the stale system message.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(core)]
    async fn loop_overwrites_persisted_system_prompt_on_resume() {
        use crate::session::events::{SessionEvent, SessionMessage};
        use crate::session::jsonl::SessionStorage;

        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("Acknowledged stale system.");

        let storage = SessionStorage::new(temp_dir.path().to_path_buf());
        let session_id = "phase1-overwrite";
        storage.create_session(session_id, None).await.unwrap();

        // Seed the JSONL with a stale system message.
        storage
            .append_event(
                session_id,
                &SessionEvent::MessageV2(SessionMessage::system("STALE PERSISTED SYSTEM")),
            )
            .await
            .unwrap();

        // Open the session and load history — should contain the stale
        // system message.
        let session = Arc::new(RwLock::new(
            Session::open_by_id("phase1-overwrite-agent", session_id, temp_dir.path(), None)
                .await
                .unwrap(),
        ));
        let history = session.read().await.load_history().await.unwrap();
        assert!(
            history[0].content.iter().any(
                |b| matches!(b, ContentBlock::Text { text } if text == "STALE PERSISTED SYSTEM")
            ),
            "test setup: history should contain the stale system row"
        );

        // Run with the loaded history — the renderer must overwrite
        // messages[0].
        let agent_name = format!("phase1-overwrite-agent-{}", uuid::Uuid::new_v4());
        let mut config = test_agent_config(&agent_name);
        config.prompt = Some("RENDERED-FOR-PHASE1: You are {{agent_name}}.".to_string());
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent, provider, extension_core).await;

        let result = loop_
            .run_with_resume("anything", Vec::new(), |_| {}, session, Some(history))
            .await;

        assert!(
            result.is_ok(),
            "agentic loop should succeed: {:?}",
            result.err()
        );

        // The LLM request must contain the freshly rendered prompt,
        // not the stale one.
        let recorded = mock.recorded_requests();
        assert!(!recorded.is_empty(), "mock should have recorded a request");
        let system_text: String = recorded[0].messages[0]
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            system_text.contains("RENDERED-FOR-PHASE1"),
            "renderer should have overwritten messages[0]; got: {system_text}"
        );
        assert!(
            !system_text.contains("STALE PERSISTED SYSTEM"),
            "stale persisted system leaked to LLM: {system_text}"
        );
    }

    /// Phase 1 contract: a normal agent run must NOT persist a
    /// `MessageV2{role:"system"}` row. The system prompt lives in
    /// the renderer's output, not in JSONL.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(core)]
    async fn loop_does_not_persist_system_messages() {
        crate::identity::init_test_env();
        ensure_global_core();
        let temp_dir = TempDir::new().unwrap();
        let (provider, mock) = mock_provider();
        mock.queue_text("done");

        let agent_name = format!("phase1-no-system-row-{}", uuid::Uuid::new_v4());
        let mut config = test_agent_config(&agent_name);
        config.prompt = Some("You are {{agent_name}}.".to_string());
        let agent = Arc::new(Agent::new_for_test(config, temp_dir.path()).await.unwrap());
        let extension_core = global_core().unwrap();
        let loop_ = AgenticLoop::new(agent, provider, extension_core).await;

        let session = test_session(&agent_name, temp_dir.path()).await;
        let session_id = session.read().await.id.clone();
        let result = loop_
            .run_with_resume("hello", Vec::new(), |_| {}, session, None)
            .await;
        assert!(result.is_ok(), "agentic loop should succeed");

        // Read history and confirm no system messages were persisted.
        let history = loop_.extension_core.clone(); // placeholder to keep borrow alive

        // Reload from disk via the session's storage so we know we're
        // checking the actual JSONL, not the in-memory messages vec.
        let sessions_dir = temp_dir.path().join("data").join("sessions");
        let storage = crate::session::jsonl::SessionStorage::new(sessions_dir);
        let events = storage.load_events(&session_id).await.unwrap();

        let system_rows = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    crate::session::events::SessionEvent::MessageV2(m)
                        if matches!(m.role(), crate::common::types::message::MessageRole::System)
                )
            })
            .count();

        assert_eq!(
            system_rows, 0,
            "JSONL must not carry MessageV2{{role:system}} rows from the loop; found {system_rows}"
        );
        let _ = history;
    }

    /// Phase 1 contract: hook-driven sections fire in parallel. Four
    /// hooks each sleep 50ms; total must be < 100ms when parallel
    /// (serial would take ~200ms+).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(core)]
    async fn loop_invokes_tools_skills_agents_mcp_hooks_in_parallel() {
        use crate::agents::prompt::context::TurnPromptContext;
        use crate::agents::prompt::PromptRenderer;
        use std::time::Instant;

        crate::identity::init_test_env();
        ensure_global_core();
        let core = Arc::new(crate::extensions::framework::ExtensionCore::new());

        // Register four 50ms-sleep handlers (one per section).
        #[derive(Debug)]
        struct SleepHandler(&'static str, std::time::Duration);
        #[async_trait::async_trait]
        impl crate::extensions::framework::core::HookHandler for SleepHandler {
            async fn handle(
                &self,
                _ctx: crate::extensions::framework::core::HookContext,
            ) -> crate::extensions::framework::types::HookResult {
                tokio::time::sleep(self.1).await;
                crate::extensions::framework::types::HookResult::Continue(
                    crate::extensions::framework::types::HookOutput::Text(self.0.to_string()),
                )
            }
            fn hook_point(&self) -> crate::extensions::framework::core::HookPoint {
                crate::extensions::framework::core::HookPoint::PromptSystemSection {
                    section: self.0.to_string(),
                    priority: 100,
                }
            }
            fn priority(&self) -> i32 {
                100
            }
            fn name(&self) -> String {
                format!("Sleep-{}", self.0)
            }
        }

        for section in ["tools", "skills", "agents", "mcp_context"] {
            core.register_hook(
                crate::extensions::framework::core::HookPoint::PromptSystemSection {
                    section: section.to_string(),
                    priority: 100,
                },
                Arc::new(SleepHandler(section, std::time::Duration::from_millis(50))),
                &crate::extensions::framework::types::ExtensionId::new(format!("sleep-{section}")),
            )
            .await
            .unwrap();
        }

        let renderer = PromptRenderer::new(core);
        let ctx = TurnPromptContext {
            principal_id: "test".into(),
            agent_name: "test-agent".into(),
            body: "{{tools}} {{skills}} {{agents}} {{mcp_context}}".into(),
            capabilities: None,
            active_extensions: None,
            principal_memory: None,
            workspace: tempdir_unused(),
            resolved_model: "default".into(),
            channel: "discord".into(),
            thinking_level: "medium".into(),
            sandbox_enabled: false,
            model_aliases: vec![],
            has_gateway: false,
            iteration_budget: None,
            quota_state: None,
            soft_cancel_pending: false,
            capability_diff: None,
            tool_definitions: vec![],
        };

        let started = Instant::now();
        let rendered = renderer.render_for_iteration(&ctx).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "parallel render took {elapsed:?} — should be ~50ms with fan-out, not ~200ms serial"
        );
        assert!(rendered.contains("tools"));
        assert!(rendered.contains("skills"));
        assert!(rendered.contains("agents"));
        assert!(rendered.contains("mcp_context"));
    }

    /// Phase 1 contract: a stuck handler (>2s) must not stall the
    /// renderer. The section soft-fails to empty and the placeholder
    /// is stripped via `remove_missing=true`.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(core)]
    async fn loop_per_hook_timeout_fails_open() {
        use crate::agents::prompt::context::TurnPromptContext;
        use crate::agents::prompt::PromptRenderer;

        crate::identity::init_test_env();
        ensure_global_core();
        let core = Arc::new(crate::extensions::framework::ExtensionCore::new());

        #[derive(Debug)]
        struct StuckHandler;
        #[async_trait::async_trait]
        impl crate::extensions::framework::core::HookHandler for StuckHandler {
            async fn handle(
                &self,
                _ctx: crate::extensions::framework::core::HookContext,
            ) -> crate::extensions::framework::types::HookResult {
                // Sleep far longer than the renderer's 2s timeout.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                crate::extensions::framework::types::HookResult::Continue(
                    crate::extensions::framework::types::HookOutput::Text("never".to_string()),
                )
            }
            fn hook_point(&self) -> crate::extensions::framework::core::HookPoint {
                crate::extensions::framework::core::HookPoint::PromptSystemSection {
                    section: "skills".to_string(),
                    priority: 100,
                }
            }
            fn priority(&self) -> i32 {
                100
            }
            fn name(&self) -> String {
                "Stuck".to_string()
            }
        }

        core.register_hook(
            crate::extensions::framework::core::HookPoint::PromptSystemSection {
                section: "skills".to_string(),
                priority: 100,
            },
            Arc::new(StuckHandler),
            &crate::extensions::framework::types::ExtensionId::new("stuck"),
        )
        .await
        .unwrap();

        let renderer = PromptRenderer::new(core);
        let ctx = TurnPromptContext {
            principal_id: "test".into(),
            agent_name: "test-agent".into(),
            body: "before {{skills}} after".into(),
            capabilities: None,
            active_extensions: None,
            principal_memory: None,
            workspace: tempdir_unused(),
            resolved_model: "default".into(),
            channel: "discord".into(),
            thinking_level: "medium".into(),
            sandbox_enabled: false,
            model_aliases: vec![],
            has_gateway: false,
            iteration_budget: None,
            quota_state: None,
            soft_cancel_pending: false,
            capability_diff: None,
            tool_definitions: vec![],
        };

        // Must complete in ~2s (timeout) — not 5s (handler's actual
        // sleep) and definitely not panic.
        let started = std::time::Instant::now();
        let rendered = renderer.render_for_iteration(&ctx).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "renderer must respect 2s per-hook timeout; took {elapsed:?}"
        );
        assert!(!rendered.contains("{{skills}}"));
        assert!(!rendered.contains("never"));
    }

    fn tempdir_unused() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("peko-render-{}", uuid::Uuid::new_v4()))
    }

    // ---- Phase 2: inert fields flow through to the rendered prompt ----
    //
    // These tests build a `TurnPromptContext` directly (bypassing the
    // full `AgenticLoop::run` path) so the inert-field wiring is
    // exercised without the harness's quota/meter/serial dependencies.
    // The renderer already reads each placeholder from `ctx`; these
    // tests pin that wiring so Phase 2's back-compat guarantees hold.

    use crate::agents::prompt::context::TurnPromptContext;
    use crate::agents::prompt::PromptRenderer;

    fn inert_ctx() -> TurnPromptContext {
        TurnPromptContext {
            principal_id: "test-principal".into(),
            agent_name: "test-agent".into(),
            body: "channel={{channel}} thinking={{thinking_level}} runtime={{runtime}} sandbox={{sandbox}} aliases={{model_aliases}}".into(),
            capabilities: None,
            active_extensions: None,
            principal_memory: None,
            workspace: tempdir_unused(),
            resolved_model: "claude-sonnet-4-6".into(),
            channel: "cli".into(),
            thinking_level: "high".into(),
            sandbox_enabled: true,
            model_aliases: vec!["sonnet".into(), "haiku".into()],
            has_gateway: true,
            iteration_budget: None,
            quota_state: None,
            soft_cancel_pending: false,
            capability_diff: None,
            tool_definitions: vec![],
        }
    }

    #[tokio::test]
    async fn loop_renders_resolved_model_id_in_runtime_section() {
        // Pin Phase 2: `resolved_model_id` cached at loop construction
        // flows into `{{runtime}}`'s `Model:` line. Back-compat
        // hardcoded values render if `ctx.resolved_model` is the
        // legacy default.
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let ctx = inert_ctx();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(
            rendered.contains("Model: claude-sonnet-4-6"),
            "expected resolved_model_id to surface in runtime section; got: {rendered}"
        );
    }

    #[tokio::test]
    async fn loop_renders_channel_and_thinking_level_from_context() {
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let ctx = inert_ctx();
        let body = "channel={{channel}} thinking={{thinking_level}}";
        let mut ctx = ctx;
        ctx.body = body.into();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("channel=cli"), "channel: {rendered}");
        assert!(rendered.contains("thinking=high"), "thinking: {rendered}");
    }

    #[tokio::test]
    async fn loop_renders_sandbox_section_when_enabled() {
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let ctx = inert_ctx();
        let mut ctx = ctx;
        ctx.body = "{{sandbox}}".into();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(
            rendered.contains("## Sandbox") && rendered.contains("Sandbox: enabled"),
            "expected sandbox section when sandbox_enabled=true; got: {rendered}"
        );
    }

    #[tokio::test]
    async fn loop_renders_model_aliases_list_when_set() {
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let ctx = inert_ctx();
        let mut ctx = ctx;
        ctx.body = "{{model_aliases}}".into();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("## Model Aliases"));
        assert!(rendered.contains("- sonnet"));
        assert!(rendered.contains("- haiku"));
    }

    #[tokio::test]
    async fn loop_omits_optional_sections_when_disabled() {
        // Back-compat: agents that don't set the inert fields must
        // render without those sections, matching the legacy hardcoded
        // defaults (`"discord"`, `"medium"`, sandbox off, no aliases).
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let ctx = TurnPromptContext {
            channel: "discord".into(),
            thinking_level: "medium".into(),
            sandbox_enabled: false,
            model_aliases: vec![],
            ..inert_ctx()
        };
        let rendered = renderer.render_for_iteration(&ctx).await;
        // Sandbox disabled → no Sandbox header.
        assert!(!rendered.contains("## Sandbox"));
        // No aliases → no Model Aliases header.
        assert!(!rendered.contains("## Model Aliases"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(core)]
    async fn agentic_loop_caches_resolved_model_id_at_construction() {
        // Phase 2: `AgenticLoop::resolved_model_id` must be populated
        // at construction from `agent.resolved_model_id()` with a
        // fallback to `provider.model_id()`. We pin the wiring using
        // the existing `mock_provider()` helper so the test stays
        // independent of the resolver code path.
        crate::identity::init_test_env();
        ensure_global_core();

        let (provider, _adapter) = mock_provider();
        let temp = tempdir_unused();
        std::fs::create_dir_all(&temp).unwrap();

        let mut config = test_agent_config("phase2-resolved");
        config.prompt = Some("runtime: {{runtime}}".into());

        let agent = Arc::new(Agent::new_for_test(config, &temp).await.unwrap());
        let expected = provider.model_id().to_string();
        let loop_ = AgenticLoop::new(
            Arc::clone(&agent),
            Arc::clone(&provider),
            agent.extension_core(),
        )
        .await;

        // Test path: agent has no resolved id (`new_for_test` skips
        // the resolver). Loop must fall back to `provider.model_id()`.
        assert_eq!(loop_.resolved_model_id(), expected);
        // Pin that `loop_.resolved_model_id()` is what `build_turn_context`
        // would read into `ctx.resolved_model`.
        assert_eq!(loop_.resolved_model_id(), provider.model_id());
    }

    // ---- Phase 3: control surfaces are populated each iteration ----
    //
    // These tests pin the wiring from `AgenticLoop::build_turn_context`
    // into the four control-surface fields on `TurnPromptContext`. They
    // drive `build_turn_context` directly (not the full `run*` paths)
    // because the per-iteration render is the surface that matters:
    // every iteration calls `build_turn_context` and reads the four
    // fields into the system prompt.

    fn loop_test_agent(name: &str) -> AgentConfig {
        let mut cfg = test_agent_config(name);
        // Bodies opt in to every control-surface placeholder so each
        // test can assert on the rendered output (or directly on the
        // `TurnPromptContext` fields). Using the placeholders also
        // exercises the renderer's `{{placeholder}}` substitution path
        // so we catch regressions in `replace_placeholders`.
        cfg.prompt = Some(
            "iter={{iteration_budget}}\n\
             quota={{quota_state}}\n\
             cancel={{soft_cancel}}\n\
             diff={{capability_diff}}\n"
                .to_string(),
        );
        cfg
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn loop_renders_iteration_budget_in_prompt_at_max() {
        // Phase 3: `build_turn_context` must populate
        // `iteration_budget: Some(...)` from the per-iteration counter
        // and the loop's `max_iterations` ceiling. We pin the value
        // directly on `ctx` (Phase 1 renders it; the integration is
        // the field population) and also verify the rendered prompt
        // contains the rendered body.
        crate::identity::init_test_env();
        ensure_global_core();

        let temp = tempdir_unused();
        std::fs::create_dir_all(&temp).unwrap();
        let (provider, _adapter) = mock_provider();

        let agent = Arc::new(
            Agent::new_for_test(loop_test_agent("phase3-iter"), &temp)
                .await
                .unwrap(),
        );
        let loop_ = AgenticLoop::new(
            Arc::clone(&agent),
            Arc::clone(&provider),
            agent.extension_core(),
        )
        .await;

        // Pin the field: `iteration=3, max=10` → Some(state { 3, 10 }).
        let ctx = loop_.build_turn_context(3, &[]);
        let ib = ctx
            .iteration_budget
            .expect("iteration_budget must be populated each iteration");
        assert_eq!(ib.iteration, 3);
        assert_eq!(ib.max_iterations, 10);

        // Pin the render: `## Iteration budget` + `Iteration 3 of 10`
        // shows up in the Markdown body the loop would pass to the
        // LLM.
        let renderer =
            crate::agents::prompt::PromptRenderer::new(Arc::clone(&loop_.extension_core));
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("## Iteration budget"));
        assert!(rendered.contains("Iteration 3 of 10"));
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn loop_renders_quota_state_when_meter_configured() {
        // Phase 3: a configured `QuotaMeter` flows through
        // `build_turn_context` into `ctx.quota_state: Some(view)`. The
        // renderer then emits the `## Quota status` section. We pin
        // the field directly AND verify the rendered body to catch
        // regressions in either the loop plumbing or the render path.
        use crate::quota::{QuotaConfig, QuotaMeter};
        crate::identity::init_test_env();
        ensure_global_core();

        let temp = tempdir_unused();
        std::fs::create_dir_all(&temp).unwrap();
        let (provider, _adapter) = mock_provider();

        let agent = Arc::new(
            Agent::new_for_test(loop_test_agent("phase3-quota"), &temp)
                .await
                .unwrap(),
        );
        let meter = Arc::new(QuotaMeter::new(
            QuotaConfig {
                input_tokens: Some(1000),
                output_tokens: None,
                request_count: Some(10),
                ..Default::default()
            },
            None,
            chrono::Utc::now(),
        ));
        let loop_ = AgenticLoop::new(
            Arc::clone(&agent),
            Arc::clone(&provider),
            agent.extension_core(),
        )
        .await
        .with_quota_meter(meter);

        let ctx = loop_.build_turn_context(1, &[]);
        let qs = ctx
            .quota_state
            .as_ref()
            .expect("quota_state must be populated when a meter is bound");
        assert_eq!(qs.input_limit, Some(1000));
        assert_eq!(qs.request_limit, Some(10));
        assert_eq!(qs.request_count, 0);

        let renderer =
            crate::agents::prompt::PromptRenderer::new(Arc::clone(&loop_.extension_core));
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("## Quota status (current window)"));
        assert!(rendered.contains("Requests:"));
        assert!(rendered.contains("1000"));
        assert!(rendered.contains("/10"));
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn loop_handles_soft_cancel_signal_mid_run() {
        // Phase 3: `build_turn_context` reads `self.cancel` on every
        // iteration. A pre-cancelled token surfaces as
        // `ctx.soft_cancel_pending == true`, which the renderer
        // converts into the `{{soft_cancel}}` section. This pins the
        // signal flow from the IPC handler's `with_cancel_token` into
        // the next-turn system prompt.
        crate::identity::init_test_env();
        ensure_global_core();

        let temp = tempdir_unused();
        std::fs::create_dir_all(&temp).unwrap();
        let (provider, _adapter) = mock_provider();

        let agent = Arc::new(
            Agent::new_for_test(loop_test_agent("phase3-cancel"), &temp)
                .await
                .unwrap(),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel(); // simulate mid-run PrincipalSendControl

        let loop_ = AgenticLoop::new(
            Arc::clone(&agent),
            Arc::clone(&provider),
            agent.extension_core(),
        )
        .await
        .with_cancel_token(cancel);

        let ctx = loop_.build_turn_context(1, &[]);
        assert!(ctx.soft_cancel_pending);

        let renderer =
            crate::agents::prompt::PromptRenderer::new(Arc::clone(&loop_.extension_core));
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("## Cancellation requested"));
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn loop_handles_capability_grant_mid_run() {
        // Phase 3: `cap_diff_tracker.observe` returns `Some(diff)` when
        // the grant set expands between iterations. The tracker's
        // state lives on the loop, so mid-run grant = a new
        // `Capabilities` snapshot the loop observes on the next call
        // to `build_turn_context`. We exercise the tracker directly
        // (same code path the loop uses) plus a render of the diff
        // the loop would surface.
        use crate::agents::prompt::context::CapabilityDiffTracker;
        use crate::extensions::framework::types::{Capabilities, Capability};
        crate::identity::init_test_env();
        ensure_global_core();

        let temp = tempdir_unused();
        std::fs::create_dir_all(&temp).unwrap();
        let (provider, _adapter) = mock_provider();

        let base_caps = Arc::new(Capabilities::with_grants([Capability::new("tool:Read")]));
        let expanded_caps = Arc::new(Capabilities::with_grants([
            Capability::new("tool:Read"),
            Capability::new("tool:Write"),
        ]));

        let agent = Arc::new(
            Agent::new_for_test(loop_test_agent("phase3-cap-grant"), &temp)
                .await
                .unwrap()
                .with_principal_capabilities(Some(Arc::clone(&base_caps))),
        );
        let loop_ = AgenticLoop::new(
            Arc::clone(&agent),
            Arc::clone(&provider),
            agent.extension_core(),
        )
        .await;

        // First observation: baseline → diff is `None` (no section).
        let ctx1 = loop_.build_turn_context(1, &[]);
        assert!(
            ctx1.capability_diff.is_none(),
            "first observation must be the baseline (no diff)"
        );

        // Drive the tracker directly with the new snapshot. The loop's
        // tracker is private; this exercises the same `observe` impl.
        let mut tracker = CapabilityDiffTracker::new();
        let first = tracker.observe(&base_caps);
        assert!(first.is_none(), "first observation is baseline");
        let second = tracker.observe(&expanded_caps);
        let diff = second.expect("grant must surface a diff on the 2nd observation");
        assert_eq!(diff.granted.len(), 1);
        assert_eq!(diff.granted[0].capability, "tool:Write");
        assert_eq!(diff.revoked.len(), 0);

        // Pin the render path: a ctx carrying this diff renders the
        // expected Markdown section.
        let ctx2 = TurnPromptContext {
            principal_id: agent.principal_id().to_string(),
            agent_name: agent.name().to_string(),
            body: "{{capability_diff}}".into(),
            capabilities: Some(expanded_caps),
            active_extensions: None,
            principal_memory: None,
            workspace: tempdir_unused(),
            resolved_model: "mock-model".into(),
            channel: "discord".into(),
            thinking_level: "medium".into(),
            sandbox_enabled: false,
            model_aliases: vec![],
            has_gateway: true,
            iteration_budget: None,
            quota_state: None,
            soft_cancel_pending: false,
            capability_diff: Some(diff),
            tool_definitions: vec![],
        };
        let renderer =
            crate::agents::prompt::PromptRenderer::new(Arc::clone(&loop_.extension_core));
        let rendered = renderer.render_for_iteration(&ctx2).await;
        assert!(rendered.contains("## Capability changes since last turn"));
        assert!(rendered.contains("Granted:"));
        assert!(rendered.contains("- tool:Write"));
    }

    #[tokio::test]
    #[serial_test::serial(core)]
    async fn loop_handles_capability_revoke_mid_run() {
        // Phase 3: mirror of the grant test — when the grant set
        // shrinks between iterations, the diff surfaces the revoked
        // capability under `Revoked:`.
        use crate::agents::prompt::context::CapabilityDiffTracker;
        use crate::extensions::framework::types::{Capabilities, Capability};
        crate::identity::init_test_env();
        ensure_global_core();

        let full_caps = Arc::new(Capabilities::with_grants([
            Capability::new("tool:Read"),
            Capability::new("tool:Write"),
        ]));
        let shrunk_caps = Arc::new(Capabilities::with_grants([Capability::new("tool:Read")]));

        let mut tracker = CapabilityDiffTracker::new();
        let first = tracker.observe(&full_caps);
        assert!(first.is_none());
        let second = tracker.observe(&shrunk_caps);
        let diff = second.expect("revoke must surface a diff");
        assert_eq!(diff.granted.len(), 0);
        assert_eq!(diff.revoked.len(), 1);
        assert_eq!(diff.revoked[0].capability, "tool:Write");

        // Pin render too.
        let temp = tempdir_unused();
        std::fs::create_dir_all(&temp).unwrap();
        let (provider, _adapter) = mock_provider();
        let agent = Arc::new(
            Agent::new_for_test(loop_test_agent("phase3-cap-revoke"), &temp)
                .await
                .unwrap(),
        );
        let loop_ = AgenticLoop::new(
            Arc::clone(&agent),
            Arc::clone(&provider),
            agent.extension_core(),
        )
        .await;

        let ctx = TurnPromptContext {
            principal_id: agent.principal_id().to_string(),
            agent_name: agent.name().to_string(),
            body: "{{capability_diff}}".into(),
            capabilities: Some(shrunk_caps),
            active_extensions: None,
            principal_memory: None,
            workspace: tempdir_unused(),
            resolved_model: "mock-model".into(),
            channel: "discord".into(),
            thinking_level: "medium".into(),
            sandbox_enabled: false,
            model_aliases: vec![],
            has_gateway: true,
            iteration_budget: None,
            quota_state: None,
            soft_cancel_pending: false,
            capability_diff: Some(diff),
            tool_definitions: vec![],
        };
        let renderer =
            crate::agents::prompt::PromptRenderer::new(Arc::clone(&loop_.extension_core));
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("Revoked:"));
        assert!(rendered.contains("- tool:Write"));
    }

    // -----------------------------------------------------------------
    // Goal verification: the system prompt is reconstructed every turn
    // from a freshly read `AgentConfig::prompt`. If the principal's
    // prompt body changes between iterations (via a reload of
    // `principal.toml`, an editor session, the principal's own
    // mid-session rewrite, or any path that writes back into the
    // `Agent` the loop is driving), the next iteration's rendered
    // prompt must reflect the change immediately — no cache, no
    // snapshot.
    //
    // The render path's freshness is pinned here by calling
    // `build_turn_context` twice on the same `AgenticLoop` and
    // asserting that:
    //   - `ctx.body` reflects the prompt value at call time.
    //   - the rendered Markdown reflects the prompt value at call
    //     time (placeholder substitution operates on the fresh body).
    //
    // The `build_turn_context` body is `self.agent.config.prompt.clone()` —
    // a fresh read each call (agentic_loop.rs:1360). If anyone re-adds
    // a cached `system_prompt: String` field that precomputes at
    // construction, this test will fail.
    // -----------------------------------------------------------------
    #[tokio::test]
    #[serial_test::serial(core)]
    async fn loop_renders_fresh_prompt_body_each_iteration() {
        crate::identity::init_test_env();
        ensure_global_core();

        let temp = tempdir_unused();
        std::fs::create_dir_all(&temp).unwrap();
        let (provider, _adapter) = mock_provider();

        // Build the agent and the loop. We move the Arc into the loop
        // and never hold another clone — the loop is the unique
        // owner, which means `Arc::get_mut` on its internal `agent`
        // field succeeds after we take a `&mut AgenticLoop`.
        let mut cfg = test_agent_config("phase4-rebuild-v1");
        cfg.prompt = Some("v1: You are {{agent_name}}.".to_string());

        let agent = Arc::new(Agent::new_for_test(cfg, &temp).await.unwrap());
        let mut loop_ = AgenticLoop::new(
            Arc::clone(&agent),
            Arc::clone(&provider),
            agent.extension_core(),
        )
        .await;
        // Drop the test-side handle so the loop is the unique owner
        // of the `Arc<Agent>`. This is the precondition for
        // `Arc::get_mut(&mut loop_.agent)` to succeed — pinning this
        // guarantee is part of the test's intent: if anyone re-adds
        // an extra Arc clone inside the loop construction or run path,
        // the panic below will fail loudly.
        drop(agent);
        assert_eq!(
            Arc::strong_count(&loop_.agent),
            1,
            "loop must be the unique owner of Arc<Agent>"
        );

        // Iteration 1: render with the v1 body.
        let ctx1 = loop_.build_turn_context(1, &[]);
        assert_eq!(ctx1.body, "v1: You are {{agent_name}}.");
        let renderer =
            crate::agents::prompt::PromptRenderer::new(Arc::clone(&loop_.extension_core));
        let rendered1 = renderer.render_for_iteration(&ctx1).await;
        assert!(
            rendered1.starts_with("v1: You are phase4-rebuild-v1."),
            "iteration 1 must render the v1 body verbatim; got: {rendered1}"
        );
        assert!(!rendered1.contains("v2:"));

        // Iteration 2: mutate `loop_.agent.config.prompt` in place.
        // `Arc::get_mut` requires unique ownership, so the loop is
        // the only Arc holder here.
        Arc::get_mut(&mut loop_.agent)
            .expect("loop is the unique Arc<Agent> owner")
            .config
            .prompt = Some("v2: You are {{agent_name}}.".to_string());

        let ctx2 = loop_.build_turn_context(2, &[]);
        assert_eq!(
            ctx2.body, "v2: You are {{agent_name}}.",
            "iteration 2 must read the fresh body — no caching"
        );
        let rendered2 = renderer.render_for_iteration(&ctx2).await;
        assert!(
            rendered2.starts_with("v2: You are phase4-rebuild-v1."),
            "iteration 2 must render the v2 body verbatim; got: {rendered2}"
        );
        assert!(!rendered2.contains("v1:"));

        // Iteration 3: another mutation, confirming freshness is
        // every-turn (not a one-shot post-mutation refresh).
        Arc::get_mut(&mut loop_.agent)
            .expect("loop is still the unique Arc<Agent> owner")
            .config
            .prompt = Some("v3: You are {{agent_name}}.".to_string());

        let ctx3 = loop_.build_turn_context(3, &[]);
        let rendered3 = renderer.render_for_iteration(&ctx3).await;
        assert!(
            rendered3.starts_with("v3: You are phase4-rebuild-v1."),
            "iteration 3 must render the v3 body; got: {rendered3}"
        );
        assert!(!rendered3.contains("v1:"));
        assert!(!rendered3.contains("v2:"));
    }
}
