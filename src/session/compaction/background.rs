//! Background Compaction Worker
//!
//! Runs compaction asynchronously to avoid blocking the agent loop.
//!
//! Features:
//! - Async compaction in background task
//! - Quotas and cooldowns to prevent excessive compactions
//! - In-flight compaction tracking
//! - Result notification via callback

use crate::common::types::message::LlmMessage;
use crate::quota::{QuotaMeter, QuotaScope};
use crate::session::compaction::{CompactionConfig, Compactor};
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};

/// Returns true if compaction should trigger based on dual-threshold logic.
///
/// Triggers when **either** condition is met:
/// - Ratio-based: `estimated_tokens >= (context_window * auto_threshold_percent / 100)`
/// - Reserved-based: `estimated_tokens >= (context_window - reserve_tokens)`
#[must_use]
fn should_auto_compact(
    estimated_tokens: usize,
    context_window: usize,
    config: &CompactionConfig,
) -> bool {
    if !config.enabled {
        return false;
    }
    // Ratio-based: catches large models early
    let ratio_threshold = (context_window * config.auto_threshold_percent as usize) / 100;
    // Reserved-based: ensures LLM response headroom
    let reserved_threshold = context_window.saturating_sub(config.reserve_tokens);
    estimated_tokens >= ratio_threshold || estimated_tokens >= reserved_threshold
}

/// Quota configuration for background compaction
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct CompactionQuota {
    /// Minimum time between compactions
    pub cooldown_seconds: u64,
    /// Maximum compactions per session
    pub max_compactions_per_session: usize,
    /// Maximum consecutive auto-compactions before requiring manual trigger
    pub max_consecutive_auto: usize,
}

impl Default for CompactionQuota {
    fn default() -> Self {
        Self {
            cooldown_seconds: 60,             // 1 minute cooldown
            max_compactions_per_session: 100, // Generous limit
            max_consecutive_auto: 5,          // Force manual after 5 auto compactions
        }
    }
}

/// Request to background compaction worker
#[derive(Debug)]
pub struct CompactionRequest {
    /// Messages to potentially compact
    pub messages: Vec<LlmMessage>,
    /// Previous summary for cumulative updates
    pub previous_summary: Option<String>,
    /// Response channel for result
    pub response_tx: oneshot::Sender<CompactionResponse>,
}

/// Response from background compaction.
///
/// Phase 9b.N.4: type alias of
/// `peko_engine::compaction::CompactionResponse` so the
/// `CompactorBackend` trait port (defined in `peko-engine`) and
/// the root-owned `BackgroundCompactor` agree on the response
/// type. The variants match exactly:
///
/// - `Completed(CompactionResult)`
/// - `NotNeeded`
/// - `Skipped(String)`
/// - `Failed(String)`
///
/// The historical import path
/// `use crate::session::compaction::background::CompactionResponse`
/// keeps compiling for any pre-9b.N.4 caller. The canonical path is
/// `peko_engine::compaction::CompactionResponse` (re-exported as
/// `crate::session::compaction::CompactionResponse` via the root
/// `compaction.rs` shim).
pub use peko_engine::compaction::CompactionResponse;

/// Background compaction worker handle
#[derive(Debug, Clone)]
pub struct BackgroundCompactor {
    /// Sender to worker task
    request_tx: mpsc::Sender<CompactionRequest>,
    /// Current state (for quota tracking)
    state: Arc<Mutex<WorkerState>>,
    /// Quota configuration
    quota: CompactionQuota,
}

/// Internal worker state
#[derive(Debug)]
struct WorkerState {
    /// Last compaction time
    last_compaction: Option<Instant>,
    /// Number of compactions this session
    compaction_count: usize,
    /// Number of consecutive auto-compactions
    consecutive_auto: usize,
    /// Whether compaction is currently in progress
    is_compacting: bool,
}

impl BackgroundCompactor {
    /// Create a new background compactor with the given provider.
    ///
    /// F19: `meter` is the principal's quota meter. The spawned worker
    /// task opens a [`QuotaScope::with`] around every LLM call so the
    /// summarization call goes through a [`MeteredProvider`] and
    /// auto-charges. Pass [`QuotaMeter::unlimited()`] for unquota'd
    /// sessions (CLI / tests / legacy one-shots).
    ///
    /// F20: `peer_meter` is an optional per-peer meter. When `Some`,
    /// the worker task opens a nested `QuotaScope::with(peer_meter, ...)`
    /// inside the principal scope so every summarization call charges
    /// BOTH meters via [`StackedMeteredProvider`]. `None` falls back
    /// to plain `MeteredProvider` (F19 behavior).
    pub fn new(
        provider: Arc<crate::providers::Provider>,
        meter: Arc<QuotaMeter>,
        peer_meter: Option<Arc<QuotaMeter>>,
    ) -> Self {
        let (request_tx, mut request_rx) = mpsc::channel::<CompactionRequest>(4);
        let state = Arc::new(Mutex::new(WorkerState {
            last_compaction: None,
            compaction_count: 0,
            consecutive_auto: 0,
            is_compacting: false,
        }));

        let state_clone = state.clone();
        let meter_clone = Arc::clone(&meter);

        // Spawn background worker task. We wrap the loop body in
        // `QuotaScope::with` because `tokio::spawn` does NOT inherit
        // the parent task's task-local — see `quota::scope` docstring.
        //
        // F20: when peer_meter is `Some`, nest it inside the principal
        // scope so the worker picks up both via `QuotaScope::collect_stack`.
        tokio::spawn(async move {
            let worker_body = async move {
                debug!("Background compaction worker started");

                while let Some(request) = request_rx.recv().await {
                    let provider = provider.clone();
                    let state = state_clone.clone();

                    // Process compaction request — the inner
                    // `Compactor::compact` builds a stacked metered
                    // provider via `StackedMeteredProvider::from_current_scope`
                    // so the summarization LLM call auto-charges every
                    // meter in the active stack (peer innermost →
                    // principal outermost, peer trip fires first).
                    let result = process_compaction_request(request, provider, state).await;

                    if let Err(e) = result {
                        error!("Background compaction error: {}", e);
                    }
                }

                debug!("Background compaction worker stopped");
            };
            match peer_meter {
                Some(pm) => {
                    QuotaScope::with(meter_clone, async move {
                        QuotaScope::with(pm, worker_body).await
                    })
                    .await;
                }
                None => {
                    QuotaScope::with(meter_clone, worker_body).await;
                }
            }
        });

        Self {
            request_tx,
            state,
            quota: CompactionQuota::default(),
        }
    }

    /// Create with custom config and quota
    #[allow(dead_code)]
    pub fn with_config(
        provider: Arc<crate::providers::Provider>,
        config: CompactionConfig,
        quota: CompactionQuota,
        meter: Arc<QuotaMeter>,
        peer_meter: Option<Arc<QuotaMeter>>,
    ) -> Self {
        let (request_tx, mut request_rx) = mpsc::channel::<CompactionRequest>(4);
        let state = Arc::new(Mutex::new(WorkerState {
            last_compaction: None,
            compaction_count: 0,
            consecutive_auto: 0,
            is_compacting: false,
        }));

        let state_clone = state.clone();
        let meter_clone = Arc::clone(&meter);

        // Spawn background worker task with custom config. Same
        // `QuotaScope::with` wrap as `new` — see comment there.
        // F20: nest peer scope when peer_meter is `Some`.
        tokio::spawn(async move {
            let worker_body = async move {
                debug!("Background compaction worker started (custom config)");

                while let Some(request) = request_rx.recv().await {
                    let provider = provider.clone();
                    let state = state_clone.clone();
                    let config = config.clone();

                    // Process compaction request with custom config
                    let result =
                        process_compaction_request_with_config(request, provider, state, config)
                            .await;

                    if let Err(e) = result {
                        error!("Background compaction error: {}", e);
                    }
                }

                debug!("Background compaction worker stopped");
            };
            match peer_meter {
                Some(pm) => {
                    QuotaScope::with(meter_clone, async move {
                        QuotaScope::with(pm, worker_body).await
                    })
                    .await;
                }
                None => {
                    QuotaScope::with(meter_clone, worker_body).await;
                }
            }
        });

        Self {
            request_tx,
            state,
            quota,
        }
    }

    /// Create with custom config, quota, and an explicit context window.
    /// The context window is passed through to the compactor for threshold checks.
    #[allow(dead_code)]
    pub fn with_config_and_window(
        provider: Arc<crate::providers::Provider>,
        config: CompactionConfig,
        quota: CompactionQuota,
        meter: Arc<QuotaMeter>,
        peer_meter: Option<Arc<QuotaMeter>>,
        _context_window: usize,
    ) -> Self {
        // For now, the context window is used by the caller when calling
        // should_request(). The compactor itself uses the config values.
        Self::with_config(provider, config, quota, meter, peer_meter)
    }

    /// Request compaction (non-blocking)
    /// Returns receiver for result
    pub async fn request_compaction(
        &self,
        messages: Vec<LlmMessage>,
        previous_summary: Option<String>,
    ) -> Result<oneshot::Receiver<CompactionResponse>> {
        let (response_tx, response_rx) = oneshot::channel();

        let request = CompactionRequest {
            messages,
            previous_summary,
            response_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| anyhow::anyhow!("Background worker channel closed"))?;

        Ok(response_rx)
    }

    /// Check if compaction should be requested (quota check)
    pub async fn should_request(
        &self,
        estimated_tokens: usize,
        context_window: usize,
        config: &CompactionConfig,
    ) -> bool {
        // First check if enabled and over threshold
        if !config.enabled {
            return false;
        }

        if !should_auto_compact(estimated_tokens, context_window, config) {
            return false;
        }

        // Check quotas
        let state = self.state.lock().await;

        // Check max compactions per session (prefer config value, fall back to quota)
        let max_compactions = config.max_compactions_per_session;
        if state.compaction_count >= max_compactions {
            warn!(
                "Compaction quota exceeded: {} >= {}",
                state.compaction_count, max_compactions
            );
            return false;
        }

        // Check cooldown (prefer config value, fall back to quota)
        let cooldown = config.cooldown_seconds;
        if let Some(last) = state.last_compaction {
            let elapsed = last.elapsed().as_secs();
            if elapsed < cooldown {
                debug!("Compaction on cooldown: {}s remaining", cooldown - elapsed);
                return false;
            }
        }

        // Check if compaction already in progress
        if state.is_compacting {
            debug!("Compaction already in progress");
            return false;
        }

        // Check consecutive auto limit
        if state.consecutive_auto >= self.quota.max_consecutive_auto {
            warn!(
                "Max consecutive auto-compactions reached: {}",
                state.consecutive_auto
            );
            return false;
        }

        true
    }

    /// Get current worker status
    #[allow(dead_code)]
    pub async fn status(&self) -> String {
        let state = self.state.lock().await;
        let cooldown_remaining = state
            .last_compaction
            .map(|last| {
                let elapsed = last.elapsed().as_secs();
                if elapsed < self.quota.cooldown_seconds {
                    format!("{}s", self.quota.cooldown_seconds - elapsed)
                } else {
                    "ready".to_string()
                }
            })
            .unwrap_or_else(|| "ready".to_string());

        format!(
            "🧹 Compactions: {} | Consecutive auto: {} | Cooldown: {} | In progress: {}",
            state.compaction_count,
            state.consecutive_auto,
            cooldown_remaining,
            if state.is_compacting { "yes" } else { "no" }
        )
    }

    /// Reset consecutive auto counter (call after successful manual compaction)
    #[allow(dead_code)]
    pub async fn reset_consecutive(&self) {
        let mut state = self.state.lock().await;
        state.consecutive_auto = 0;
    }
}

/// Process a compaction request (default config)
async fn process_compaction_request(
    request: CompactionRequest,
    provider: Arc<crate::providers::Provider>,
    state: Arc<Mutex<WorkerState>>,
) -> Result<()> {
    process_compaction_request_with_config(request, provider, state, CompactionConfig::default())
        .await
}

/// Process a compaction request with custom config
async fn process_compaction_request_with_config(
    request: CompactionRequest,
    provider: Arc<crate::providers::Provider>,
    state: Arc<Mutex<WorkerState>>,
    config: CompactionConfig,
) -> Result<()> {
    // Mark as in progress
    {
        let mut s = state.lock().await;
        s.is_compacting = true;
    }

    // Ensure we mark as not compacting when done
    let _guard = scopeguard::guard(state.clone(), |s| {
        let s = s.clone();
        tokio::spawn(async move {
            let mut state = s.lock().await;
            state.is_compacting = false;
        });
    });

    // Check if compaction is actually needed.
    // The caller is responsible for passing the correct context_window to
    // should_request(). Here we just verify the message list is long enough.
    // (F21 removed the dead `let _estimated_tokens = Compactor::estimate_tokens(...)`
    // line that was here — the variable was computed and never read; the
    // real trigger gating is `should_request` upstream.)
    if request.messages.len() < 4 {
        let _ = request.response_tx.send(CompactionResponse::NotNeeded);
        return Ok(());
    }

    // Perform compaction. The worker task is already inside a
    // `QuotaScope::with` (see `BackgroundCompactor::new`/`with_config`),
    // so `Compactor::compact` builds its own `MeteredProvider` from
    // the active task-local inside `generate_summary_with_llm`. The
    // summarization LLM call then auto-charges.
    let mut compactor = Compactor::with_config(config, request.previous_summary.clone());

    match compactor.compact(&request.messages, &provider).await {
        Ok(result) => {
            // Update state
            {
                let mut s = state.lock().await;
                s.last_compaction = Some(Instant::now());
                s.compaction_count += 1;
                s.consecutive_auto += 1;
            }

            info!(
                "Background compaction #{} completed: {} messages → summary",
                result.state.compaction_count, result.entry.messages_compacted
            );

            let _ = request
                .response_tx
                .send(CompactionResponse::Completed(result));
        }
        Err(e) => {
            error!("Background compaction failed: {}", e);
            let _ = request
                .response_tx
                .send(CompactionResponse::Failed(e.to_string()));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compaction_quota_default() {
        let quota = CompactionQuota::default();
        assert_eq!(quota.cooldown_seconds, 60);
        assert_eq!(quota.max_compactions_per_session, 100);
        assert_eq!(quota.max_consecutive_auto, 5);
    }

    #[test]
    fn should_auto_compact_ratio_threshold_fires() {
        let config = CompactionConfig {
            enabled: true,
            auto_threshold_percent: 85,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            ..CompactionConfig::default()
        };
        // Large model: 1M context, 860K tokens → 86% → ratio threshold fires.
        assert!(should_auto_compact(860_000, 1_000_000, &config));
        // Well under ratio.
        assert!(!should_auto_compact(500_000, 1_000_000, &config));
    }

    #[test]
    fn should_auto_compact_reserved_threshold_fires() {
        let config = CompactionConfig {
            enabled: true,
            auto_threshold_percent: 85,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            ..CompactionConfig::default()
        };
        // Standard model: 128K context, 115K tokens → below 85% ratio
        // (108.8K) but above reserved threshold (128K - 16K = 112K).
        assert!(should_auto_compact(115_000, 128_000, &config));
        // Well under both.
        assert!(!should_auto_compact(100_000, 128_000, &config));
    }

    #[test]
    fn should_auto_compact_respects_enabled_flag() {
        let config = CompactionConfig {
            enabled: false,
            ..CompactionConfig::default()
        };
        assert!(!should_auto_compact(1_000_000, 128_000, &config));
    }
}
