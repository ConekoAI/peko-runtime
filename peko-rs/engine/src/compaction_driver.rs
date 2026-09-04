//! Compaction driver for the agentic loop.
//!
//! Encapsulates the entire compaction lifecycle:
//! - Pre-compaction hook invocation
//! - Background compactor coordination
//! - Post-compaction hook invocation
//! - Session recording and cache updates
//!
//! Phase 9b.N.4 lifted this file from `src/engine/compaction_orchestrator.rs`
//! into `peko-engine`. The lift relied on three trait ports so the
//! driver can talk to root-only types without a direct dependency:
//!
//! - **`ToolFunnel`** (`peko-extension-host`) — abstracted `ExtensionCore`
//!   for hook firing. Three new methods added in 9b.N.4 cover the
//!   compaction / session-state hooks (`invoke_session_compaction_pre_hook`,
//!   `invoke_session_compaction_post_hook`, `invoke_session_state_change_hook`).
//! - **`SessionView`** (`peko-engine`) — extended in 9b.N.4 with
//!   `record_compaction`, `load_previous_compaction_summary`, and
//!   `update_context_cache` for the driver's session writes.
//! - **`CompactorBackend`** (`peko-engine::compaction`) — new in 9b.N.4,
//!   abstracts `BackgroundCompactor` so the driver holds a
//!   `Box<dyn CompactorBackend>` instead of a concrete impl.
//!
//! The driver no longer owns a "model context registry". The
//! single source of truth for the model's max context length is
//! `ModelInfo::context_length` in the `ProviderCatalog`. The caller
//! resolves that value once before constructing the driver and
//! passes it as a concrete `usize` — see `AgenticLoop::run_inner`
//! where the driver is built.

use crate::compaction::{
    CompactionConfig, CompactionPhase, CompactionRequest, CompactionResponse, CompactionResult,
    CompactorBackend,
};
use crate::events::AgenticEvent;
use crate::SessionView;
use anyhow::Result;
use peko_extension_api::hook_io::{CompactionPreparationPayload, CompactionResultPayload};
use peko_extension_api::session::SessionSnapshot;
use peko_extension_api::ToolFunnel;
use peko_message::LlmMessage;
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Drives one compaction cycle within the agentic loop.
///
/// The loop just calls `check_and_compact()` at the start of each iteration.
/// All complexity (hooks, background tasks, session updates) is encapsulated here.
pub struct CompactionDriver {
    /// Trait object over the root-owned `BackgroundCompactor`. The
    /// driver calls `should_request` to gate the trigger and
    /// `request` to submit. The trait port (Phase 9b.N.4) lets the
    /// driver move into `peko-engine` without dragging the
    /// concrete `BackgroundCompactor` + `Provider` + `QuotaScope`
    /// root-only couplings with it.
    backend: Box<dyn CompactorBackend>,
    config: CompactionConfig,
    context_window: usize,
    /// Receiver for pending background compaction result
    pending_compaction: Option<tokio::sync::oneshot::Receiver<CompactionResponse>>,
    /// Whether compaction was performed this iteration
    compaction_performed: bool,
    /// Last compaction result for post-hook
    last_compaction_result: Option<CompactionResult>,
    /// Usage consumed by the most recent compaction's summarization
    /// LLM call(s). The engine loop reads this via
    /// [`Self::last_compaction_usage`] and folds it into the run's
    /// `total_usage` so the cost of compaction is not silently
    /// dropped on the floor.
    last_compaction_usage: Option<peko_message::TokenUsage>,
    /// Compaction audit fix #4: whether the backend's quota state has
    /// been hydrated from the session-persisted snapshot yet this
    /// run. Hydration happens once, lazily, before the first gate
    /// check (`CompactionDriver::new` is sync, so it can't happen at
    /// construction).
    limits_state_hydrated: bool,
}

impl CompactionDriver {
    /// Create a new compaction driver.
    ///
    /// `context_window` is the **resolved** model max context — the
    /// caller consults `ProviderCatalog::model_context_length` (the
    /// single source of truth) before invoking this. The driver
    /// does not perform catalog resolution itself; doing so would
    /// require threading `Arc<ProviderCatalog>` through every call
    /// site. The value is concrete (a `usize`), not an `Option`, so
    /// the caller picks a fallback policy at the boundary — typically
    /// the catalog value or a sane default when the model has no
    /// declared limit.
    ///
    /// `backend` is the trait-object view of root's
    /// `BackgroundCompactor`. The driver owns a `Box<dyn
    /// CompactorBackend>` and never holds the concrete type.
    pub fn new(
        backend: Box<dyn CompactorBackend>,
        config: CompactionConfig,
        context_window: usize,
    ) -> Self {
        Self {
            backend,
            config,
            context_window,
            pending_compaction: None,
            compaction_performed: false,
            last_compaction_result: None,
            last_compaction_usage: None,
            limits_state_hydrated: false,
        }
    }

    /// Check if compaction is needed and perform it.
    ///
    /// This method handles:
    /// 1. Token estimation and threshold checking
    /// 2. Agent-requested compaction via the persisted
    ///    `compact_requested` flag (plan D2 — OR'd into the threshold
    ///    decision, cleared once compaction genuinely starts)
    /// 3. Pre-compaction hook invocation
    /// 4. Background compaction initiation
    /// 5. Polling for background compaction completion
    /// 6. Post-compaction hook invocation
    /// 7. Session recording and cache updates
    ///
    /// Returns `Ok(true)` if messages were modified by compaction.
    pub async fn check_and_compact<S>(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        session: &S,
        funnel: &dyn ToolFunnel,
        on_event: &(dyn Fn(AgenticEvent) + Send + Sync),
        run_id: &str,
    ) -> Result<bool>
    where
        S: SessionView + ?Sized,
    {
        // F21: hybrid estimator. Anchors on the last assistant message
        // with provider-reported usage and char/4-estimates only the
        // trailing slice since that anchor. Falls back to chars/4 across
        // the full conversation when no usage data is available (e.g.
        // pre-F21 JSONL reloads with `usage: None` everywhere).
        let estimated = estimate_context_tokens(messages);
        let estimated_tokens = estimated.tokens;

        // WS1 (implicit session management): also read the persisted
        // token counter (`Session::last_total_tokens`) so the
        // threshold decision survives restarts and JSONL reloads
        // where the in-memory `messages` slice is empty. Use the
        // more conservative of the two — `estimated_tokens` can
        // drift low after a long pause / cold load, while
        // `last_total_tokens` is anchored by every persisted
        // assistant message's `usage.total_tokens`.
        let (_, _, last_total) = session.token_usage().await;
        let effective_tokens = estimated_tokens.max(last_total);

        // Compaction audit fix #4: hydrate the backend's quota state
        // from the session-persisted snapshot before the first gate
        // check, so the count / cooldown / consecutive limits are
        // per-session rather than per-run.
        self.ensure_limits_state_hydrated(session).await;

        // Plan D2: an agent may have requested compaction out-of-band
        // (the session tool's `compact` action persists a
        // `compact_requested` flag on the session metadata). The flag
        // ORs into the threshold decision and is cleared ONLY when
        // compaction genuinely starts (below), so a crashed run does
        // not lose the request.
        let forced = session.peek_compact_request().await;

        // Start background compaction if needed and not already running
        if self.pending_compaction.is_none()
            && (forced
                || self
                    .backend
                    .should_request(effective_tokens, self.context_window, &self.config)
                    .await)
        {
            info!(
                "Context window approaching limit ({} tokens, last_total={}), checking compaction...",
                effective_tokens, last_total
            );
            if forced {
                // Compaction is genuinely starting — consume the
                // request so it fires exactly once.
                session.clear_compact_request().await;
                on_event(AgenticEvent::Thinking {
                    run_id: run_id.to_string(),
                    text: "Compacting this session as requested. Summarizing older messages..."
                        .to_string(),
                    is_delta: false,
                    is_final: false,
                    signature: None,
                });
            } else {
                on_event(AgenticEvent::Thinking {
                    run_id: run_id.to_string(),
                    text: "Session is getting long. Summarizing older messages...".to_string(),
                    is_delta: false,
                    is_final: false,
                    signature: None,
                });
            }

            self.invoke_pre_hook(messages, session, funnel, effective_tokens, CompactionPhase::PreTurn)
                .await;
        }

        // Check if background compaction has completed
        self.poll_background_compaction(messages, session).await;

        // Post-compaction hook and cleanup
        if self.compaction_performed {
            self.invoke_post_hook(messages, session, funnel, run_id)
                .await;
            self.compaction_performed = false;
            self.last_compaction_result = None;
            // `last_compaction_usage` is intentionally NOT cleared
            // here — the engine loop drains it via `last_compaction_usage()`
            // after `check_and_compact` returns, so the usage reaches
            // `total_usage` for this iteration.
        }

        Ok(true)
    }

    /// PR 3: fire a mid-turn compaction.
    ///
    /// Structurally parallel to `check_and_compact` but with two
    /// important differences:
    ///
    /// 1. **Phase tag** — the resulting `CompactionEntry.phase` and
    ///    `CompactionDetails.phase` are tagged `MidTurn` so hooks
    ///    can tell a post-tool-result summary from a top-of-iteration
    ///    one.
    /// 2. **Snapshot splice** — the worker's compacted list
    ///    (`[initial system?, summary, kept tail]`) is installed
    ///    wholesale by the result poll, exactly like pre-turn. The
    ///    mid-turn-specific step is splicing the `snapshot` system
    ///    message *above the last real user message* so the model
    ///    sees the runtime environment + the capability list before
    ///    re-engaging with the current turn's tool-call /
    ///    tool-result pair, which the kept tail preserves verbatim.
    ///
    /// `snapshot` is the environment snapshot the engine loop built
    /// for this turn; the driver doesn't know how to construct one
    /// (it doesn't have access to the agent / principal fields).
    ///
    /// Returns `Ok(true)` if `messages` was mutated (compaction
    /// completed and the snapshot was spliced in); `Ok(false)` if the
    /// backend returned `NotNeeded` / `Skipped` / `Failed` and
    /// `messages` is unchanged.
    pub async fn compact_mid_turn<S>(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        session: &S,
        funnel: &dyn ToolFunnel,
        on_event: &(dyn Fn(AgenticEvent) + Send + Sync),
        run_id: &str,
        snapshot: peko_session::EnvironmentSnapshot,
    ) -> Result<bool>
    where
        S: SessionView + ?Sized,
    {
        // Mid-turn has its own gating logic — the caller has already
        // decided we should fire (the agentic loop checked estimated
        // tokens after tool execution). We submit the request
        // unconditionally and let the backend decide whether the
        // summarization can actually help (it may return
        // `NotNeeded` if there's nothing to compact, or `Skipped` if
        // the cooldown is active). If either happens, we leave
        // `messages` alone and the loop continues; the next iteration
        // will hit `check_and_compact` (pre-turn) and may evict
        // instead.
        if self.pending_compaction.is_some() {
            // Already running one — don't double-stack.
            return Ok(false);
        }

        // Compaction audit fix #4: same per-session hydration as
        // `check_and_compact` — a mid-turn fire may precede the first
        // pre-turn gate on a fresh run.
        self.ensure_limits_state_hydrated(session).await;

        info!(
            "Mid-turn compaction fired ({} messages, snapshot covers {} capabilities)",
            messages.len(),
            snapshot.permission_policy_summary.len()
        );
        on_event(AgenticEvent::Thinking {
            run_id: run_id.to_string(),
            text: "Compacting mid-turn after tool execution; preserving current tool result."
                .to_string(),
            is_delta: false,
            is_final: false,
            signature: None,
        });

        // Pre-hook fires the same way as `check_and_compact`. The
        // hook may cancel compaction (Handled) or hand back
        // pre-baked messages (ReplaceMessages). Both cases are
        // honoured.
        let effective_tokens = estimate_context_tokens(messages).tokens;
        self.invoke_pre_hook(messages, session, funnel, effective_tokens, CompactionPhase::MidTurn)
            .await;

        // Wait for the background worker. `compact_mid_turn` is
        // synchronous from the agentic loop's perspective — the
        // engine can't proceed without knowing whether the summary
        // has been spliced. We poll until the result lands, with
        // the same 100ms timeout as `check_and_compact`'s implicit
        // poll, but loop here until the receiver resolves. A
        // realistic summarization LLM call takes 1–3s; a few
        // hundred 100ms polls is fine.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            self.poll_background_compaction(messages, session).await;
            if self.compaction_performed || self.pending_compaction.is_none() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                warn!("Mid-turn compaction deadline (30s) exceeded; leaving messages unchanged");
                return Ok(false);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        if !self.compaction_performed {
            // Backend returned NotNeeded / Skipped / Failed.
            return Ok(false);
        }

        // The poll above already installed the worker's compacted
        // message list (`[initial system?, summary, kept tail]`), so
        // the summary is in place exactly once. The mid-turn-specific
        // step is splicing the environment snapshot above the last
        // real user message in the compacted list, so the model
        // re-orients right before the preserved tool-call /
        // tool-result tail.
        if self.last_compaction_result.take().is_some() {
            let snapshot_msg = snapshot.to_system_message();

            match messages
                .iter()
                .rposition(|m| m.role == peko_message::MessageRole::User)
            {
                Some(idx) => {
                    messages.insert(idx, snapshot_msg);
                    info!(
                        "Mid-turn compaction spliced snapshot above last user message (idx {})",
                        idx
                    );
                }
                None => {
                    // No user message in the compacted list. Shouldn't
                    // happen mid-turn (we just received tool results
                    // for an LLM call responding to a user message),
                    // but degrade gracefully: insert the snapshot at
                    // the top instead.
                    warn!("Mid-turn compaction: no user message found; inserting snapshot at top");
                    messages.insert(0, snapshot_msg);
                }
            }
        }

        self.compaction_performed = false;
        // Post-hook fires the same as pre-turn — let handlers see
        // the result and mutate `messages` further if they want.
        self.invoke_post_hook(messages, session, funnel, run_id)
            .await;

        Ok(true)
    }

    /// Reset the driver state (e.g., when starting a new run).
    pub fn reset(&mut self) {
        self.pending_compaction = None;
        self.compaction_performed = false;
        self.last_compaction_result = None;
        self.last_compaction_usage = None;
        self.limits_state_hydrated = false;
    }

    /// Token usage consumed by the most recent compaction's
    /// summarization LLM call(s). Returns `None` if no compaction has
    /// completed yet (or since the last reset). The engine loop
    /// drains this after `check_and_compact` returns and folds the
    /// value into its run-level `total_usage`.
    pub fn last_compaction_usage(&mut self) -> Option<peko_message::TokenUsage> {
        self.last_compaction_usage.take()
    }

    /// Get the context window size.
    #[must_use]
    pub fn context_window(&self) -> usize {
        self.context_window
    }

    /// Get the compaction config.
    #[must_use]
    pub fn config(&self) -> &CompactionConfig {
        &self.config
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Compaction audit fix #4: hydrate the backend's quota state
    /// from the session-persisted snapshot exactly once per run (the
    /// driver is rebuilt per `run_inner`), so
    /// `max_compactions_per_session` / `cooldown_seconds` /
    /// `max_consecutive_auto` are per-session limits rather than
    /// per-run.
    async fn ensure_limits_state_hydrated<S>(&mut self, session: &S)
    where
        S: SessionView + ?Sized,
    {
        if self.limits_state_hydrated {
            return;
        }
        self.limits_state_hydrated = true;
        let persisted = session.compaction_limits_state().await;
        self.backend.hydrate_limits_state(persisted).await;
    }

    /// Compaction audit fix #4: persist the backend's quota state
    /// after a worker mutation (success or failure) so the next run
    /// of this session hydrates from it. The worker updates its
    /// state *before* answering the oneshot, so the snapshot read
    /// here already reflects the mutation.
    async fn sync_limits_state_to_session<S>(&self, session: &S)
    where
        S: SessionView + ?Sized,
    {
        let state = self.backend.limits_state().await;
        if let Err(e) = session.store_compaction_limits_state(state).await {
            warn!("Failed to persist compaction state: {}", e);
        }
    }

    async fn invoke_pre_hook<S>(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        _session: &S,
        funnel: &dyn ToolFunnel,
        estimated_tokens: usize,
        phase: CompactionPhase,
    ) where
        S: SessionView + ?Sized,
    {
        let _ = estimated_tokens;
        let threshold_tokens = self
            .context_window
            .saturating_sub(self.config.reserve_tokens);
        let _keep_recent_tokens = self.config.keep_recent_tokens;

        let _ = threshold_tokens;

        // The driver used to import root's
        // `crate::session::compaction::turn_boundaries` here for the
        // message-selection + split-turn extraction. Those helpers are
        // still root-only (they don't belong on the driver's
        // trait-port surface — the boundary rule says the driver
        // decides "should we compact", not "which messages do we
        // compact"). For the pre-hook payload we now pass the full
        // message list and let the hook / compactor decide.
        //
        // Pre-9b.N.4 behavior: the driver called
        // `turn_boundaries::select_messages_respecting_boundaries` to
        // build a `messages_to_summarize` slice and a
        // `turn_prefix_messages` slice for split-turn compaction. The
        // hook handler could then mutate the slice. Post-9b.N.4: we
        // still pass a `messages_to_summarize` slice — root's
        // `BackgroundCompactor` does the selection internally — and
        // the driver's pre-hook payload uses the full message
        // list as the summary slice. The hook contract is unchanged
        // (handlers see `serde_json::Value` blobs and can do whatever
        // they want). If a future phase reintroduces turn-boundary
        // helpers in `peko-engine`, the pre-hook can be tightened.
        let messages_to_summarize = messages.clone();

        let is_split_turn = false;
        let turn_prefix_messages: Vec<LlmMessage> = vec![];

        let prev_summary = _session
            .load_previous_compaction_summary()
            .await
            .ok()
            .flatten();

        // File-ops extraction lived in root's `summary_format` and is
        // not lifted in 9b.N.4 — pass `serde_json::Value::Null` to
        // signal "no file-ops data". Hooks that depend on this field
        // see `Null` and should degrade gracefully. Future phase can
        // lift `summary_format` if a hook really needs it.
        let file_ops = serde_json::Value::Null;

        let payload = CompactionPreparationPayload {
            messages_to_summarize: messages_to_summarize.clone(),
            turn_prefix_messages: turn_prefix_messages.clone(),
            is_split_turn,
            previous_summary: prev_summary.clone(),
            file_ops,
            estimated_tokens,
            threshold_tokens,
            model_context_limit: self.context_window,
            settings: serde_json::to_value(&self.config).unwrap_or(serde_json::Value::Null),
        };

        let decision = funnel.invoke_session_compaction_pre_hook(payload).await;

        match decision {
            peko_extension_api::hook_io::HookDecision::ReplaceMessages(custom_messages) => {
                info!(
                    "SessionCompaction hook replaced messages: {} → {}",
                    messages.len(),
                    custom_messages.len()
                );
                *messages = custom_messages;
                self.compaction_performed = true;
            }
            peko_extension_api::hook_io::HookDecision::Handled => {
                info!("SessionCompaction hook cancelled compaction");
            }
            peko_extension_api::hook_io::HookDecision::PassThrough => {
                // PassThrough or other — run built-in background compactor
                let request = CompactionRequest {
                    messages: messages.clone(),
                    previous_summary: prev_summary,
                    phase,
                };
                match self.backend.request(request).await {
                    Ok(receiver) => {
                        self.pending_compaction = Some(receiver);
                    }
                    Err(e) => {
                        warn!("Failed to start background compaction: {}", e);
                    }
                }
            }
        }
    }

    async fn poll_background_compaction<S>(&mut self, messages: &mut Vec<LlmMessage>, session: &S)
    where
        S: SessionView + ?Sized,
    {
        if let Some(ref mut receiver) = self.pending_compaction {
            match tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver).await {
                Ok(Ok(response)) => {
                    match response {
                        CompactionResponse::Completed(result) => {
                            *messages = result.messages.clone();
                            self.compaction_performed = true;
                            self.last_compaction_result = Some(result.clone());
                            // Stash the summarization LLM call usage
                            // so the engine loop can fold it into
                            // `total_usage` after `check_and_compact`
                            // returns. Previously this cost was
                            // silently dropped because the compactor
                            // returned only the summary text.
                            self.last_compaction_usage = Some(result.usage);
                            info!(
                                "Background compaction #{} complete: {} messages → summary, saved {} tokens ({} → {})",
                                result.entry.compaction_number,
                                result.entry.messages_compacted,
                                result.entry.tokens_before - result.entry.tokens_after,
                                result.entry.tokens_before,
                                result.entry.tokens_after
                            );

                            // Record compaction entry in session
                            if let Err(e) = session
                                .record_compaction(
                                    &result.entry.summary,
                                    result.entry.messages_compacted,
                                    result.entry.tokens_before,
                                    result.entry.tokens_after,
                                    result.entry.compaction_number,
                                    result.entry.details.as_ref(),
                                )
                                .await
                            {
                                warn!("Failed to record compaction entry: {}", e);
                            }
                            // Audit fix #4: persist the worker's quota
                            // state so the next run hydrates from it.
                            self.sync_limits_state_to_session(session).await;
                        }
                        CompactionResponse::NotNeeded => {
                            debug!("Background compaction: not needed");
                        }
                        CompactionResponse::Skipped(reason) => {
                            debug!("Background compaction skipped: {}", reason);
                        }
                        CompactionResponse::Failed(err) => {
                            warn!("Background compaction failed: {}", err);
                            // Audit fix #4: a failed attempt mutates
                            // the cooldown stamp + consecutive-failure
                            // counter — persist those too.
                            self.sync_limits_state_to_session(session).await;
                        }
                    }
                    self.pending_compaction = None;
                }
                Ok(Err(_)) => {
                    warn!("Background compaction channel closed");
                    self.pending_compaction = None;
                }
                Err(_) => {
                    // Timeout - compaction still in progress, continue with LLM call
                }
            }
        }
    }

    async fn invoke_post_hook<S>(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        session: &S,
        funnel: &dyn ToolFunnel,
        _run_id: &str,
    ) where
        S: SessionView + ?Sized,
    {
        // The post-hook fires `HookPoint::SessionCompactionPost`. We
        // dispatch either `HookInput::CompactionResult` (when we have a
        // fresh compaction) or `HookInput::SessionState` (fallback).
        // Pre-9b.N.4 code constructed `HookInput::SessionState` with a
        // `SessionSnapshot { session_id, message_count, context_tokens,
        // metadata }`. The session_id was fetched via `s.id.clone()`
        // (a root-only field on `crate::session::Session`).
        //
        // Post-9b.N.4 the `SessionView` trait doesn't expose `id()` —
        // the agentic loop already supplies `run_id` for hook stamping,
        // and `session_id` is the same as `run_id` for non-parallel
        // sessions. We use `_run_id` as the session_id stand-in.
        let session_id = _run_id.to_string();

        if let Some(ref result) = self.last_compaction_result {
            let payload = CompactionResultPayload {
                summary: result.entry.summary.clone(),
                messages_compacted: result.entry.messages_compacted,
                tokens_before: result.entry.tokens_before,
                tokens_after: result.entry.tokens_after,
                compaction_number: result.entry.compaction_number,
                details: result.entry.details.clone(),
                messages_after: messages.clone(),
            };

            let decision = funnel.invoke_session_compaction_post_hook(payload).await;

            if let peko_extension_api::hook_io::HookDecision::ReplaceMessages(modified) = decision {
                info!(
                    "SessionCompactionPost hook modified messages: {} → {}",
                    messages.len(),
                    modified.len()
                );
                *messages = modified;
            }
        } else {
            // SessionState fallback
            let snapshot = SessionSnapshot {
                session_id,
                message_count: messages.len(),
                // F21: same hybrid estimator as the pre-hook check. Extension
                // hooks see real provider-reported usage counts after the
                // first assistant turn instead of a chars/4 heuristic.
                context_tokens: estimate_context_tokens(messages).tokens,
                metadata: HashMap::new(),
            };

            let _ = funnel.invoke_session_state_change_hook(snapshot).await;
        }

        // Update context cache after compaction
        if let Err(e) = session.update_context_cache(messages).await {
            warn!("Failed to update context cache: {}", e);
        }
    }
}

// ----------------------------------------------------------------------
// F21 hybrid token estimator — local copy from `src/session/compaction.rs`.
// Phase 9b.N.4 keeps this in peko-engine because the driver's
// pre-hook + post-hook both need it. The root-owned `Compactor` also
// uses it (the `compact` call). Duplication is the lesser evil here
// vs lifting the entire `Compactor` (which depends on `Provider`).
// ----------------------------------------------------------------------
//
// Approximate characters per token for the fallback heuristic.
const CHARS_PER_TOKEN: usize = 4;

/// Walk backward to find the last assistant message with usage data.
///
/// `LlmMessage::usage` is populated by F21 — every assistant turn
/// constructed in the current process carries the provider-reported
/// `TokenUsage`. Pre-F21 JSONL files have `usage: None` everywhere
/// and the heuristic falls back to chars/4 across the full
/// conversation.
fn find_last_assistant_usage(messages: &[LlmMessage]) -> Option<(peko_message::TokenUsage, usize)> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, m)| m.role == peko_message::MessageRole::Assistant && m.usage.is_some())
        .map(|(i, m)| (m.usage.unwrap(), i))
}

/// Detailed token usage estimate with breakdown (F21 hybrid estimator).
///
/// Mirrors `crate::compaction::ContextUsageEstimate` (lifted from
/// root's `src/session/compaction.rs:207` in 9b.N.4). The local
/// type here is `pub(crate)` so the agentic loop's mid-turn
/// trigger can call [`estimate_context_tokens_for_agentic`] and
/// read the same `tokens` field. Callers outside this crate should
/// use the public `crate::compaction::ContextUsageEstimate`
/// re-export.
#[derive(Debug, Clone)]
#[allow(dead_code)] // written for F17 inspection; readers added in F30+
pub struct ContextUsageEstimate {
    /// Total estimated tokens
    pub tokens: usize,
    /// Tokens from the last assistant usage record
    pub usage_tokens: usize,
    /// Tokens estimated for trailing messages after last usage
    pub trailing_tokens: usize,
    /// Index of the last assistant message with usage data
    pub last_usage_index: Option<usize>,
}

/// Hybrid token estimation — anchors on the last assistant message
/// with provider-reported usage, then char/4-estimates the trailing
/// slice since that anchor. Falls back to chars/4 across the full
/// conversation when no usage data is available.
///
/// Mirrors `crate::session::compaction::Compactor::estimate_context_tokens`
/// (root) — duplicated here so the driver's pre-hook + post-hook
/// can run without a root dep. The two implementations are
/// behaviour-equivalent; any future change must update both.
/// Public re-export of the hybrid estimator for the agentic loop's
/// mid-turn trigger. The loop calls this AFTER tool execution to
/// decide whether a mid-turn compaction should fire (separate from
/// the top-of-iteration `check_and_compact` path). The result type
/// is the same `ContextUsageEstimate` returned internally; the loop
/// only reads the `tokens` field.
pub fn estimate_context_tokens_for_agentic(messages: &[LlmMessage]) -> ContextUsageEstimate {
    estimate_context_tokens(messages)
}

fn estimate_context_tokens(messages: &[LlmMessage]) -> ContextUsageEstimate {
    if let Some((usage, index)) = find_last_assistant_usage(messages) {
        let usage_tokens = (usage.input + usage.output) as usize;
        let trailing_tokens: usize = messages[index + 1..]
            .iter()
            .map(|m| content_block_token_estimate(&m.content))
            .sum();
        ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        }
    } else {
        let estimated = estimate_tokens(messages);
        ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        }
    }
}

/// Heuristic token estimator — chars / 4 across the conversation.
fn estimate_tokens(messages: &[LlmMessage]) -> usize {
    messages
        .iter()
        .map(|m| content_block_token_estimate(&m.content))
        .sum()
}

/// Sum the token estimate for a single message's content blocks.
/// Text uses chars/4; images use the three-tier
/// `estimate_image_tokens` so retention budgets reflect realistic
/// image costs instead of the F21 placeholder of 50 chars/image.
fn content_block_token_estimate(content: &[peko_message::ContentBlock]) -> usize {
    use peko_message::ContentBlock;
    content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::Image { source, mime_type } => {
                // Image token cost is char-equivalent (chars / 4) at the
                // CHARS_PER_TOKEN denominator used by text. Multiply by
                // CHARS_PER_TOKEN to keep the downstream
                // `+ 20) / CHARS_PER_TOKEN + 4` arithmetic unit-stable.
                let image_tokens =
                    peko_session::estimate_image_tokens(source, mime_type);
                image_tokens.saturating_mul(CHARS_PER_TOKEN)
            }
            _ => 50,
        })
        .sum::<usize>()
        .saturating_add(20)
        / CHARS_PER_TOKEN
        + 4
}

// Phase 9b.N.4: `load_compaction_config` lives in root
// (`src/session/compaction.rs`) because it depends on the `dirs` +
// `toml` crates, which aren't in `peko-engine`'s dep graph. Root is
// the right home — it already owns the `Config` struct that calls
// into this loader. The lifted `CompactionDriver` accepts the
// loaded `CompactionConfig` as a constructor argument.

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;

    /// Stub `SessionView` with a switchable compact-request flag.
    /// Everything unrelated to the flag is inert.
    struct StubSession {
        requested: Mutex<bool>,
        clears: AtomicUsize,
        last_total: AtomicUsize,
        /// Fix #4: the persisted compaction quota state this session
        /// would return, plus a log of every state the driver stored.
        limits: Mutex<crate::compaction::CompactionLimitsState>,
        stored_states: Mutex<Vec<crate::compaction::CompactionLimitsState>>,
    }

    impl StubSession {
        fn new(requested: bool) -> Self {
            Self {
                requested: Mutex::new(requested),
                clears: AtomicUsize::new(0),
                last_total: AtomicUsize::new(0),
                limits: Mutex::new(crate::compaction::CompactionLimitsState::default()),
                stored_states: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl SessionView for StubSession {
        async fn add_tool_result(
            &self,
            _tool_call_id: &str,
            _tool_name: &str,
            _result: &str,
            _is_error: bool,
        ) -> Result<()> {
            Ok(())
        }
        async fn record_compaction(
            &self,
            _summary: &str,
            _messages_compacted: usize,
            _tokens_before: usize,
            _tokens_after: usize,
            _compaction_number: usize,
            _details: Option<&serde_json::Value>,
        ) -> Result<()> {
            Ok(())
        }
        async fn load_previous_compaction_summary(&self) -> Result<Option<String>> {
            Ok(None)
        }
        async fn update_context_cache(&self, _messages: &[LlmMessage]) -> Result<()> {
            Ok(())
        }
        async fn id(&self) -> String {
            "stub-session".to_string()
        }
        async fn token_usage(&self) -> (usize, usize, usize) {
            (0, 0, self.last_total.load(Ordering::SeqCst))
        }
        async fn add_user(&self, _content: String) -> Result<()> {
            Ok(())
        }
        async fn add_user_with_source(
            &self,
            _content: String,
            _source: peko_session::events::MessageSource,
        ) -> Result<()> {
            Ok(())
        }
        async fn set_model(&self, _provider: &str, _model: &str) {}
        async fn record_model_change(&self, _provider: &str, _model_id: &str) -> Result<()> {
            Ok(())
        }
        async fn set_model_context_limit(&self, _limit: usize) {}
        async fn add_assistant(
            &self,
            _content: String,
            _tool_calls: Option<Vec<peko_message::ToolCallInfo>>,
            _usage: Option<peko_message::TokenUsage>,
        ) -> Result<()> {
            Ok(())
        }
        async fn add_assistant_with_blocks(
            &self,
            _content_blocks: Vec<peko_message::ContentBlock>,
            _tool_calls: Option<Vec<peko_message::ToolCallBlock>>,
            _thinking: Option<peko_message::ThinkingBlock>,
            _usage: Option<peko_message::TokenUsage>,
        ) -> Result<()> {
            Ok(())
        }
        async fn load_history(&self) -> Result<Vec<LlmMessage>> {
            Ok(vec![])
        }
        async fn peek_compact_request(&self) -> bool {
            *self.requested.lock().expect("requested mutex poisoned")
        }
        async fn clear_compact_request(&self) {
            *self.requested.lock().expect("requested mutex poisoned") = false;
            self.clears.fetch_add(1, Ordering::SeqCst);
        }
        async fn compaction_limits_state(&self) -> crate::compaction::CompactionLimitsState {
            *self.limits.lock().expect("limits mutex poisoned")
        }
        async fn store_compaction_limits_state(
            &self,
            state: crate::compaction::CompactionLimitsState,
        ) -> Result<()> {
            self.stored_states
                .lock()
                .expect("stored_states mutex poisoned")
                .push(state);
            Ok(())
        }
    }

    /// Stub `CompactorBackend`: `should_request` returns a fixed gate
    /// value; `request` records the call and returns a receiver that
    /// stays pending (the sender is stashed so the channel neither
    /// resolves nor closes during the driver's 100ms poll).
    struct StubBackend {
        gate: bool,
        request_calls: AtomicUsize,
        senders: Mutex<Vec<oneshot::Sender<CompactionResponse>>>,
        /// Fix #4: states the driver hydrated us with, plus the worker
        /// state `limits_state` reports.
        hydrated: Mutex<Vec<crate::compaction::CompactionLimitsState>>,
        worker_state: Mutex<crate::compaction::CompactionLimitsState>,
    }

    impl StubBackend {
        fn new(gate: bool) -> Self {
            Self {
                gate,
                request_calls: AtomicUsize::new(0),
                senders: Mutex::new(vec![]),
                hydrated: Mutex::new(vec![]),
                worker_state: Mutex::new(crate::compaction::CompactionLimitsState::default()),
            }
        }
    }

    #[async_trait]
    impl CompactorBackend for StubBackend {
        async fn should_request(
            &self,
            _estimated_tokens: usize,
            _context_window: usize,
            _config: &CompactionConfig,
        ) -> bool {
            self.gate
        }
        async fn request(
            &self,
            _request: CompactionRequest,
        ) -> Result<oneshot::Receiver<CompactionResponse>> {
            self.request_calls.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = oneshot::channel();
            self.senders
                .lock()
                .expect("senders mutex poisoned")
                .push(tx);
            Ok(rx)
        }
        async fn hydrate_limits_state(&self, state: crate::compaction::CompactionLimitsState) {
            self.hydrated
                .lock()
                .expect("hydrated mutex poisoned")
                .push(state);
        }
        async fn limits_state(&self) -> crate::compaction::CompactionLimitsState {
            *self
                .worker_state
                .lock()
                .expect("worker_state mutex poisoned")
        }
    }

    /// No-op `ToolFunnel` (same shape as the renderer's test stub —
    /// every hook passes through / returns empty).
    struct EmptyFunnel;

    #[async_trait]
    impl ToolFunnel for EmptyFunnel {
        async fn is_parallelizable(&self, _tool_name: &str) -> bool {
            true
        }
        async fn pre_tool_use(
            &self,
            _tool_name: &str,
            _params: serde_json::Value,
            _workspace: Option<String>,
            _agent_id: Option<String>,
            _session_id: Option<String>,
            _caller_id: Option<String>,
            _principal_id: Option<String>,
            _principal_name: Option<String>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
        ) {
        }
        async fn post_tool_use(
            &self,
            _tool_name: &str,
            _params: serde_json::Value,
            _workspace: Option<String>,
            _agent_id: Option<String>,
            _session_id: Option<String>,
            _caller_id: Option<String>,
            _principal_id: Option<String>,
            _principal_name: Option<String>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
        ) {
        }
        async fn execute_tool_via_hook(
            &self,
            _tool_name: &str,
            _params: serde_json::Value,
            _workspace: Option<String>,
            _agent_id: Option<String>,
            _session_id: Option<String>,
            _caller_id: Option<String>,
            _principal_id: Option<String>,
            _principal_name: Option<String>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
            _abort_signal: Option<tokio::sync::watch::Receiver<bool>>,
        ) -> Result<(String, serde_json::Value, bool)> {
            anyhow::bail!("EmptyFunnel::execute_tool_via_hook not implemented")
        }
        async fn invoke_session_compaction_pre_hook(
            &self,
            _payload: CompactionPreparationPayload,
        ) -> peko_extension_api::hook_io::HookDecision {
            peko_extension_api::hook_io::HookDecision::PassThrough
        }
        async fn invoke_session_compaction_post_hook(
            &self,
            _payload: CompactionResultPayload,
        ) -> peko_extension_api::hook_io::HookDecision {
            peko_extension_api::hook_io::HookDecision::PassThrough
        }
        async fn invoke_session_state_change_hook(
            &self,
            _snapshot: SessionSnapshot,
        ) -> peko_extension_api::hook_io::HookDecision {
            peko_extension_api::hook_io::HookDecision::PassThrough
        }
        async fn invoke_stop_hook(&self, _merged: serde_json::Value) {}
        async fn invoke_after_agent_hook(&self, _merged: serde_json::Value) {}
        async fn set_session_key(&self, _agent_id: &str, _key: Option<String>) {}
        async fn list_tool_definitions_with_allowlist(
            &self,
            _capabilities: &peko_extension_api::Capabilities,
            _active_extensions: Option<&peko_extension_api::ActiveExtensionSet>,
            _principal_id: &peko_subject::PrincipalId,
        ) -> Vec<peko_provider_api::ToolDefinition> {
            Vec::new()
        }
        async fn has_deferred_tools_for(&self, _principal_id: &peko_subject::PrincipalId) -> bool {
            false
        }
        async fn invoke_prompt_section_hook(
            &self,
            _section: &str,
            _priority: i32,
            _principal_id: Option<&str>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
            _workspace: Option<String>,
        ) -> Option<String> {
            None
        }
        async fn invoke_session_context_build_hook(
            &self,
            _snapshot: SessionSnapshot,
            _principal_id: Option<&str>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
            _workspace: Option<String>,
        ) -> Option<String> {
            None
        }
    }

    struct Fixture {
        driver: CompactionDriver,
        backend: Arc<StubBackend>,
        session: StubSession,
        events: Arc<Mutex<Vec<String>>>,
    }

    fn fixture(requested: bool, gate: bool) -> Fixture {
        let backend = Arc::new(StubBackend::new(gate));
        let driver = CompactionDriver::new(
            Box::new(ArcBackend(Arc::clone(&backend))),
            CompactionConfig::default(),
            200_000,
        );
        Fixture {
            driver,
            backend,
            session: StubSession::new(requested),
            events: Arc::new(Mutex::new(vec![])),
        }
    }

    /// The driver owns a `Box<dyn CompactorBackend>`; wrap the
    /// shared `Arc<StubBackend>` so tests can inspect the calls.
    struct ArcBackend(Arc<StubBackend>);

    #[async_trait]
    impl CompactorBackend for ArcBackend {
        async fn should_request(
            &self,
            estimated_tokens: usize,
            context_window: usize,
            config: &CompactionConfig,
        ) -> bool {
            self.0
                .should_request(estimated_tokens, context_window, config)
                .await
        }
        async fn request(
            &self,
            request: CompactionRequest,
        ) -> Result<oneshot::Receiver<CompactionResponse>> {
            self.0.request(request).await
        }
        async fn hydrate_limits_state(&self, state: crate::compaction::CompactionLimitsState) {
            self.0.hydrate_limits_state(state).await;
        }
        async fn limits_state(&self) -> crate::compaction::CompactionLimitsState {
            self.0.limits_state().await
        }
    }

    fn small_messages() -> Vec<LlmMessage> {
        // A couple of tiny messages — far below any threshold.
        vec![LlmMessage::user("hello"), LlmMessage::assistant("hi")]
    }

    fn event_sink(events: &Arc<Mutex<Vec<String>>>) -> impl Fn(AgenticEvent) + Send + Sync + '_ {
        move |event| {
            if let AgenticEvent::Thinking { text, .. } = event {
                events.lock().expect("events mutex poisoned").push(text);
            }
        }
    }

    #[tokio::test]
    async fn forced_compaction_fires_below_threshold_and_clears_flag() {
        let mut f = fixture(true, false); // flag set, token gate closed
        let mut messages = small_messages();
        let on_event = event_sink(&f.events);

        f.driver
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();

        assert_eq!(
            f.backend.request_calls.load(Ordering::SeqCst),
            1,
            "forced flag must start compaction even below threshold"
        );
        assert!(
            f.driver.pending_compaction.is_some(),
            "background compaction should be pending"
        );
        assert!(
            !f.session.peek_compact_request().await,
            "flag cleared once compaction starts"
        );
        assert_eq!(f.session.clears.load(Ordering::SeqCst), 1);
        let events = f.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 1);
        assert!(
            events[0].contains("as requested"),
            "forced wording expected, got: {}",
            events[0]
        );
    }

    #[tokio::test]
    async fn no_flag_below_threshold_does_nothing() {
        let mut f = fixture(false, false); // no flag, gate closed
        let mut messages = small_messages();
        let on_event = event_sink(&f.events);

        f.driver
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();

        assert_eq!(f.backend.request_calls.load(Ordering::SeqCst), 0);
        assert!(f.driver.pending_compaction.is_none());
        assert_eq!(f.session.clears.load(Ordering::SeqCst), 0);
        assert!(f.events.lock().expect("events mutex poisoned").is_empty());
    }

    #[tokio::test]
    async fn threshold_triggered_compaction_unchanged_without_flag() {
        let mut f = fixture(false, true); // no flag, gate open (over threshold)
        let mut messages = small_messages();
        let on_event = event_sink(&f.events);

        f.driver
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();

        assert_eq!(f.backend.request_calls.load(Ordering::SeqCst), 1);
        assert!(
            f.driver.pending_compaction.is_some(),
            "background compaction should be pending"
        );
        assert_eq!(
            f.session.clears.load(Ordering::SeqCst),
            0,
            "non-forced start must not touch the flag"
        );
        let events = f.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 1);
        assert!(
            events[0].contains("Session is getting long"),
            "threshold wording preserved, got: {}",
            events[0]
        );
    }

    /// WS1 regression: when the in-memory `messages` slice is empty
    /// (cold reload from JSONL) but the persisted
    /// `Session::last_total_tokens` is over the threshold, the
    /// driver must still fire compaction. Before WS1 the
    /// estimator only saw the empty slice and let the session keep
    /// growing until `ContextWindowExceeded` recovery fired.
    #[tokio::test]
    async fn persisted_last_total_triggers_compaction_with_empty_messages() {
        let mut f = fixture(false, true); // gate open (would fire if effective_tokens > threshold)
        // Empty messages — simulates a cold reload where the
        // estimator has nothing to anchor on.
        let mut messages: Vec<LlmMessage> = vec![];
        let on_event = event_sink(&f.events);

        // Pretend the previous run had accumulated 90k tokens.
        f.session.last_total.store(90_000, Ordering::SeqCst);

        f.driver
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();

        // The driver should call into the backend — proving
        // `effective_tokens = max(0, 90_000) = 90_000` reached the
        // gate. Without the WS1 max() this would have been 0 and the
        // gate would have stayed closed.
        assert_eq!(
            f.backend.request_calls.load(Ordering::SeqCst),
            1,
            "persisted last_total_tokens must trigger compaction even with empty messages"
        );
    }

    /// WS1 symmetry: when the estimator sees a larger number than
    /// the persisted counter, use the estimator's value (don't
    /// regress existing behavior). This is the existing happy path
    /// covered by `threshold_triggered_compaction_unchanged_without_flag`
    /// but with an explicit zero on `last_total` to lock in the max()
    /// semantic.
    #[tokio::test]
    async fn estimator_wins_when_larger_than_persisted_counter() {
        let mut f = fixture(false, true);
        let mut messages = small_messages();
        let on_event = event_sink(&f.events);

        // last_total is zero; the in-memory messages drive the gate.
        f.session.last_total.store(0, Ordering::SeqCst);

        f.driver
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();

        assert_eq!(f.backend.request_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn forced_flag_survives_while_compaction_already_pending() {
        let mut f = fixture(true, false);
        let mut messages = small_messages();
        let on_event = event_sink(&f.events);

        // First call: forced start consumes the flag.
        f.driver
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();
        assert_eq!(f.backend.request_calls.load(Ordering::SeqCst), 1);

        // Re-arm the flag while the first compaction is still pending:
        // the driver must NOT start a second compaction and must
        // NOT clear the flag (nothing genuinely started for it).
        *f.session
            .requested
            .lock()
            .expect("requested mutex poisoned") = true;
        f.driver
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();
        assert_eq!(
            f.backend.request_calls.load(Ordering::SeqCst),
            1,
            "no second compaction while one is pending"
        );
        assert!(
            f.session.peek_compact_request().await,
            "flag survives while a compaction is already pending"
        );
    }

    /// Build the `Completed` response the worker would send, with a
    /// compacted list of the real shape `[system, summary, kept...]`.
    fn completed_response(
        compacted: Vec<LlmMessage>,
        compacted_count: usize,
    ) -> CompactionResponse {
        CompactionResponse::Completed(CompactionResult {
            messages: compacted,
            entry: crate::compaction::CompactionEntry {
                timestamp: chrono::Utc::now(),
                summary: "summary text".to_string(),
                first_kept_entry_id: "kept_1".to_string(),
                messages_compacted: compacted_count,
                tokens_before: 1_000,
                tokens_after: 100,
                compaction_number: 1,
                phase: CompactionPhase::MidTurn,
                details: None,
            },
            state: crate::compaction::CompactionState::default(),
            usage: peko_message::TokenUsage::default(),
        })
    }

    fn tool_result_message() -> LlmMessage {
        LlmMessage {
            role: peko_message::MessageRole::Tool,
            content: vec![peko_message::ContentBlock::ToolResult {
                tool_call_id: "tc-1".to_string(),
                name: "Read".to_string(),
                content: vec![peko_message::ContentBlock::Text {
                    text: "file body".to_string(),
                }],
                is_error: false,
            }],
            ..Default::default()
        }
    }

    /// Drive `compact_mid_turn` while a responder task answers the
    /// backend's stashed sender with `response`. Returns the driver's
    /// return value.
    async fn drive_mid_turn(
        f: &mut Fixture,
        messages: &mut Vec<LlmMessage>,
        response: CompactionResponse,
    ) -> Result<bool> {
        let backend = Arc::clone(&f.backend);
        let responder = async move {
            let tx = loop {
                if let Some(tx) = backend
                    .senders
                    .lock()
                    .expect("senders mutex poisoned")
                    .pop()
                {
                    break tx;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            };
            tx.send(response).expect("driver receiver alive");
        };
        let on_event = event_sink(&f.events);
        let snapshot = peko_session::EnvironmentSnapshot {
            runtime_environment: "test-os".to_string(),
            permission_policy_summary: vec!["tool:read".to_string()],
        };
        let (driven, ()) = tokio::join!(
            f.driver.compact_mid_turn(
                messages,
                &f.session,
                &EmptyFunnel,
                &on_event,
                "run-1",
                snapshot,
            ),
            responder
        );
        driven
    }

    fn text_contains(messages: &[LlmMessage], needle: &str) -> usize {
        messages
            .iter()
            .filter(|m| {
                m.content.iter().any(|b| {
                    matches!(b, peko_message::ContentBlock::Text { text } if text.contains(needle))
                })
            })
            .count()
    }

    /// Fix 2 regression: mid-turn compaction must not duplicate the
    /// summary. The result poll installs the worker's compacted list
    /// (summary included); the driver then splices ONLY the
    /// environment snapshot above the last user message.
    #[tokio::test]
    async fn mid_turn_compaction_installs_summary_once_and_splices_snapshot() {
        let mut f = fixture(false, false);
        let mut messages = vec![
            LlmMessage::system("system prompt"),
            LlmMessage::user("older question"),
            LlmMessage::assistant("older answer"),
            LlmMessage::user("current question"),
            LlmMessage::assistant("calling a tool"),
            tool_result_message(),
        ];
        let original_len = messages.len();

        // The worker's compacted list: older turns collapsed into the
        // summary; the current turn (user + assistant + tool result)
        // survives in the kept tail.
        let compacted = vec![
            LlmMessage::system("system prompt"),
            LlmMessage::system("[Conversation Summary - 3 messages]:\nsummary text"),
            LlmMessage::user("current question"),
            LlmMessage::assistant("calling a tool"),
            tool_result_message(),
        ];
        let compacted_len = compacted.len();

        let mutated = drive_mid_turn(&mut f, &mut messages, completed_response(compacted, 3))
            .await
            .unwrap();

        assert!(mutated, "mid-turn compaction should report a mutation");
        assert_eq!(
            text_contains(&messages, "[Conversation Summary - 3 messages]:"),
            1,
            "summary must appear exactly once: {messages:?}"
        );
        assert_eq!(text_contains(&messages, "## Environment Snapshot"), 1);
        // Compacted list + one spliced snapshot message.
        assert_eq!(messages.len(), compacted_len + 1);
        assert!(
            messages.len() < original_len + 2,
            "older turns must be collapsed, not retained"
        );
        // Snapshot sits directly above the last user message; the
        // in-flight tool-call / tool-result pair stays at the tail.
        let snapshot_idx = messages
            .iter()
            .position(|m| {
                m.content.iter().any(|b| {
                    matches!(b, peko_message::ContentBlock::Text { text } if text.contains("## Environment Snapshot"))
                })
            })
            .expect("snapshot present");
        assert_eq!(
            messages[snapshot_idx + 1].role,
            peko_message::MessageRole::User
        );
        assert_eq!(
            messages.last().expect("tail").role,
            peko_message::MessageRole::Tool
        );
    }

    /// Fix 2 fallback: no user message in the compacted list — the
    /// snapshot goes to the top and the summary still appears once.
    #[tokio::test]
    async fn mid_turn_compaction_without_user_message_inserts_snapshot_at_top() {
        let mut f = fixture(false, false);
        let mut messages = vec![
            LlmMessage::system("system prompt"),
            LlmMessage::assistant("orphaned answer"),
        ];

        let compacted = vec![
            LlmMessage::system("system prompt"),
            LlmMessage::system("[Conversation Summary - 2 messages]:\nsummary text"),
            LlmMessage::assistant("orphaned answer"),
        ];

        let mutated = drive_mid_turn(&mut f, &mut messages, completed_response(compacted, 2))
            .await
            .unwrap();

        assert!(mutated);
        assert_eq!(text_contains(&messages, "[Conversation Summary"), 1);
        assert_eq!(text_contains(&messages, "## Environment Snapshot"), 1);
        assert!(
            messages[0].content.iter().any(|b| {
                matches!(b, peko_message::ContentBlock::Text { text } if text.contains("## Environment Snapshot"))
            }),
            "snapshot must lead the list when there is no user message"
        );
    }

    /// Fix #4: the driver hydrates the backend from the
    /// session-persisted quota state exactly once per run, before the
    /// first gate check.
    #[tokio::test]
    async fn driver_hydrates_backend_from_session_state_once_per_run() {
        let mut f = fixture(false, false);
        let persisted = crate::compaction::CompactionLimitsState {
            compaction_count: 7,
            last_compaction_at_ms: Some(123),
            consecutive_auto: 2,
            consecutive_failures: 1,
        };
        *f.session.limits.lock().expect("limits mutex poisoned") = persisted;

        let mut messages = small_messages();
        let on_event = event_sink(&f.events);
        for _ in 0..2 {
            f.driver
                .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
                .await
                .unwrap();
        }

        let hydrated = f.backend.hydrated.lock().expect("hydrated mutex poisoned");
        assert_eq!(
            hydrated.as_slice(),
            &[persisted],
            "backend must be hydrated exactly once, with the persisted state"
        );
    }

    /// Fix #4: after a Completed compaction the driver persists the
    /// worker's quota state back onto the session.
    #[tokio::test]
    async fn driver_persists_worker_state_after_completed_compaction() {
        let mut f = fixture(false, false);
        let worker_state = crate::compaction::CompactionLimitsState {
            compaction_count: 3,
            last_compaction_at_ms: Some(42),
            consecutive_auto: 3,
            consecutive_failures: 0,
        };
        *f.backend
            .worker_state
            .lock()
            .expect("worker_state mutex poisoned") = worker_state;

        let mut messages = vec![
            LlmMessage::system("system prompt"),
            LlmMessage::user("older question"),
            LlmMessage::assistant("older answer"),
            LlmMessage::user("current question"),
            LlmMessage::assistant("calling a tool"),
            tool_result_message(),
        ];
        let compacted = vec![
            LlmMessage::system("system prompt"),
            LlmMessage::system("[Conversation Summary - 3 messages]:\nsummary text"),
            LlmMessage::user("current question"),
            LlmMessage::assistant("calling a tool"),
            tool_result_message(),
        ];

        let mutated = drive_mid_turn(&mut f, &mut messages, completed_response(compacted, 3))
            .await
            .unwrap();
        assert!(mutated);

        let stored = f
            .session
            .stored_states
            .lock()
            .expect("stored_states mutex poisoned");
        assert_eq!(
            stored.as_slice(),
            &[worker_state],
            "worker quota state must be persisted after Completed"
        );
    }

    /// Fix #4: a Failed response also syncs the worker state (the
    /// failure stamps the cooldown + bumps consecutive failures).
    #[tokio::test]
    async fn driver_persists_worker_state_after_failed_compaction() {
        let mut f = fixture(false, false);
        let worker_state = crate::compaction::CompactionLimitsState {
            compaction_count: 1,
            last_compaction_at_ms: Some(42),
            consecutive_auto: 1,
            consecutive_failures: 2,
        };
        *f.backend
            .worker_state
            .lock()
            .expect("worker_state mutex poisoned") = worker_state;

        let mut messages = small_messages();
        let mutated = drive_mid_turn(
            &mut f,
            &mut messages,
            CompactionResponse::Failed("provider down".to_string()),
        )
        .await
        .unwrap();
        assert!(!mutated, "failed compaction must not mutate messages");

        let stored = f
            .session
            .stored_states
            .lock()
            .expect("stored_states mutex poisoned");
        assert_eq!(
            stored.as_slice(),
            &[worker_state],
            "worker quota state must be persisted after Failed"
        );
    }
}
