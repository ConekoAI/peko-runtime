//! Compaction orchestrator for the agentic loop.
//!
//! Encapsulates the entire compaction lifecycle:
//! - Pre-compaction hook invocation
//! - Background compactor coordination
//! - Post-compaction hook invocation
//! - Session recording and cache updates
//!
//! Phase 9b.N.4 lifted this file from `src/engine/compaction_orchestrator.rs`
//! into `peko-engine`. The lift relied on three trait ports so the
//! orchestrator can talk to root-only types without a direct dependency:
//!
//! - **`ToolFunnel`** (`peko-extension-host`) — abstracted `ExtensionCore`
//!   for hook firing. Three new methods added in 9b.N.4 cover the
//!   compaction / session-state hooks (`invoke_session_compaction_pre_hook`,
//!   `invoke_session_compaction_post_hook`, `invoke_session_state_change_hook`).
//! - **`SessionView`** (`peko-engine`) — extended in 9b.N.4 with
//!   `record_compaction`, `load_previous_compaction_summary`, and
//!   `update_context_cache` for the orchestrator's session writes.
//! - **`CompactorBackend`** (`peko-engine::compaction`) — new in 9b.N.4,
//!   abstracts `BackgroundCompactor` so the orchestrator holds a
//!   `Box<dyn CompactorBackend>` instead of a concrete impl.
//!
//! The orchestrator no longer owns a "model context registry". The
//! single source of truth for the model's max context length is
//! `ModelInfo::context_length` in the `ProviderCatalog`. The caller
//! resolves that value once before constructing the orchestrator and
//! passes it as a concrete `usize` — see `AgenticLoop::run_inner`
//! where the orchestrator is built.

use crate::compaction::{
    CompactionConfig, CompactionRequest, CompactionResponse, CompactionResult, CompactorBackend,
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

/// Orchestrates compaction within the agentic loop.
///
/// The loop just calls `check_and_compact()` at the start of each iteration.
/// All complexity (hooks, background tasks, session updates) is encapsulated here.
pub struct CompactionOrchestrator {
    /// Trait object over the root-owned `BackgroundCompactor`. The
    /// orchestrator calls `should_request` to gate the trigger and
    /// `request` to submit. The trait port (Phase 9b.N.4) lets the
    /// orchestrator move into `peko-engine` without dragging the
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
}

impl CompactionOrchestrator {
    /// Create a new compaction orchestrator.
    ///
    /// `context_window` is the **resolved** model max context — the
    /// caller consults `ProviderCatalog::model_context_length` (the
    /// single source of truth) before invoking this. The orchestrator
    /// does not perform catalog resolution itself; doing so would
    /// require threading `Arc<ProviderCatalog>` through every call
    /// site. The value is concrete (a `usize`), not an `Option`, so
    /// the caller picks a fallback policy at the boundary — typically
    /// the catalog value or a sane default when the model has no
    /// declared limit.
    ///
    /// `backend` is the trait-object view of root's
    /// `BackgroundCompactor`. The orchestrator owns a `Box<dyn
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
                    .should_request(estimated_tokens, self.context_window, &self.config)
                    .await)
        {
            info!(
                "Context window approaching limit ({} tokens), checking compaction...",
                estimated_tokens
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

            self.invoke_pre_hook(messages, session, funnel, estimated_tokens)
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

    /// Reset the orchestrator state (e.g., when starting a new run).
    pub fn reset(&mut self) {
        self.pending_compaction = None;
        self.compaction_performed = false;
        self.last_compaction_result = None;
        self.last_compaction_usage = None;
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

    async fn invoke_pre_hook<S>(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        _session: &S,
        funnel: &dyn ToolFunnel,
        estimated_tokens: usize,
    ) where
        S: SessionView + ?Sized,
    {
        let _ = estimated_tokens;
        let threshold_tokens = self
            .context_window
            .saturating_sub(self.config.reserve_tokens);
        let _keep_recent_tokens = self.config.keep_recent_tokens;

        let _ = threshold_tokens;

        // The orchestrator used to import root's
        // `crate::session::compaction::turn_boundaries` here for the
        // message-selection + split-turn extraction. Those helpers are
        // still root-only (they don't belong on the orchestrator's
        // trait-port surface — the boundary rule says the orchestrator
        // decides "should we compact", not "which messages do we
        // compact"). For the pre-hook payload we now pass the full
        // message list and let the hook / compactor decide.
        //
        // Pre-9b.N.4 behavior: the orchestrator called
        // `turn_boundaries::select_messages_respecting_boundaries` to
        // build a `messages_to_summarize` slice and a
        // `turn_prefix_messages` slice for split-turn compaction. The
        // hook handler could then mutate the slice. Post-9b.N.4: we
        // still pass a `messages_to_summarize` slice — root's
        // `BackgroundCompactor` does the selection internally — and
        // the orchestrator's pre-hook payload uses the full message
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
                        }
                        CompactionResponse::NotNeeded => {
                            debug!("Background compaction: not needed");
                        }
                        CompactionResponse::Skipped(reason) => {
                            debug!("Background compaction skipped: {}", reason);
                        }
                        CompactionResponse::Failed(err) => {
                            warn!("Background compaction failed: {}", err);
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
// Phase 9b.N.4 keeps this in peko-engine because the orchestrator's
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
/// type here is private — callers should use the public
/// `crate::compaction::ContextUsageEstimate` re-export instead. The
/// duplicate definition exists because the orchestrator's
/// pre-hook + post-hook both call `estimate_context_tokens` and
/// want a local typed return rather than threading the `compaction`
/// re-export through the orchestrator's private helpers.
#[derive(Debug, Clone)]
#[allow(dead_code)] // written for F17 inspection; readers added in F30+
struct ContextUsageEstimate {
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
/// (root) — duplicated here so the orchestrator's pre-hook + post-hook
/// can run without a root dep. The two implementations are
/// behaviour-equivalent; any future change must update both.
fn estimate_context_tokens(messages: &[LlmMessage]) -> ContextUsageEstimate {
    use peko_message::ContentBlock;
    if let Some((usage, index)) = find_last_assistant_usage(messages) {
        let usage_tokens = (usage.input + usage.output) as usize;
        let trailing_tokens: usize = messages[index + 1..]
            .iter()
            .map(|m| {
                let content_len: usize = m
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => text.len(),
                        _ => 50,
                    })
                    .sum();
                (content_len + 20) / CHARS_PER_TOKEN + 4
            })
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
    use peko_message::ContentBlock;
    messages
        .iter()
        .map(|m| {
            let content_len: usize = m
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.len(),
                    _ => 50,
                })
                .sum();
            (content_len + 20) / CHARS_PER_TOKEN + 4
        })
        .sum()
}

// Phase 9b.N.4: `load_compaction_config` lives in root
// (`src/session/compaction.rs`) because it depends on the `dirs` +
// `toml` crates, which aren't in `peko-engine`'s dep graph. Root is
// the right home — it already owns the `Config` struct that calls
// into this loader. The lifted `CompactionOrchestrator` accepts the
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
    }

    impl StubSession {
        fn new(requested: bool) -> Self {
            Self {
                requested: Mutex::new(requested),
                clears: AtomicUsize::new(0),
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
        async fn add_user(&self, _content: String) -> Result<()> {
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
    }

    /// Stub `CompactorBackend`: `should_request` returns a fixed gate
    /// value; `request` records the call and returns a receiver that
    /// stays pending (the sender is stashed so the channel neither
    /// resolves nor closes during the orchestrator's 100ms poll).
    struct StubBackend {
        gate: bool,
        request_calls: AtomicUsize,
        senders: Mutex<Vec<oneshot::Sender<CompactionResponse>>>,
    }

    impl StubBackend {
        fn new(gate: bool) -> Self {
            Self {
                gate,
                request_calls: AtomicUsize::new(0),
                senders: Mutex::new(vec![]),
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
        orchestrator: CompactionOrchestrator,
        backend: Arc<StubBackend>,
        session: StubSession,
        events: Arc<Mutex<Vec<String>>>,
    }

    fn fixture(requested: bool, gate: bool) -> Fixture {
        let backend = Arc::new(StubBackend::new(gate));
        let orchestrator = CompactionOrchestrator::new(
            Box::new(ArcBackend(Arc::clone(&backend))),
            CompactionConfig::default(),
            200_000,
        );
        Fixture {
            orchestrator,
            backend,
            session: StubSession::new(requested),
            events: Arc::new(Mutex::new(vec![])),
        }
    }

    /// The orchestrator owns a `Box<dyn CompactorBackend>`; wrap the
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

        f.orchestrator
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();

        assert_eq!(
            f.backend.request_calls.load(Ordering::SeqCst),
            1,
            "forced flag must start compaction even below threshold"
        );
        assert!(
            f.orchestrator.pending_compaction.is_some(),
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

        f.orchestrator
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();

        assert_eq!(f.backend.request_calls.load(Ordering::SeqCst), 0);
        assert!(f.orchestrator.pending_compaction.is_none());
        assert_eq!(f.session.clears.load(Ordering::SeqCst), 0);
        assert!(f.events.lock().expect("events mutex poisoned").is_empty());
    }

    #[tokio::test]
    async fn threshold_triggered_compaction_unchanged_without_flag() {
        let mut f = fixture(false, true); // no flag, gate open (over threshold)
        let mut messages = small_messages();
        let on_event = event_sink(&f.events);

        f.orchestrator
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();

        assert_eq!(f.backend.request_calls.load(Ordering::SeqCst), 1);
        assert!(
            f.orchestrator.pending_compaction.is_some(),
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

    #[tokio::test]
    async fn forced_flag_survives_while_compaction_already_pending() {
        let mut f = fixture(true, false);
        let mut messages = small_messages();
        let on_event = event_sink(&f.events);

        // First call: forced start consumes the flag.
        f.orchestrator
            .check_and_compact(&mut messages, &f.session, &EmptyFunnel, &on_event, "run-1")
            .await
            .unwrap();
        assert_eq!(f.backend.request_calls.load(Ordering::SeqCst), 1);

        // Re-arm the flag while the first compaction is still pending:
        // the orchestrator must NOT start a second compaction and must
        // NOT clear the flag (nothing genuinely started for it).
        *f.session
            .requested
            .lock()
            .expect("requested mutex poisoned") = true;
        f.orchestrator
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
}
