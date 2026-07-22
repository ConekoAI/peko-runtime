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
use crate::session_view::SessionView;
use anyhow::Result;
use peko_extension_api::hook_io::{CompactionPreparationPayload, CompactionResultPayload};
use peko_extension_api::session::SessionSnapshot;
use peko_extension_host::ToolFunnel;
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
    /// 2. Pre-compaction hook invocation
    /// 3. Background compaction initiation
    /// 4. Polling for background compaction completion
    /// 5. Post-compaction hook invocation
    /// 6. Session recording and cache updates
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
        S: SessionView,
    {
        // F21: hybrid estimator. Anchors on the last assistant message
        // with provider-reported usage and char/4-estimates only the
        // trailing slice since that anchor. Falls back to chars/4 across
        // the full conversation when no usage data is available (e.g.
        // pre-F21 JSONL reloads with `usage: None` everywhere).
        let estimated = estimate_context_tokens(messages);
        let estimated_tokens = estimated.tokens;

        // Start background compaction if needed and not already running
        if self.pending_compaction.is_none()
            && self
                .backend
                .should_request(estimated_tokens, self.context_window, &self.config)
                .await
        {
            info!(
                "Context window approaching limit ({} tokens), checking compaction...",
                estimated_tokens
            );
            on_event(AgenticEvent::Thinking {
                run_id: run_id.to_string(),
                text: "Session is getting long. Summarizing older messages...".to_string(),
                is_delta: false,
                is_final: false,
                signature: None,
            });

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
        S: SessionView,
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
        S: SessionView,
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
        S: SessionView,
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
        .map(|(i, m)| (m.usage.clone().unwrap(), i))
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
