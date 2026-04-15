//! Background Compaction Worker
//!
//! Runs compaction asynchronously to avoid blocking the agent loop.
//!
//! Features:
//! - Async compaction in background task
//! - Quotas and cooldowns to prevent excessive compactions
//! - In-flight compaction tracking
//! - Result notification via callback

use crate::compaction::{CompactionConfig, CompactionResult, Compactor};
use crate::providers::ChatMessage;
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};

/// Quota configuration for background compaction
#[derive(Debug, Clone, Copy)]
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
    pub messages: Vec<ChatMessage>,
    /// Previous summary for cumulative updates
    pub previous_summary: Option<String>,
    /// Response channel for result
    pub response_tx: oneshot::Sender<CompactionResponse>,
}

/// Response from background compaction
#[derive(Debug, Clone)]
pub enum CompactionResponse {
    /// Compaction completed successfully
    Completed(CompactionResult),
    /// Compaction not needed (under threshold)
    NotNeeded,
    /// Compaction skipped due to quota/cooldown
    Skipped(String),
    /// Compaction failed
    Failed(String),
}

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
    /// Create a new background compactor with the given provider
    pub fn new(provider: Arc<crate::providers::Provider>) -> Self {
        let (request_tx, mut request_rx) = mpsc::channel::<CompactionRequest>(4);
        let state = Arc::new(Mutex::new(WorkerState {
            last_compaction: None,
            compaction_count: 0,
            consecutive_auto: 0,
            is_compacting: false,
        }));

        let state_clone = state.clone();

        // Spawn background worker task
        tokio::spawn(async move {
            debug!("Background compaction worker started");

            while let Some(request) = request_rx.recv().await {
                let provider = provider.clone();
                let state = state_clone.clone();

                // Process compaction request
                let result = process_compaction_request(request, provider, state).await;

                if let Err(e) = result {
                    error!("Background compaction error: {}", e);
                }
            }

            debug!("Background compaction worker stopped");
        });

        Self {
            request_tx,
            state,
            quota: CompactionQuota::default(),
        }
    }

    /// Create with custom config and quota
    pub fn with_config(
        provider: Arc<crate::providers::Provider>,
        config: CompactionConfig,
        quota: CompactionQuota,
    ) -> Self {
        let (request_tx, mut request_rx) = mpsc::channel::<CompactionRequest>(4);
        let state = Arc::new(Mutex::new(WorkerState {
            last_compaction: None,
            compaction_count: 0,
            consecutive_auto: 0,
            is_compacting: false,
        }));

        let state_clone = state.clone();

        // Spawn background worker task with custom config
        tokio::spawn(async move {
            debug!("Background compaction worker started (custom config)");

            while let Some(request) = request_rx.recv().await {
                let provider = provider.clone();
                let state = state_clone.clone();
                let config = config.clone();

                // Process compaction request with custom config
                let result =
                    process_compaction_request_with_config(request, provider, state, config).await;

                if let Err(e) = result {
                    error!("Background compaction error: {}", e);
                }
            }

            debug!("Background compaction worker stopped");
        });

        Self {
            request_tx,
            state,
            quota,
        }
    }

    /// Request compaction (non-blocking)
    /// Returns receiver for result
    pub async fn request_compaction(
        &self,
        messages: Vec<ChatMessage>,
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
    pub async fn should_request(&self, estimated_tokens: usize, config: &CompactionConfig) -> bool {
        // First check if enabled and over threshold
        if !config.enabled {
            return false;
        }

        let threshold = config
            .context_window_tokens
            .saturating_sub(config.reserve_tokens + config.keep_recent_tokens);

        if estimated_tokens < threshold {
            return false;
        }

        // Check quotas
        let state = self.state.lock().await;

        // Check max compactions per session
        if state.compaction_count >= self.quota.max_compactions_per_session {
            warn!(
                "Compaction quota exceeded: {} >= {}",
                state.compaction_count, self.quota.max_compactions_per_session
            );
            return false;
        }

        // Check cooldown
        if let Some(last) = state.last_compaction {
            let elapsed = last.elapsed().as_secs();
            if elapsed < self.quota.cooldown_seconds {
                debug!(
                    "Compaction on cooldown: {}s remaining",
                    self.quota.cooldown_seconds - elapsed
                );
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

    // Check if compaction is actually needed
    let estimated_tokens = Compactor::estimate_tokens(&request.messages);
    let threshold = config
        .context_window_tokens
        .saturating_sub(config.reserve_tokens + config.keep_recent_tokens);

    if estimated_tokens < threshold {
        let _ = request.response_tx.send(CompactionResponse::NotNeeded);
        return Ok(());
    }

    // Perform compaction
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
}
