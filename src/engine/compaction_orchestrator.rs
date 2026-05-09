//! Compaction orchestrator for the agentic loop
//!
//! Encapsulates the entire compaction lifecycle:
//! - Config parsing and model context registry setup
//! - Pre-compaction hook invocation
//! - Background compactor coordination
//! - Post-compaction hook invocation
//! - Session recording and cache updates

use crate::compaction::{
    background::{BackgroundCompactor, CompactionResponse},
    registry::ModelContextRegistry,
    CompactionConfig, CompactionResult,
};
use crate::engine::AgenticEvent;
use crate::extension::core::hook_points::HookPoint;
use crate::extension::types::{HookInput, HookOutput, HookResult, SessionSnapshot};
use crate::extension::ExtensionCore;
use crate::providers::Provider;
use crate::session::Session;
use crate::types::message::LlmMessage;
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
}

impl CompactionOrchestrator {
    /// Create a new compaction orchestrator for the given provider and agent config.
    pub fn new(provider: Arc<Provider>, agent_config: &crate::types::AgentConfig) -> Self {
        let config = load_compaction_config();
        let mut registry = ModelContextRegistry::new();
        let override_registry = ModelContextRegistry {
            default_limit: registry.default_limit,
            limits: config.model_limits.clone(),
        };
        registry.merge(&override_registry);

        let provider_str = agent_config.provider.provider_type.to_string();
        let context_window = registry.get(&provider_str, &agent_config.provider.default_model);

        let background_compactor = BackgroundCompactor::new(provider);

        Self {
            background_compactor,
            config,
            context_window,
            pending_compaction: None,
            compaction_performed: false,
            last_compaction_result: None,
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
        let estimated_tokens = crate::compaction::Compactor::estimate_tokens(messages);

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
        }

        Ok(true)
    }

    /// Reset the orchestrator state (e.g., when starting a new run).
    pub fn reset(&mut self) {
        self.pending_compaction = None;
        self.compaction_performed = false;
        self.last_compaction_result = None;
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
            crate::compaction::turn_boundaries::select_messages_respecting_boundaries(
                messages,
                keep_recent_tokens,
            );

        let turn_prefix_messages = if is_split_turn {
            let split_point = messages_to_summarize.len();
            crate::compaction::turn_boundaries::extract_turn_prefix(messages, split_point)
                .unwrap_or_default()
        } else {
            vec![]
        };

        let prev_summary = {
            let s = session.read().await;
            s.load_previous_compaction_summary().await.ok().flatten()
        };

        let file_ops = crate::compaction::summary_format::extract_file_ops_from_messages(
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
        run_id: &str,
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
                context_tokens: crate::compaction::Compactor::estimate_tokens(messages),
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
        .map(|h| h.join(".pekobot").join("config.toml"))
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
