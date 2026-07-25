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

use crate::synthetic_stream::synthesize_stream_from_blocking;
use crate::{
    load_principal_memory, AgentView, AgenticError, AgenticEvent, AsyncInboxItem, AsyncInboxLike,
    BackgroundCompactorFactory, CapabilityDiffTracker, CompactionConfig, IterationBudgetState,
    LifecyclePhase, OrchestratorConfig, PromptRenderer, ProviderView, QuotaStateView, SessionView,
    StackedMeteredProvider, StreamOrchestrator, TurnPromptContext,
};
use anyhow::Result;
use futures::StreamExt;
use peko_extension_host::ToolFunnel;
use peko_message::{ContentBlock, LlmMessage};
use peko_provider_api::{
    clamp_openai_prompt_cache_key, CacheRetention, ChatOptions, MessageRole, RetryableError,
    StopReason, TokenUsage, ToolDefinition, DEFAULT_MAX_OUTPUT_TOKENS,
};
use peko_quota::QuotaScope;
use peko_tools_core::HOOK_TIMEOUT;
use std::sync::Arc;
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
    /// Phase 9b.N.5b.1: lifted from `Arc<Agent>` to `Arc<dyn
    /// AgentView>` so the loop never holds the root-only `Agent`
    /// type directly. `AgentView` (defined in
    /// `peko_engine::agent_view`) exposes only the methods/fields the
    /// loop reads. The trait impl lives at
    /// `src/engine/agent_view_compat.rs` (orphan-rule-friendly).
    pub agent: Arc<dyn AgentView>,
    /// Phase 9b.N.5b.9: switched from `Arc<crate::providers::Provider>`
    /// to `Arc<dyn ProviderView>` so the loop holds the trait-object
    /// surface the engine crate owns (the concrete `Provider` impl
    /// remains root-only until a future `peko-providers` phase).
    /// `ProviderView` impl at `src/engine/provider_view_compat.rs`
    /// wraps the same `Provider`; callers passing the concrete root
    /// type get an implicit `Unsize<Arc<dyn ProviderView>>` coercion.
    provider: Arc<dyn ProviderView>,
    /// Phase 9b.N.5b.7: factory that builds a fresh `Box<dyn CompactorBackend>`
    /// for every `run_inner_with_meter` invocation. Replaces the line-892
    /// direct construction of `crate::session::compaction::background::BackgroundCompactor::new(...)`.
    /// The factory trait lives at
    /// `peko_engine::compaction::factory`; the root impl captures the
    /// inner `Arc<Provider>` at factory construction time. Default value
    /// is built inside `new` from the concrete `Provider` parameter via
    /// `BackgroundCompactorFactoryAdapter`.
    compactor_factory: Arc<dyn BackgroundCompactorFactory>,
    max_iterations: usize,
    system_prompt: String,
    /// Extension core for skill loading, tool registration, and hook
    /// firing. Phase 9b.N.5b.4 switched the field from the concrete
    /// root `Arc<crate::extensions::framework::ExtensionCore>` to
    /// `Arc<dyn ToolFunnel>` — the trait port the renderer, tool
    /// executor, and compaction orchestrator all use. The renderer
    /// (now in `peko_engine::prompt::renderer`) calls
    /// `ToolFunnel::invoke_prompt_section_hook` /
    /// `invoke_session_context_build_hook` instead of constructing
    /// `HookPoint` / `HookInput` directly, so the loop never needs
    /// the concrete root type.
    pub extension_core: Arc<dyn ToolFunnel>,
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
    quota_meter: Arc<peko_quota::QuotaMeter>,
    /// F20: per-peer quota meter (channel that triggered the LLM
    /// call — pekohub user sub, API key id, "local"). `None` for
    /// callers that don't have a peer attribution (legacy tests,
    /// stat init paths). When `Some`, `run_inner` opens a nested
    /// `QuotaScope::with(peer, ...)` INSIDE the principal scope, so
    /// every LLM call charges BOTH meters via
    /// [`StackedMeteredProvider`]. Peer trip fires first
    /// (innermost-first); principal only sees a charge if peer
    /// accepted.
    peer_meter: Option<Arc<peko_quota::QuotaMeter>>,
    /// F31b: per-iteration streaming retry budget. Mirrors codex
    /// `run_sampling_request`'s `stream_max_retries` (turn.rs:1123-1218).
    /// On a retryable mid-stream error (transient 5xx, timeout,
    /// connection reset — see `RetryableError::is_retryable` in
    /// `providers/transport/retry.rs`), the loop sleeps the
    /// computed-or-server-suggested delay and re-issues the request
    /// with the same `messages` checkpoint (the `original_input`
    /// save/restore shape). Default 3 (matches codex); set to 0
    /// via `with_stream_max_retries(0)` to disable.
    stream_max_retries: u32,
    /// Per-session queue of completed async tasks, drained at the start
    /// of `run_inner` iteration. Surfaced to the LLM as a
    /// synthetic user-role message containing all queued completions.
    ///
    /// Trait-object view of root's `SharedSessionInbox` via the
    /// [`AsyncInboxAdapter`] compat shim — Phase 9b.N.5b.2 lifted the
    /// `InboxItem` + `SessionInbox` coupling so the engine crate
    /// never imports `completion_queue`. Callers wrap the inbox in
    /// `Arc::new(AsyncInboxAdapter::new(...))` before calling
    /// [`AgenticLoop::with_async_completion_queue`].
    async_completion_queue: Option<Arc<dyn AsyncInboxLike>>,
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
    /// Phase 9b.N.5b.9c: compaction config (thresholds, model
    /// override, retention policy). Passed in by the caller — root
    /// loads it from `~/.peko/config.toml` via
    /// `crate::session::compaction::load_compaction_config()` and
    /// the loop never imports `dirs` / `toml`. `peko_engine` can't
    /// own the loader because it doesn't depend on those crates.
    /// Cloned into `CompactionOrchestrator::new` at the start of
    /// every `run_inner_with_meter` invocation (line 940 in the
    /// pre-9c version).
    compaction_config: CompactionConfig,
}

impl AgenticLoop {
    /// Create a new agentic loop
    ///
    /// # Arguments
    /// * `agent` - The agent configuration (trait-object view of root's
    ///   `Agent`; see `peko_engine::AgentView`).
    /// * `provider` - The LLM provider to use
    /// * `extension_core` - The trait-object view of the extension host
    ///   (`peko_extension_host::ToolFunnel`). Phase 9b.N.5b.4 switched
    ///   the constructor param to `Arc<dyn ToolFunnel>` — the concrete
    ///   `ExtensionCore` still implements `ToolFunnel` via the
    ///   `src/engine/extension_core_funnel_compat.rs` impl, so
    ///   `Arc::new(core) as Arc<dyn ToolFunnel>` works at call sites.
    pub async fn new(
        agent: Arc<dyn AgentView>,
        provider: Arc<dyn ProviderView>,
        extension_core: Arc<dyn ToolFunnel>,
        compactor_factory: Arc<dyn BackgroundCompactorFactory>,
        compaction_config: CompactionConfig,
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
            compactor_factory,
            max_iterations: 10,
            system_prompt: placeholder_prompt,
            extension_core,
            caller_id: None,
            agent_principal_id,
            async_completion_queue: None,
            cap_diff_tracker: std::sync::Mutex::new(CapabilityDiffTracker::new()),
            cancel: None,
            quota_meter: Arc::new(peko_quota::QuotaMeter::unlimited()),
            peer_meter: None,
            // F31b: default to 3 streaming retries per iteration,
            // matching the HTTP-layer `RetryPolicy::default()`.
            stream_max_retries: 3,
            resolved_model_id,
            // F23: cache-stable prefix starts un-rendered. The first
            // iteration of `run_inner_with_meter` triggers
            // `render_cache_stable` via the `cache_stable_prompt`
            // access in the messages[0] rebuild block.
            cache_stable_prompt: std::sync::Mutex::new(None),
            // Phase 9b.N.5b.9c: caller supplies the config. Root
            // loads it from `~/.peko/config.toml`; tests pass
            // `CompactionConfig::default()`.
            compaction_config,
        }
    }

    /// Phase 9b.N.5b.7: replace the constructor-supplied
    /// `BackgroundCompactorFactory` with a custom implementation. Used
    /// by callers that want a different compactor backend, or by tests
    /// that inject a mock factory after construction.
    ///
    /// Phase 9b.N.5b.9c: the constructor gained a required
    /// `compactor_factory` parameter; this builder is now the only
    /// way to swap the factory after `new`. The
    /// `BackgroundCompactorFactoryAdapter` (root-side
    /// `src/engine/background_compactor_factory_compat.rs`) is the
    /// canonical factory for production callers.
    #[must_use]
    pub fn with_provider_factory(mut self, factory: Arc<dyn BackgroundCompactorFactory>) -> Self {
        self.compactor_factory = factory;
        self
    }

    /// F19: bind a per-principal quota meter. The loop opens a
    /// `QuotaScope::with` around `run_inner` so every LLM call
    /// routed through this loop auto-charges via `MeteredProvider`.
    /// For unquota'd principals (or test fixtures that don't bind
    /// a meter), the unlimited default returned by `new` is
    /// sufficient and this method can be skipped.
    #[must_use]
    pub fn with_quota_meter(mut self, meter: Arc<peko_quota::QuotaMeter>) -> Self {
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
    pub fn with_peer_meter(mut self, meter: Option<Arc<peko_quota::QuotaMeter>>) -> Self {
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
    pub fn with_async_completion_queue(mut self, queue: Arc<dyn AsyncInboxLike>) -> Self {
        self.async_completion_queue = Some(queue);
        self
    }

    /// Set maximum iterations
    #[must_use]
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// F31b: set the per-iteration streaming-retry budget.
    ///
    /// `0` disables mid-stream retry (preserves the pre-F31b behavior
    /// of `return Err(e)` on the first transient failure). The
    /// default of 3 matches `RetryPolicy::default()` and codex
    /// `run_sampling_request`'s `stream_max_retries`.
    #[must_use]
    pub fn with_stream_max_retries(mut self, max: u32) -> Self {
        self.stream_max_retries = max;
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
    pub fn extension_core(&self) -> &dyn ToolFunnel {
        // Trait-object view of the concrete `Arc<ExtensionCore>` so
        // call-sites outside this crate can hold the port without
        // importing root's `ExtensionCore` type. Phase 9b.N.5b.3
        // returns `&dyn ToolFunnel` rather than `&Arc<ExtensionCore>`
        // so future dependents (after the loop lifts into
        // `peko-engine`) only depend on the trait, not the root
        // extension host.
        &*self.extension_core
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
        session: &dyn SessionView,
        history: Option<Vec<LlmMessage>>,
    ) -> Result<AgenticResult> {
        let config = OrchestratorConfig::final_only();
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
        session: &dyn SessionView,
        history: Option<Vec<LlmMessage>>,
        streaming_config: OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        let session_id = session.id().await;
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
        // Phase 9b.N.5b.9b: route through `SessionView::add_user` so the
        // write lock is acquired inside the trait impl, not here.
        session.add_user(user_text.to_string()).await?;

        // Continue with the unified run logic
        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }

    /// F28: rich-input overload of [`Self::run_streaming_with_resume`].
    /// Accepts a fully-formed `LlmMessage` so callers (e.g. MCP
    /// sampling) can attach multimodal `ContentBlock::Image` blocks
    /// to the user turn.
    ///
    /// The session JSONL stores text-only user messages (the on-disk
    /// shape didn't change in F28). The persisted text is the joined
    /// text content of the rich message; non-text blocks drop on the
    /// session-storage floor — the LLM still sees them because we
    /// push the full rich `LlmMessage` onto `messages` for the
    /// request body.
    ///
    /// Use [`Self::run_streaming_with_resume`] when the caller only
    /// has a text prompt — that path stays byte-for-byte equivalent
    /// to the pre-F28 shape (single `LlmMessage::user(text)` turn,
    /// session JSONL carries the text).
    pub async fn run_streaming_with_resume_rich(
        &self,
        user_message: LlmMessage,
        pre_user_messages: Vec<LlmMessage>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        session: &dyn SessionView,
        history: Option<Vec<LlmMessage>>,
        streaming_config: OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        let session_id = session.id().await;
        info!(
            "Starting v4 rich-input streaming agentic loop for agent: {} (session: {})",
            self.agent.name(),
            session_id
        );

        // Phase 1: seed messages the same way as the text-only path.
        let mut messages = if let Some(h) = history {
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

        messages.extend(pre_user_messages);
        messages.push(user_message.clone());

        // Persist only the text portion of the rich user message —
        // session JSONL keeps the pre-F28 text-only shape.
        let persisted_text = user_message
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        {
            // `add_user` already errors on empty input; persist an
            // empty placeholder when an image-only message arrives
            // so the session JSONL still gets the user turn marker.
            // Phase 9b.N.5b.9b: route through `SessionView::add_user`
            // so the write lock is acquired inside the trait impl.
            session
                .add_user(if persisted_text.is_empty() {
                    "[image attached]".to_string()
                } else {
                    persisted_text
                })
                .await?;
        }

        // Drop the unused dummy binding; we already persisted above.
        let _ = session_id;

        // Emit start event now (text-only path emits it earlier in
        // `run_streaming_with_resume`; we delay it here so we don't
        // emit twice when the text-only caller also calls this path
        // by accident). For consistency with the unified
        // orchestrator contract, emit start here too.
        on_event(AgenticEvent::Lifecycle {
            run_id: run_id.clone(),
            phase: LifecyclePhase::Start,
            error: None,
        });

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
        session: &dyn SessionView,
        history: Option<Vec<LlmMessage>>,
        streaming_config: OrchestratorConfig,
    ) -> Result<AgenticResult> {
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());

        let session_id = session.id().await;
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

    /// F31x: fire the `Stop` hook at every loop-exit site.
    ///
    /// Observe-only — the loop's return value is unaffected by handler
    /// output. The `payload` object is forwarded via `HookInput::Json`
    /// so handlers can pattern-match on `reason` (`"end"`,
    /// `"interrupted"`, `"max_iterations"`) and the iteration count.
    /// `agent_name` + `agent_did` are folded into the same payload
    /// so the `AfterAgent` handler sees the agent identity.
    ///
    /// Wrapped in `tokio::time::timeout(HOOK_TIMEOUT, ...)`; soft-fails
    /// on timeout (matches the `loop_per_hook_timeout_fails_open` test
    /// shape — handlers cannot stall the loop).
    async fn fire_stop_hook(&self, run_id: &str, payload: serde_json::Value) {
        // Merge agent identity into the payload so both `Stop` and
        // `AfterAgent` handlers see `agent_name` + `agent_did`
        // alongside the per-exit-reason fields. Pre-existing Stop
        // handlers that only read `reason` / `iterations` keep
        // working — the new fields are additive.
        let mut merged = payload;
        if let serde_json::Value::Object(ref mut map) = merged {
            map.insert("agent_name".to_string(), self.agent.name().into());
            map.insert(
                "agent_did".to_string(),
                self.agent.identity_did().to_string().into(),
            );
        }

        // `Stop` hook — per-turn exit signal.
        //
        // Phase 9b.N.5b.3 routed the firing through the
        // `ToolFunnel::invoke_stop_hook` trait method. The trait impl
        // lives in `src/engine/extension_core_funnel_compat.rs` and
        // builds `HookInput::Json(merged) + HookPoint::Stop`
        // internally, so this agentic loop no longer touches
        // `HookPoint` or `HookInput` directly at the loop-exit seam.
        // Soft-fails on timeout (the impl wraps `invoke_hook` in
        // `HOOK_TIMEOUT`).
        let funnel = &*self.extension_core;
        funnel.invoke_stop_hook(merged.clone()).await;

        // F31x.1: fire `AfterAgent` alongside `Stop` so the per-turn
        // cleanup hook actually fires every run. `Agent::stop()`
        // still fires `AfterAgent` for the rare long-running-agent
        // case, but the loop-exit site is the natural seam for the
        // stateless-service flow (where agents are cold-started per
        // request and never explicitly stopped). Symmetric with the
        // `Stop` site above — both go through `ToolFunnel` trait
        // methods.
        funnel.invoke_after_agent_hook(merged).await;
        let _ = run_id; // currently unused by handlers; kept on signature for forward-compat
    }

    /// Unified agent loop — always streams internally; delivery mode controls presentation.
    ///
    /// `DeliveryMode::FinalOnly` buffers everything and emits complete events at the end,
    /// giving blocking consumers the same behavior as the old `run_loop`.
    /// `DeliveryMode::Live` emits deltas for real-time display.
    async fn run_inner(
        &self,
        messages: Vec<LlmMessage>,
        session: &dyn SessionView,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        run_id: String,
        streaming_config: OrchestratorConfig,
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
        session: &dyn SessionView,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
        run_id: String,
        streaming_config: OrchestratorConfig,
        provider: StackedMeteredProvider,
    ) -> Result<AgenticResult> {
        // Get session_id once at start
        let session_id = session.id().await;

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
            .set_session_key(self.agent.identity_did(), Some(session_id.clone()))
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

            // Phase 9b.N.5b.9b: route through `SessionView::{set_model,
            // record_model_change}` so the write lock is acquired
            // inside the trait impl, not here.
            session.set_model(&provider_name, &model_name).await;
            // Record model change event in session JSONL for normalization
            if let Err(e) = session
                .record_model_change(&provider_name, &model_name)
                .await
            {
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
        let context_window = if self.agent.has_llm_resolver() {
            provider
                .context_window()
                .map(|n| n as usize)
                .unwrap_or(FALLBACK_CONTEXT_WINDOW_TOKENS)
        } else {
            FALLBACK_CONTEXT_WINDOW_TOKENS
        };
        // Phase 9b.N.5b.7: route the line-892 construction through the
        // `BackgroundCompactorFactory` port instead of naming
        // `crate::session::compaction::background::BackgroundCompactor` directly.
        // The default factory (built inside `new`) captures the inner
        // `Arc<Provider>` and rebuilds a fresh `BackgroundCompactor`
        // here with the loop's stored meters.
        let compactor_backend = self
            .compactor_factory
            .build(Arc::clone(&self.quota_meter), self.peer_meter.clone());
        // Phase 9b.N.5b.9c: compaction config comes from the loop's
        // stored field (loaded by root at construction time and passed
        // in via the new `compaction_config` parameter). The loop no
        // longer calls `crate::session::compaction::load_compaction_config()`
        // directly — that loader depends on `dirs` + `toml`, which
        // aren't in `peko-engine`'s dep graph.
        let mut compaction_orchestrator = crate::CompactionOrchestrator::new(
            compactor_backend,
            self.compaction_config.clone(),
            context_window,
        );

        // Propagate the resolved model max into the session so the
        // `session` tool and IPC layer can surface it (used by the
        // CLI dry-run and external status surfaces). The orchestrator
        // pins this same value at run start.
        // Phase 9b.N.5b.9b: route through `SessionView` so the write
        // lock is acquired inside the trait impl.
        session.set_model_context_limit(context_window).await;

        // Initialize tool executor with a fresh per-loop gate. The
        // gate is cloned into each `execute(...)` future via the
        // executor's `Arc` interior, so all parallel calls in a single
        // fan-out share it (F33 — audit section 3 row 3).
        let tool_executor = crate::ToolExecutor::new();

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
                        AsyncInboxItem::Completion(e) => completions.push(e),
                        AsyncInboxItem::Steering(m) => steering.push(m),
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
                    // F31x: Stop hook observe-only — soft-interrupt
                    // signal carries `reason: "interrupted"`.
                    self.fire_stop_hook(
                        &run_id,
                        serde_json::json!({ "reason": "interrupted", "iterations": iteration }),
                    )
                    .await;
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
                    session,
                    &*self.extension_core,
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
                // F31a: emit a dedicated phase so IPC consumers can
                // distinguish cap-hit from a clean End. `success: false`
                // surfaces the failure to the caller; the configured
                // ceiling travels on the Lifecycle event's error field
                // and is also folded into the final_answer so the IPC
                // `principal_log` stream-renderer can show it.
                on_event(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::MaxIterations {
                        iterations: self.max_iterations,
                    },
                    error: Some(format!("max_iterations ({})", self.max_iterations)),
                });
                // F31x: Stop hook observe-only — cap-hit signal
                // carries the configured ceiling so handlers can
                // distinguish "user asked for N turns" from other
                // exit reasons.
                self.fire_stop_hook(
                    &run_id,
                    serde_json::json!({
                        "reason": "max_iterations",
                        "iterations": self.max_iterations,
                    }),
                )
                .await;
                return Ok(AgenticResult {
                    success: false,
                    final_answer: format!("Max iterations reached ({})", self.max_iterations),
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
                thinking_effort: peko_provider_api::ThinkingEffort::None,
                thinking_summary: None,
                encrypted_reasoning: false,
                // F26: every new ChatOptions knob is also at its
                // "preserve pre-F26 wire shape" default here, so the
                // engine loop's loopback tests stay green even with
                // extra knobs enabled. Per-adapter emission gates on
                // each knob's "is set" predicate (e.g.
                // `ServiceTier::as_wire_str()` returns `None` for
                // `ServiceTier::None`, so the body suppresses the
                // `service_tier` field entirely).
                tool_choice: peko_provider_api::ToolChoice::Auto,
                parallel_tool_calls: None,
                service_tier: peko_provider_api::ServiceTier::None,
                safety_identifier: None,
                // F27: defaults preserve the pre-F27 Anthropic wire
                // shape — empty `betas`, `beta_api: false`, and
                // `thinking_keep: Off` all suppress emission. Caller-
                // supplied knobs land on `ChatOptions` via IPC once
                // the surface is plumbed.
                betas: Vec::new(),
                beta_api: false,
                thinking_keep: peko_provider_api::ThinkingKeep::Off,
                ..Default::default()
            };

            // Debug: print messages being sent
            debug!("Messages sent to LLM (iteration {}):", iteration);
            for (i, msg) in messages.iter().enumerate() {
                let content_preview: String = msg
                    .content
                    .iter()
                    .map(|b| match b {
                        peko_message::ContentBlock::Text { text } => {
                            format!("[Text: {}]", text.chars().take(50).collect::<String>())
                        }
                        peko_message::ContentBlock::ToolCall { id, name, .. } => {
                            format!("[ToolCall: {name} ({id})]")
                        }
                        peko_message::ContentBlock::ToolResult {
                            tool_call_id, name, ..
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
                // F31c: lift `QuotaError` into `AgenticError::Quota`
                // so callers can match `as_quota()` to render
                // quota-exceeded UX (used / limit / window_end)
                // without string-parsing. `.into()` chains
                // `AgenticError: From<QuotaError>` →
                // `anyhow::Error: From<AgenticError>` (the
                // `#[from]` on the variant feeds the outer
                // `thiserror::Error` derive).
                return Err(crate::AgenticError::from(existing_err).into());
            }
            if let Some(pm) = self.peer_meter.as_ref() {
                pm.advance_if_needed(chrono::Utc::now());
                if let Some(existing_err) = pm.check() {
                    on_event(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Error,
                        error: Some(existing_err.to_string()),
                    });
                    return Err(crate::AgenticError::from(existing_err).into());
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
            //
            // F31b: transient mid-stream errors (5xx, network reset,
            // timeout) ALSO retry — but against a separate budget
            // (`stream_max_retries`, codex `run_sampling_request` shape)
            // and with the same `messages` / `options` snapshot. Both
            // the initial `stream_with_eviction` call (which can fail
            // before any event is emitted) and a mid-stream `Err` from
            // the byte stream fall through the same retry path below.
            let mut stream_attempt: u32 = 0;
            let mut stream = 'stream_retry: loop {
                match self
                    .stream_with_eviction(&provider, &model_id, &messages, &tool_defs, &options)
                    .await
                {
                    Ok(s) => break 'stream_retry s,
                    Err(e) => {
                        // F31b: retry the start-stream call when the
                        // error is transient. `stream_with_eviction`
                        // already handles `ContextWindowExceeded` by
                        // dropping the oldest message and re-issuing;
                        // any other retryable error reaches this branch
                        // and is re-attempted against the same `messages`
                        // snapshot.
                        if stream_attempt < self.stream_max_retries
                            && peko_provider_api::RetryableError::is_retryable(&e)
                        {
                            let retry_after = peko_provider_api::RetryableError::retry_after(&e);
                            let max_delay = std::time::Duration::from_secs(30);
                            let delay =
                                retry_after.map(|d| d.min(max_delay)).unwrap_or_else(|| {
                                    std::time::Duration::from_millis(
                                        1000u64.saturating_mul(2u64.pow(stream_attempt)),
                                    )
                                    .min(max_delay)
                                });
                            let reason_truncated: String =
                                e.to_string().chars().take(256).collect();
                            on_event(AgenticEvent::Retry {
                                run_id: run_id.clone(),
                                iteration,
                                attempt: stream_attempt,
                                max_attempts: self.stream_max_retries,
                                retry_after,
                                delay,
                                reason: reason_truncated,
                            });
                            info!(
                                "Stream-start retry {}/{} after {:?} (reason: {})",
                                stream_attempt + 1,
                                self.stream_max_retries,
                                delay,
                                e
                            );
                            tokio::time::sleep(delay).await;
                            stream_attempt += 1;
                            continue 'stream_retry;
                        }
                        on_event(AgenticEvent::Lifecycle {
                            run_id: run_id.clone(),
                            phase: LifecyclePhase::Error,
                            error: Some(e.to_string()),
                        });
                        return Err(e);
                    }
                }
            };

            info!("Stream started, processing events...");

            // Create orchestrator for this iteration
            let mut orchestrator = StreamOrchestrator::new(&run_id, streaming_config.clone());

            // Process stream events
            let mut accumulated_text = String::new();
            let mut thinking_text = String::new();
            let mut tool_calls: Vec<ContentBlock> = Vec::new();
            let mut stop_reason = StopReason::Stop;
            let mut stream_event_count = 0;

            // F31b: per-iteration streaming retry budget. Mirrors codex
            // `run_sampling_request`'s `stream_max_retries` shape — on a
            // retryable mid-stream error (transient 5xx, timeout, network
            // reset), sleep the computed-or-server-suggested delay and
            // re-issue the request with the same `messages` / `options`
            // checkpoint (the `original_input` save/restore pattern).
            // The `stream_with_eviction` wrapper above handles
            // `ContextWindowExceeded` separately (F22); this layer
            // handles transient transport failures only. The `Err(e)`
            // arm of the inner `stream.next()` loop shares the same
            // budget counter as the start-stream retry above — both are
            // scoped to this iteration.
            'inner_stream: loop {
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
                                    peko_provider_api::StreamEvent::ToolCallEnd {
                                        tool_call,
                                        ..
                                    } => {
                                        tool_calls.push(tool_call);
                                    }
                                    peko_provider_api::StreamEvent::Done {
                                        stop_reason: reason,
                                    } => {
                                        stop_reason = reason;
                                    }
                                    peko_provider_api::StreamEvent::Usage {
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
                                // F31b: mid-stream retry budget. The
                                // `RetryableError::is_retryable()` impl
                                // covers HTTP 429/500/502/503/504/529,
                                // network timeouts, connection resets,
                                // and refused connections. `400`/`413`
                                // are explicitly NOT retryable here —
                                // those surface as `ContextWindowExceeded`
                                // through `stream_with_eviction` (F22),
                                // not at this layer.
                                if stream_attempt < self.stream_max_retries
                                    && peko_provider_api::RetryableError::is_retryable(&e)
                                {
                                    let retry_after =
                                        peko_provider_api::RetryableError::retry_after(&e);
                                    let max_delay = std::time::Duration::from_secs(30);
                                    let delay = retry_after
                                        .map(|d| d.min(max_delay))
                                        .unwrap_or_else(|| {
                                            std::time::Duration::from_millis(
                                                1000u64.saturating_mul(2u64.pow(stream_attempt)),
                                            )
                                            .min(max_delay)
                                        });
                                    let reason_truncated: String =
                                        e.to_string().chars().take(256).collect();
                                    on_event(AgenticEvent::Retry {
                                        run_id: run_id.clone(),
                                        iteration,
                                        attempt: stream_attempt,
                                        max_attempts: self.stream_max_retries,
                                        retry_after,
                                        delay,
                                        reason: reason_truncated,
                                    });
                                    info!(
                                        "Mid-stream retry {}/{} after {:?} (reason: {})",
                                        stream_attempt + 1,
                                        self.stream_max_retries,
                                        delay,
                                        e
                                    );
                                    tokio::time::sleep(delay).await;
                                    stream_attempt += 1;
                                    // Re-issue the stream with the same
                                    // `messages` / `options` checkpoint.
                                    // `stream_with_eviction` handles
                                    // `ContextWindowExceeded` for the
                                    // new attempt; transient 5xx simply
                                    // round-trips again.
                                    match self
                                        .stream_with_eviction(
                                            &provider, &model_id, &messages, &tool_defs, &options,
                                        )
                                        .await
                                    {
                                        Ok(new_stream) => {
                                            stream = new_stream;
                                            continue 'inner_stream;
                                        }
                                        Err(reissue_err) => {
                                            on_event(AgenticEvent::Lifecycle {
                                                run_id: run_id.clone(),
                                                phase: LifecyclePhase::Error,
                                                error: Some(reissue_err.to_string()),
                                            });
                                            return Err(reissue_err);
                                        }
                                    }
                                }
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
                let tool_call_blocks: Vec<peko_message::ToolCallBlock> = tool_calls
                    .iter()
                    .filter_map(|tc| {
                        if let ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } = tc
                        {
                            Some(peko_message::ToolCallBlock {
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
                    Some(peko_message::ThinkingBlock {
                        text: thinking_text.clone(),
                        signature: None,
                    })
                };

                {
                    // Phase 9b.N.5b.9b: route through `SessionView` so
                    // the write lock is acquired inside the trait impl.
                    session
                        .add_assistant_with_blocks(
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
                            // Phase 9b.N.3: route through `&dyn
                            // ToolFunnel`. `&self.extension_core` is
                            // `&Arc<ExtensionCore>`; deref to
                            // `&ExtensionCore` so it coerces to the
                            // trait (impl lives on `ExtensionCore`,
                            // not `Arc<ExtensionCore>`).
                            &*self.extension_core,
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
                            // Phase 9b.N.5b.9e: `session` parameter is
                            // `&dyn SessionView`; forward as-is to the
                            // tool-execution port seam.
                            session,
                            &session_id,
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
                    // F31x: Stop hook observe-only — late soft-interrupt
                    // also fires Stop (handlers can tell apart the
                    // pre-stream vs post-stream variants only by
                    // listening to the Lifecycle::Interrupted event
                    // emitted just above).
                    self.fire_stop_hook(
                        &run_id,
                        serde_json::json!({ "reason": "interrupted", "iterations": iteration }),
                    )
                    .await;
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
            // Phase 9b.N.5b.9b: route through `SessionView` so the
            // write lock is acquired inside the trait impl. The root
            // `Session::add_assistant` takes an `Option<Vec<ToolCall>>`
            // second argument that every loop call site passes `None`;
            // `SessionView::add_assistant` drops the parameter for
            // forward-compatibility with future callers that surface
            // `peko_message::ToolCallInfo` instead.
            session
                .add_assistant(
                    accumulated_text.clone(),
                    None,
                    Some(iteration_usage.clone()),
                )
                .await?;

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

            // F31x: Stop hook observe-only — clean End carries
            // `reason: "end"` and the iteration count so handlers
            // can distinguish a normal completion from cap-hit /
            // soft-interrupt.
            self.fire_stop_hook(
                &run_id,
                serde_json::json!({ "reason": "end", "iterations": iteration }),
            )
            .await;

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
    ///
    /// F35 — when the agent's [`AgentConfig::enable_tool_search`] is true
    /// AND there is at least one `ToolExposure::Deferred` tool visible to
    /// the principal, appends a synthetic `__tool_search` `ToolDefinition`
    /// so the model can resolve deferred tools on demand. Mirrors codex
    /// `tools/spec_plan.rs:928-949 append_tool_search_executor`.
    pub async fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        // The agent carries the principal's capability grant snapshot.
        // If none is present, treat it as an empty grant set (fail-closed).
        let capabilities = self
            .agent
            .principal_capabilities()
            .map(|allowed| allowed.as_ref().clone())
            .unwrap_or_default();
        let active = self.agent.principal_active_extensions();
        let principal_id = peko_subject::PrincipalId(self.agent_principal_id.clone());
        let mut defs = self
            .extension_core
            .list_tool_definitions_with_allowlist(&capabilities, active, &principal_id)
            .await;

        // F35 — append the synthetic `__tool_search` stub only when both
        // gates pass: (a) the agent opted in via
        // `AgentConfig::enable_tool_search`, and (b) at least one
        // `Deferred` tool is registered for this principal. The second
        // gate avoids bloating the catalog when there's nothing to
        // discover — `Deferred` tools aren't visible in
        // `list_tool_definitions_with_allowlist` (F34) so we walk the
        // unfiltered `list_tools` to count them.
        if self.agent.config_enable_tool_search() {
            let has_deferred = self
                .extension_core
                .has_deferred_tools_for(&principal_id)
                .await;
            if has_deferred {
                defs.push(ToolDefinition {
                    name: crate::tool_search_metadata::TOOL_SEARCH_TOOL_NAME.to_string(),
                    description: crate::tool_search_metadata::synthetic_description(),
                    parameters: crate::tool_search_metadata::synthetic_parameters(),
                });
            }
        }

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
    pub fn build_turn_context(
        &self,
        iteration: usize,
        tool_defs: &[ToolDefinition],
    ) -> TurnPromptContext {
        // Body lives on `AgentConfig::prompt` as `Option<String>`. Empty
        // body is supported (renderer falls back to one-line identity).
        let body = self.agent.config_prompt_body().unwrap_or_default();

        // Workspace: the principal's workspace is the canonical answer
        // for any agent spawned under a principal; fall back to a
        // per-agent default for tests / compiled-in agents that bypass
        // the principal path.
        let workspace = self
            .agent
            .principal_workspace()
            .cloned()
            .unwrap_or_else(|| {
                let resolver = peko_extension_host::default_agent_workspace(self.agent.name());
                resolver
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
        let iteration_budget = Some(crate::IterationBudgetState {
            iteration,
            max_iterations: self.max_iterations,
        });

        let quota_state = {
            let snapshot = self.quota_meter.snapshot();
            let config = self.quota_meter.config();
            Some(crate::QuotaStateView {
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
            principal_memory: crate::load_principal_memory(&workspace),
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
    /// via [`peko_engine::compaction::drop_oldest_respecting_pairs`]
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
            Box<dyn futures::Stream<Item = Result<peko_provider_api::StreamEvent>> + Send>,
        >,
    > {
        use crate::compaction::drop_oldest_respecting_pairs;
        use peko_provider_api::is_context_window_exceeded;

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
        session: &dyn SessionView,
        history: Option<Vec<LlmMessage>>,
        streaming_config: OrchestratorConfig,
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
        // Phase 9b.N.5b.9b: route through `SessionView::add_user`.
        session.add_user(user_text.to_string()).await?;

        // Run the streaming loop
        self.run_inner(messages, session, on_event, run_id, streaming_config)
            .await
    }
}

#[cfg(test)]
mod tests {
    // Empty by design.
    //
    // The agentic_loop's test suite references root-only fixture types
    // (`Agent`, `ExtensionCore`, `Subject`, `SessionManager`, `Provider`,
    // `MockAdapter`, `BuiltinToolAdapter`, `LlmResolver`, etc.) that cannot
    // lift into `peko-engine` without violating `check_workspace_deps.py`
    // forbidden-edge rules.
    //
    // The actual tests live at `src/engine/agentic_loop_compat.rs` (root)
    // and exercise `peko_engine::AgenticLoop` via the public API only.
    //
    // Mirrors the precedent set by `crates/engine/src/tool_executor.rs`.
}
