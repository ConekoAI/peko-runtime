//! Compaction orchestrator for the agentic loop
//!
//! Encapsulates the entire compaction lifecycle:
//! - Pre-compaction hook invocation
//! - Background compactor coordination
//! - Post-compaction hook invocation
//! - Session recording and cache updates
//!
//! The orchestrator no longer owns a "model context registry". The
//! single source of truth for the model's max context length is
//! `ModelInfo::context_length` in the `ProviderCatalog`. The caller
//! resolves that value once before constructing the orchestrator and
//! passes it as a concrete `usize` — see `AgenticLoop::run_inner`
//! where the orchestrator is built.

use crate::common::types::message::LlmMessage;
use crate::engine::AgenticEvent;
use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::types::{HookInput, HookOutput, HookResult, SessionSnapshot};
use crate::extensions::framework::ExtensionCore;
use crate::providers::Provider;
use crate::session::compaction::{
    background::{BackgroundCompactor, CompactionResponse},
    CompactionConfig, CompactionResult,
};
use crate::session::Session;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Orchestrates compaction within the agentic loop.
///
/// The loop just calls `check_and_compact()` at the start of each iteration.
/// All complexity (hooks, background tasks, session updates) is encapsulated here.
pub struct CompactionOrchestrator {
    background_compactor: BackgroundCompactor,
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
    last_compaction_usage: Option<crate::providers::TokenUsage>,
}

impl CompactionOrchestrator {
    /// Create a new compaction orchestrator for the given provider and
    /// the model's max context length in tokens.
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
    pub fn new(provider: Arc<Provider>, context_window: usize) -> Self {
        let config = load_compaction_config();

        let background_compactor = BackgroundCompactor::new(provider);

        Self {
            background_compactor,
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
    pub async fn check_and_compact(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        session: &Arc<RwLock<Session>>,
        extension_core: &Arc<ExtensionCore>,
        on_event: &(dyn Fn(AgenticEvent) + Send + Sync),
        run_id: &str,
    ) -> Result<bool> {
        let estimated_tokens = crate::session::compaction::Compactor::estimate_tokens(messages);

        // Start background compaction if needed and not already running
        if self.pending_compaction.is_none()
            && self
                .background_compactor
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

            self.invoke_pre_hook(messages, session, extension_core, estimated_tokens)
                .await;
        }

        // Check if background compaction has completed
        self.poll_background_compaction(messages, session).await;

        // Post-compaction hook and cleanup
        if self.compaction_performed {
            self.invoke_post_hook(messages, session, extension_core, run_id)
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
    pub fn last_compaction_usage(&mut self) -> Option<crate::providers::TokenUsage> {
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

    async fn invoke_pre_hook(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        session: &Arc<RwLock<Session>>,
        extension_core: &Arc<ExtensionCore>,
        estimated_tokens: usize,
    ) {
        let _ = estimated_tokens;
        let threshold_tokens = self
            .context_window
            .saturating_sub(self.config.reserve_tokens);
        let keep_recent_tokens = self.config.keep_recent_tokens;

        let _ = threshold_tokens;

        let (messages_to_summarize, _messages_to_keep, is_split_turn) =
            crate::session::compaction::turn_boundaries::select_messages_respecting_boundaries(
                messages,
                keep_recent_tokens,
            );

        let turn_prefix_messages = if is_split_turn {
            let split_point = messages_to_summarize.len();
            crate::session::compaction::turn_boundaries::extract_turn_prefix(messages, split_point)
                .unwrap_or_default()
        } else {
            vec![]
        };

        let prev_summary = {
            let s = session.read().await;
            s.load_previous_compaction_summary().await.ok().flatten()
        };

        let file_ops = crate::session::compaction::summary_format::extract_file_ops_from_messages(
            &messages_to_summarize,
        );

        let hook_input = HookInput::CompactionPreparation {
            messages_to_summarize,
            turn_prefix_messages,
            is_split_turn,
            previous_summary: prev_summary.clone(),
            file_ops,
            estimated_tokens,
            threshold_tokens,
            model_context_limit: self.context_window,
            settings: self.config.clone(),
        };

        let hook_result = extension_core
            .invoke_hook(HookPoint::SessionCompaction, hook_input)
            .await;

        match hook_result {
            HookResult::Replace(HookOutput::MessageVec(custom_messages)) => {
                info!(
                    "SessionCompaction hook replaced messages: {} → {}",
                    messages.len(),
                    custom_messages.len()
                );
                *messages = custom_messages;
                self.compaction_performed = true;
            }
            HookResult::Handled => {
                info!("SessionCompaction hook cancelled compaction");
            }
            _ => {
                // PassThrough or other — run built-in background compactor
                match self
                    .background_compactor
                    .request_compaction(messages.clone(), prev_summary)
                    .await
                {
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

    async fn poll_background_compaction(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        session: &Arc<RwLock<Session>>,
    ) {
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
                            {
                                let mut s = session.write().await;
                                if let Err(e) = s
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

    async fn invoke_post_hook(
        &mut self,
        messages: &mut Vec<LlmMessage>,
        session: &Arc<RwLock<Session>>,
        extension_core: &Arc<ExtensionCore>,
        _run_id: &str,
    ) {
        let session_id = {
            let s = session.read().await;
            s.id.clone()
        };

        let post_input = if let Some(ref result) = self.last_compaction_result {
            HookInput::CompactionResult {
                summary: result.entry.summary.clone(),
                messages_compacted: result.entry.messages_compacted,
                tokens_before: result.entry.tokens_before,
                tokens_after: result.entry.tokens_after,
                compaction_number: result.entry.compaction_number,
                details: result.entry.details.clone(),
                messages_after: messages.clone(),
            }
        } else {
            HookInput::SessionState(SessionSnapshot {
                session_id,
                message_count: messages.len(),
                context_tokens: crate::session::compaction::Compactor::estimate_tokens(messages),
                metadata: HashMap::new(),
            })
        };

        let post_result = extension_core
            .invoke_hook(HookPoint::SessionCompactionPost, post_input)
            .await;

        if let HookResult::Replace(HookOutput::MessageVec(modified)) = post_result {
            info!(
                "SessionCompactionPost hook modified messages: {} → {}",
                messages.len(),
                modified.len()
            );
            *messages = modified;
        }

        // Update context cache after compaction
        {
            let s = session.read().await;
            if let Err(e) = s.update_context_cache(messages).await {
                warn!("Failed to update context cache: {}", e);
            }
        }
    }
}

/// Load compaction config from the global config file, or use defaults.
fn load_compaction_config() -> CompactionConfig {
    let config_path = dirs::home_dir()
        .map(|h| h.join(".peko").join("config.toml"))
        .filter(|p| p.exists());

    if let Some(path) = config_path {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(root) = toml::from_str::<toml::Value>(&contents) {
                if let Some(compaction_table) = root.get("compaction") {
                    if let Ok(cfg) = compaction_table.clone().try_into::<CompactionConfig>() {
                        return cfg;
                    }
                }
            }
        }
    }

    CompactionConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_compaction_config_defaults() {
        // When no config file exists, we should get defaults
        let cfg = load_compaction_config();
        assert!(cfg.enabled);
        assert_eq!(cfg.auto_threshold_percent, 85);
    }
}
