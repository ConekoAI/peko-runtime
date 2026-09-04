//! Background Compaction Worker
//!
//! Runs compaction asynchronously to avoid blocking the agent loop.
//!
//! Features:
//! - Async compaction in background task
//! - Quotas and cooldowns to prevent excessive compactions
//! - In-flight compaction tracking
//! - Result notification via callback

use crate::compaction::{CompactionConfig, Compactor};
use anyhow::Result;
use peko_message::LlmMessage;
use peko_providers::ProviderView;
use peko_quota::{QuotaMeter, QuotaScope};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};

/// Returns true if compaction should trigger based on dual-threshold logic.
///
/// Triggers when **either** condition is met:
/// - Ratio-based: `estimated_tokens >= (context_window * auto_threshold_percent / 100)`
/// - Reserved-based: `estimated_tokens >= (context_window - reserve_tokens)`
#[must_use]
pub fn should_auto_compact(
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
    /// Which phase triggered this compaction (PR 3). The worker
    /// forwards this to `Compactor::compact` so the resulting
    /// `CompactionEntry.phase` and `CompactionDetails.phase` carry
    /// the originating trigger.
    pub phase: crate::compaction::types::CompactionPhase,
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
/// `use crate::compaction::background::CompactionResponse`
/// keeps compiling for any pre-9b.N.4 caller. The canonical path is
/// `peko_engine::compaction::CompactionResponse` (re-exported as
/// `crate::session::compaction::CompactionResponse` via the root
/// `compaction.rs` shim).
pub use crate::compaction::types::CompactionResponse;

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
///
/// Compaction audit fix #4: the quota-relevant fields are persisted
/// on the session index entry (see
/// [`crate::compaction::CompactionLimitsState`]) and hydrated into a
/// fresh `WorkerState` at every run start, so
/// `max_compactions_per_session` / `cooldown_seconds` /
/// `max_consecutive_auto` are per-SESSION limits rather than
/// per-run. `last_compaction` is a `SystemTime` (not `Instant`) so
/// it round-trips through the persisted epoch-millis timestamp.
#[derive(Debug)]
struct WorkerState {
    /// Last compaction attempt (success or failure)
    last_compaction: Option<SystemTime>,
    /// Number of compactions this session (hydrated per run)
    compaction_count: usize,
    /// Number of consecutive auto-compactions (hydrated per run)
    consecutive_auto: usize,
    /// Number of consecutive failed compaction attempts. Drives the
    /// escalating failure backoff in `should_request`; reset on
    /// success. Hydrated per run.
    consecutive_failures: u32,
    /// Whether compaction is currently in progress (never persisted —
    /// a crashed run leaves no in-flight compaction)
    is_compacting: bool,
}

/// Cap on the escalating failure backoff (15 minutes).
const MAX_FAILURE_BACKOFF_SECONDS: u64 = 15 * 60;

/// Cooldown that applies after `consecutive_failures` failed attempts:
/// the base cooldown doubled per consecutive failure, capped at
/// [`MAX_FAILURE_BACKOFF_SECONDS`]. Zero failures yields the base
/// cooldown unchanged.
fn failure_backoff_seconds(cooldown_seconds: u64, consecutive_failures: u32) -> u64 {
    // Clamp the shift so the multiplier can't overflow before the cap
    // applies (any base cooldown × 2^10 already exceeds the cap).
    let shift = consecutive_failures.min(10);
    cooldown_seconds
        .saturating_mul(1u64 << shift)
        .min(MAX_FAILURE_BACKOFF_SECONDS)
}

impl BackgroundCompactor {
    /// Create a new background compactor with the given provider.
    ///
    /// F19: `meter` is the principal's quota meter. The spawned
    /// worker task opens a [`QuotaScope::with`] around every LLM
    /// call so the summarization call goes through a
    /// [`MeteredProvider`] and auto-charges. Pass
    /// [`QuotaMeter::unlimited()`] for unquota'd sessions
    /// (CLI / tests / legacy one-shots).
    ///
    /// B5 (2026-08-22): the F20 `peer_meter` parameter was removed
    /// — peer attribution was broken for agents serving many peers
    /// simultaneously. Per-agent attribution (the `agent_meter` on
    /// `SubagentExecutor`) replaces it; compactor LLM calls now only
    /// charge the principal meter.
    pub fn new(
        provider: Arc<dyn ProviderView>,
        meter: Arc<QuotaMeter>,
    ) -> Self {
        let (request_tx, mut request_rx) = mpsc::channel::<CompactionRequest>(4);
        let state = Arc::new(Mutex::new(WorkerState {
            last_compaction: None,
            compaction_count: 0,
            consecutive_auto: 0,
            consecutive_failures: 0,
            is_compacting: false,
        }));

        let state_clone = state.clone();
        let meter_clone = Arc::clone(&meter);

        // Spawn background worker task. We wrap the loop body in
        // `QuotaScope::with` because `tokio::spawn` does NOT inherit
        // the parent task's task-local — see `quota::scope` docstring.
        tokio::spawn(async move {
            let worker_body = async move {
                debug!("Background compaction worker started");

                while let Some(request) = request_rx.recv().await {
                    let provider = provider.clone();
                    let state = state_clone.clone();

                    // Process compaction request — the inner
                    // `Compactor::compact` builds a
                    // `MeteredProvider` via
                    // `StackedMeteredProvider::from_current_scope`
                    // so the summarization LLM call auto-charges
                    // the principal meter in the active
                    // task-local.
                    let result = process_compaction_request(request, provider, state).await;

                    if let Err(e) = result {
                        error!("Background compaction error: {}", e);
                    }
                }

                debug!("Background compaction worker stopped");
            };
            QuotaScope::with(meter_clone, worker_body).await;
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
        provider: Arc<dyn ProviderView>,
        config: CompactionConfig,
        quota: CompactionQuota,
        meter: Arc<QuotaMeter>,
    ) -> Self {
        let (request_tx, mut request_rx) = mpsc::channel::<CompactionRequest>(4);
        let state = Arc::new(Mutex::new(WorkerState {
            last_compaction: None,
            compaction_count: 0,
            consecutive_auto: 0,
            consecutive_failures: 0,
            is_compacting: false,
        }));

        let state_clone = state.clone();
        let meter_clone = Arc::clone(&meter);

        // Spawn background worker task with custom config. Same
        // `QuotaScope::with` wrap as `new` — see comment there.
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
            QuotaScope::with(meter_clone, worker_body).await;
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
        provider: Arc<peko_providers::Provider>,
        config: CompactionConfig,
        quota: CompactionQuota,
        meter: Arc<QuotaMeter>,
        _context_window: usize,
    ) -> Self {
        // For now, the context window is used by the caller when calling
        // should_request(). The compactor itself uses the config values.
        Self::with_config(provider, config, quota, meter)
    }

    /// Request compaction (non-blocking)
    /// Returns receiver for result
    pub async fn request_compaction(
        &self,
        messages: Vec<LlmMessage>,
        previous_summary: Option<String>,
        phase: crate::compaction::types::CompactionPhase,
    ) -> Result<oneshot::Receiver<CompactionResponse>> {
        let (response_tx, response_rx) = oneshot::channel();

        let request = CompactionRequest {
            messages,
            previous_summary,
            response_tx,
            phase,
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

        // Check cooldown (prefer config value, fall back to quota).
        // Failures count as attempts too — `last_compaction` is
        // stamped on both success and failure — and consecutive
        // failures escalate the cooldown exponentially so a
        // persistently failing provider doesn't burn a summarization
        // call per loop iteration.
        let cooldown = failure_backoff_seconds(config.cooldown_seconds, state.consecutive_failures);
        if let Some(last) = state.last_compaction {
            // `elapsed` errs when the persisted stamp is in the
            // future (clock skew across runs); treat that as "just
            // happened" so the cooldown still applies — the
            // conservative direction (never spam summarization
            // calls).
            let elapsed = last.elapsed().unwrap_or(Duration::ZERO).as_secs();
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
                let elapsed = last.elapsed().unwrap_or(Duration::ZERO).as_secs();
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

    /// Hydrate the worker quota state from the persisted per-session
    /// snapshot (compaction audit fix #4).
    ///
    /// The compaction driver calls this once per run, before the
    /// first `should_request` gate, so the quota / cooldown /
    /// consecutive limits continue across runs of the same session
    /// instead of restarting cold.
    pub async fn hydrate_limits_state(&self, persisted: crate::compaction::CompactionLimitsState) {
        let mut state = self.state.lock().await;
        state.compaction_count = persisted.compaction_count as usize;
        state.last_compaction = persisted
            .last_compaction_at_ms
            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms));
        state.consecutive_auto = persisted.consecutive_auto as usize;
        state.consecutive_failures = persisted.consecutive_failures;
    }

    /// Snapshot the current worker quota state so the driver can
    /// persist it onto the session index entry (compaction audit
    /// fix #4). Called after every worker mutation (success or
    /// failure).
    pub async fn limits_state(&self) -> crate::compaction::CompactionLimitsState {
        let state = self.state.lock().await;
        crate::compaction::CompactionLimitsState {
            compaction_count: u32::try_from(state.compaction_count).unwrap_or(u32::MAX),
            last_compaction_at_ms: state
                .last_compaction
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            consecutive_auto: u32::try_from(state.consecutive_auto).unwrap_or(u32::MAX),
            consecutive_failures: state.consecutive_failures,
        }
    }
}

/// Process a compaction request (default config)
async fn process_compaction_request(
    request: CompactionRequest,
    provider: Arc<dyn ProviderView>,
    state: Arc<Mutex<WorkerState>>,
) -> Result<()> {
    process_compaction_request_with_config(request, provider, state, CompactionConfig::default())
        .await
}

/// Process a compaction request with custom config
async fn process_compaction_request_with_config(
    request: CompactionRequest,
    provider: Arc<dyn ProviderView>,
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

    match compactor
        .compact(&request.messages, &provider, request.phase)
        .await
    {
        Ok(mut result) => {
            // Update state
            {
                let mut s = state.lock().await;
                s.last_compaction = Some(SystemTime::now());
                s.compaction_count += 1;
                s.consecutive_auto += 1;
                s.consecutive_failures = 0;

                // Compaction audit fix #4: the per-request `Compactor`
                // numbers its entry from its own fresh state (always
                // 1); renumber from the worker's (session-hydrated)
                // count so the JSONL boundary event's
                // `compaction_number` is a per-session sequence that
                // continues across runs.
                result.entry.compaction_number = s.compaction_count;
                result.state.compaction_count = s.compaction_count;
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
            // Record the attempt so the cooldown gate applies to
            // failures too; consecutive failures escalate it via
            // `failure_backoff_seconds`.
            {
                let mut s = state.lock().await;
                s.last_compaction = Some(SystemTime::now());
                s.consecutive_failures += 1;
            }
            let _ = request
                .response_tx
                .send(CompactionResponse::Failed(e.to_string()));
        }
    }

    Ok(())
}

// ============================================================================
// Phase 7: lifted impl — `BackgroundCompactor` now lives in this crate so
// the trait impl can sit alongside it. Pre-Phase-7 this was the
// `src/engine/compaction_backend_compat.rs` orphan-rule workaround.
// ============================================================================

#[async_trait::async_trait]
impl crate::compaction::CompactorBackend for BackgroundCompactor {
    async fn should_request(
        &self,
        estimated_tokens: usize,
        context_window: usize,
        config: &crate::compaction::types::CompactionConfig,
    ) -> bool {
        BackgroundCompactor::should_request(self, estimated_tokens, context_window, config).await
    }

    async fn request(
        &self,
        request: crate::compaction::types::CompactionRequest,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<crate::compaction::types::CompactionResponse>>
    {
        // Forward the public-shape fields directly to
        // `BackgroundCompactor::request_compaction`, which creates its
        // own `(response_tx, response_rx)` oneshot pair and returns the
        // receiver. The trait port deliberately omits `response_tx` so
        // the lifted orchestrator doesn't have to construct a sender it
        // never uses. PR 3: also forward `phase` so the resulting
        // `CompactionEntry` carries the originating trigger.
        BackgroundCompactor::request_compaction(
            self,
            request.messages,
            request.previous_summary,
            request.phase,
        )
        .await
    }

    async fn hydrate_limits_state(&self, state: crate::compaction::CompactionLimitsState) {
        BackgroundCompactor::hydrate_limits_state(self, state).await;
    }

    async fn limits_state(&self) -> crate::compaction::CompactionLimitsState {
        BackgroundCompactor::limits_state(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_providers::adapters::AnyAdapter;
    use peko_providers::core::ProviderRuntimeOptions;
    use peko_providers::mock::MockAdapter;
    use peko_providers::Provider;

    fn mock_provider(mock: &MockAdapter) -> Arc<dyn ProviderView> {
        Arc::new(
            Provider::new(
                AnyAdapter::Mock(mock.clone()),
                "",
                ProviderRuntimeOptions {
                    default_model_id: "mock-model".to_string(),
                    context_window: None,
                    timeout_seconds: 300,
                    max_retries: 0,
                    retry_delay_ms: 0,
                    ..Default::default()
                },
            )
            .expect("mock provider should construct"),
        )
    }

    /// Long-enough history that `Compactor::compact` actually calls
    /// the LLM (same shape as the `compaction_top` tests).
    fn test_messages() -> Vec<LlmMessage> {
        let mut messages = vec![LlmMessage::system("You are a helpful assistant.")];
        for i in 0..30 {
            if i % 2 == 0 {
                messages.push(LlmMessage::user(format!("User message {i}")));
            } else {
                messages.push(LlmMessage::assistant(format!(
                    "Assistant response {i} with some additional text to make it longer"
                )));
            }
        }
        messages
    }

    fn test_request() -> (CompactionRequest, oneshot::Receiver<CompactionResponse>) {
        let (response_tx, rx) = oneshot::channel();
        (
            CompactionRequest {
                messages: test_messages(),
                previous_summary: None,
                response_tx,
                phase: crate::compaction::types::CompactionPhase::PreTurn,
            },
            rx,
        )
    }

    #[test]
    fn failure_backoff_doubles_per_consecutive_failure_and_caps() {
        assert_eq!(failure_backoff_seconds(60, 0), 60);
        assert_eq!(failure_backoff_seconds(60, 1), 120);
        assert_eq!(failure_backoff_seconds(60, 2), 240);
        assert_eq!(failure_backoff_seconds(60, 3), 480);
        assert_eq!(
            failure_backoff_seconds(60, 4),
            MAX_FAILURE_BACKOFF_SECONDS,
            "capped at 15 minutes"
        );
        assert_eq!(
            failure_backoff_seconds(60, 32),
            MAX_FAILURE_BACKOFF_SECONDS,
            "shift is clamped before the multiplier can overflow"
        );
    }

    #[tokio::test]
    async fn failed_attempt_records_cooldown_and_success_resets() {
        let mock = MockAdapter::new();
        let provider = mock_provider(&mock);
        let state = Arc::new(Mutex::new(WorkerState {
            last_compaction: None,
            compaction_count: 0,
            consecutive_auto: 0,
            consecutive_failures: 0,
            is_compacting: false,
        }));

        // Two consecutive failures: each stamps `last_compaction` (so
        // the cooldown gate applies) and bumps the failure counter.
        for expected_failures in 1..=2u32 {
            mock.queue_error("provider down");
            let (request, rx) = test_request();
            process_compaction_request_with_config(
                request,
                provider.clone(),
                state.clone(),
                CompactionConfig::default(),
            )
            .await
            .unwrap();
            assert!(
                matches!(rx.await.unwrap(), CompactionResponse::Failed(_)),
                "mock error must surface as CompactionResponse::Failed"
            );
            let s = state.lock().await;
            assert!(
                s.last_compaction.is_some(),
                "a failed attempt must start the cooldown"
            );
            assert_eq!(s.consecutive_failures, expected_failures);
            assert_eq!(s.compaction_count, 0, "failures don't count as compactions");
            drop(s);
        }

        // A success resets the failure counter.
        mock.queue_text("Summary of conversation: user and assistant discussed several topics.");
        let (request, rx) = test_request();
        process_compaction_request_with_config(
            request,
            provider,
            state.clone(),
            CompactionConfig::default(),
        )
        .await
        .unwrap();
        assert!(matches!(
            rx.await.unwrap(),
            CompactionResponse::Completed(_)
        ));
        let s = state.lock().await;
        assert_eq!(
            s.consecutive_failures, 0,
            "success resets the failure counter"
        );
        assert_eq!(s.compaction_count, 1);
    }

    #[tokio::test]
    async fn should_request_blocks_during_failure_cooldown() {
        let mock = MockAdapter::new();
        let compactor =
            BackgroundCompactor::new(mock_provider(&mock), Arc::new(QuotaMeter::unlimited()));
        {
            let mut s = compactor.state.lock().await;
            s.last_compaction = Some(SystemTime::now());
            s.consecutive_failures = 1;
        }
        let config = CompactionConfig::default();
        // Over the dual threshold, but inside the (escalated) failure
        // cooldown — before the fix the cooldown never started on
        // failure, so this returned true and the loop resubmitted a
        // summarization call every iteration.
        assert!(
            !compactor
                .should_request(1_000_000, 1_000_000, &config)
                .await,
            "failure cooldown must block resubmission"
        );
    }

    #[test]
    fn test_compaction_quota_default() {
        let quota = CompactionQuota::default();
        assert_eq!(quota.cooldown_seconds, 60);
        assert_eq!(quota.max_compactions_per_session, 100);
        assert_eq!(quota.max_consecutive_auto, 5);
    }

    /// Fix #4: hydrating from the persisted per-session snapshot
    /// restores the quota counters, so `should_request` enforces
    /// session-level limits on a freshly constructed (per-run)
    /// compactor instead of starting cold.
    #[tokio::test]
    async fn hydrate_limits_state_restores_session_quota() {
        let mock = MockAdapter::new();
        let compactor =
            BackgroundCompactor::new(mock_provider(&mock), Arc::new(QuotaMeter::unlimited()));
        let config = CompactionConfig::default();

        // Fresh compactor, over threshold: the gate is open.
        assert!(
            compactor
                .should_request(1_000_000, 1_000_000, &config)
                .await
        );

        // Hydrate a session that already hit the consecutive-auto
        // ceiling (default `max_consecutive_auto` is 5).
        compactor
            .hydrate_limits_state(crate::compaction::CompactionLimitsState {
                compaction_count: 5,
                last_compaction_at_ms: None,
                consecutive_auto: 5,
                consecutive_failures: 0,
            })
            .await;
        assert!(
            !compactor
                .should_request(1_000_000, 1_000_000, &config)
                .await,
            "hydrated consecutive_auto must gate the per-session limit"
        );

        // A hydrated cooldown stamp blocks too.
        compactor
            .hydrate_limits_state(crate::compaction::CompactionLimitsState {
                compaction_count: 5,
                last_compaction_at_ms: Some(
                    u64::try_from(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis(),
                    )
                    .unwrap(),
                ),
                consecutive_auto: 0,
                consecutive_failures: 0,
            })
            .await;
        assert!(
            !compactor
                .should_request(1_000_000, 1_000_000, &config)
                .await,
            "hydrated cooldown stamp must gate the per-session cooldown"
        );
    }

    /// Fix #4 end-to-end at the worker level: compact on one
    /// compactor, persist the state to the session index, drop the
    /// compactor, build a fresh one (a new "run") and hydrate from a
    /// fresh `MetadataController` — the count / cooldown / consecutive
    /// counters carry over, and the next compaction's
    /// `compaction_number` continues the per-session sequence.
    #[tokio::test]
    async fn compaction_limits_survive_across_runs() {
        use crate::index::SessionEntry;
        use crate::metadata_controller::MetadataController;

        let temp = tempfile::TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();

        let mut controller = MetadataController::new(&dir);
        let entry = SessionEntry::new("sess_abc", "test_agent", "sess_abc.jsonl");
        let session_id = entry.session_id.to_string();
        controller.create_entry(entry).await.unwrap();

        // --- Run 1 ---
        let mock1 = MockAdapter::new();
        mock1.queue_text("Summary of the conversation so far.");
        let compactor1 =
            BackgroundCompactor::new(mock_provider(&mock1), Arc::new(QuotaMeter::unlimited()));
        compactor1
            .hydrate_limits_state(controller.get_compaction_state(&session_id).await.unwrap())
            .await;

        let rx = compactor1
            .request_compaction(
                test_messages(),
                None,
                crate::compaction::types::CompactionPhase::PreTurn,
            )
            .await
            .unwrap();
        let first = match rx.await.unwrap() {
            CompactionResponse::Completed(r) => r,
            other => panic!("expected Completed, got {other:?}"),
        };
        assert_eq!(first.entry.compaction_number, 1);

        // Sync back at the mutation point (what the driver does), then
        // drop the compactor — the run ends.
        controller
            .set_compaction_state(&session_id, compactor1.limits_state().await)
            .await
            .unwrap();
        drop(compactor1);

        // --- Run 2 (fresh controller + fresh compactor) ---
        let mut controller2 = MetadataController::new(&dir);
        let persisted = controller2.get_compaction_state(&session_id).await.unwrap();
        assert_eq!(persisted.compaction_count, 1);
        assert_eq!(persisted.consecutive_auto, 1);
        assert_eq!(persisted.consecutive_failures, 0);
        assert!(persisted.last_compaction_at_ms.is_some());

        let mock2 = MockAdapter::new();
        let compactor2 =
            BackgroundCompactor::new(mock_provider(&mock2), Arc::new(QuotaMeter::unlimited()));
        compactor2.hydrate_limits_state(persisted).await;

        // The 60s cooldown from run 1 still applies — the gate must
        // refuse even though this compactor is brand new.
        let config = CompactionConfig::default();
        assert!(
            !compactor2
                .should_request(1_000_000, 1_000_000, &config)
                .await,
            "cooldown must survive across runs"
        );

        // Once the cooldown has passed (backdate the stamp), the next
        // compaction continues the per-session sequence at 2.
        let mut backdated = persisted;
        backdated.last_compaction_at_ms = Some(0);
        compactor2.hydrate_limits_state(backdated).await;
        mock2.queue_text("Updated summary of the conversation.");
        let rx = compactor2
            .request_compaction(
                test_messages(),
                Some(first.entry.summary.clone()),
                crate::compaction::types::CompactionPhase::PreTurn,
            )
            .await
            .unwrap();
        let second = match rx.await.unwrap() {
            CompactionResponse::Completed(r) => r,
            other => panic!("expected Completed, got {other:?}"),
        };
        assert_eq!(
            second.entry.compaction_number, 2,
            "compaction_number must continue the per-session sequence"
        );
        assert_eq!(compactor2.limits_state().await.compaction_count, 2);
    }

    /// Fix #4: a failed attempt's cooldown stamp + consecutive-failure
    /// counter are part of the persisted snapshot, so the escalating
    /// backoff also survives across runs.
    #[tokio::test]
    async fn failure_state_survives_across_runs() {
        let mock1 = MockAdapter::new();
        mock1.queue_error("provider down");
        let compactor1 =
            BackgroundCompactor::new(mock_provider(&mock1), Arc::new(QuotaMeter::unlimited()));

        let rx = compactor1
            .request_compaction(
                test_messages(),
                None,
                crate::compaction::types::CompactionPhase::PreTurn,
            )
            .await
            .unwrap();
        assert!(matches!(rx.await.unwrap(), CompactionResponse::Failed(_)));
        let persisted = compactor1.limits_state().await;
        assert_eq!(persisted.consecutive_failures, 1);
        assert_eq!(persisted.compaction_count, 0);
        assert!(persisted.last_compaction_at_ms.is_some());
        drop(compactor1);

        // Run 2: a fresh compactor hydrated from the failure state is
        // inside the escalated failure cooldown (2 × base).
        let mock2 = MockAdapter::new();
        let compactor2 =
            BackgroundCompactor::new(mock_provider(&mock2), Arc::new(QuotaMeter::unlimited()));
        compactor2.hydrate_limits_state(persisted).await;
        let config = CompactionConfig::default();
        assert!(
            !compactor2
                .should_request(1_000_000, 1_000_000, &config)
                .await,
            "failure cooldown must survive across runs"
        );
        let state = compactor2.state.lock().await;
        assert_eq!(state.consecutive_failures, 1);
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
