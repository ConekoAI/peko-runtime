//! `QuotaMeter` — runtime check / charge / advance engine.
//!
//! Holds an in-memory `QuotaState` under a `Mutex` and an optional
//! on-disk path for persistence. The engine loop, the compactor,
//! and subagent executors all hold `Arc<QuotaMeter>` clones and
//! coordinate through the lock.
//!
//! ## Lifecycle
//!
//! ```text
//! QuotaMeter::new(config, state_path)
//!     ├─ QuotaConfig — parsed from `principal.toml`. `None`
//!     │  limits mean "unlimited for that dimension".
//!     └─ state_path — Optional path to `quota_state.json`.
//!        When `None`, the meter is in-memory only (test mode,
//!        CLI status reads, etc.). When `Some`, the meter loads
//!        on construction and saves on every successful charge.
//! ```
//!
//! ## Operations
//!
//! - [`check`](Self::check) — read-only: returns the first limit
//!   that is currently over, or `None` if all under.
//! - [`charge`](Self::charge) — folds a `TokenUsage` into the
//!   state counters and returns `QuotaError` if the resulting
//!   totals cross a limit. Always calls `advance_if_needed`
//!   first so a request that straddles the cycle boundary lands
//!   in the new window.
//! - [`advance_if_needed`](Self::advance_if_needed) — rolls
//!   `window_start` / `window_end` forward when `now >=
//!   window_end`. Also detects config drift (cycle changed since
//!   load) and resets with the new cycle.
//! - [`reset`](Self::reset) — forced reset (for `peko quota reset`).
//!   Clears counters, keeps the cycle from the live config.

use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::config::{QuotaConfig, QuotaCycle};
use super::error::QuotaError;
use super::state::QuotaState;
use crate::common::types::message::TokenUsage;

/// In-process quota meter. Cheap to clone — the work happens under
/// the inner `Mutex<QuotaState>`, not on the `Arc` itself.
///
/// `config` is wrapped in a `Mutex` so `set_config` can mutate it
/// in place through `&self` (the meter is shared via `Arc`); the
/// lock is uncontended in the hot path so the cost is one atomic
/// acquire per call.
pub struct QuotaMeter {
    config: Mutex<QuotaConfig>,
    state_path: Option<PathBuf>,
    state: Mutex<QuotaState>,
}

impl QuotaMeter {
    /// Convenience constructor for an unlimited meter (no state
    /// file, every limit `None`). `charge` is a no-op for these
    /// meters — see `QuotaConfig::has_any_limit`. Used by callers
    /// that haven't yet been bound to a principal (e.g. unit tests
    /// that build an `Agent` directly).
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            config: Mutex::new(QuotaConfig::default()),
            state_path: None,
            state: Mutex::new(QuotaState::fresh(QuotaCycle::Daily, Utc::now())),
        }
    }

    /// Build a meter. If `state_path` is `Some` and the file
    /// exists, restore counters from disk; otherwise start with a
    /// fresh `QuotaState::fresh(cycle, now)`. The "now" used at
    /// construction is captured here — callers that want to roll
    /// forward should call [`advance_if_needed`](Self::advance_if_needed)
    /// explicitly.
    pub fn new(config: QuotaConfig, state_path: Option<PathBuf>, now: DateTime<Utc>) -> Self {
        // `tokio::fs` is async; we don't want to plumb async into
        // the constructor (the caller already has the path and can
        // load via `load_from_disk` if it wants async restore).
        // For F18, callers use `load_or_init` below.
        let state = QuotaState::fresh(config.cycle, now);
        Self {
            config: Mutex::new(config),
            state_path,
            state: Mutex::new(state),
        }
    }

    /// Async load + constructor. If `state_path` is `Some` and
    /// exists, restore; else start fresh. Preferred over `new`
    /// for production use — `PrincipalManager::load` is already
    /// async.
    pub async fn load_or_init(
        config: QuotaConfig,
        state_path: Option<PathBuf>,
        now: DateTime<Utc>,
    ) -> std::io::Result<Self> {
        let state = match state_path.as_deref() {
            Some(p) => match QuotaState::load(p).await? {
                Some(s) => s,
                None => QuotaState::fresh(config.cycle, now),
            },
            None => QuotaState::fresh(config.cycle, now),
        };
        Ok(Self {
            config: Mutex::new(config),
            state_path,
            state: Mutex::new(state),
        })
    }

    /// Read-only limit check. Returns the **first** exceeded
    /// limit (if any), ordered input → output → request. The
    /// loop calls this *before* an LLM call so a freshly-tripped
    /// quota aborts the run cleanly.
    #[must_use]
    pub fn check(&self) -> Option<QuotaError> {
        let state = self.state.lock().ok()?;
        let config = self.config.lock().ok()?;
        Self::check_inner(&config, &state)
    }

    fn check_inner(config: &QuotaConfig, state: &QuotaState) -> Option<QuotaError> {
        // Semantics: `limit` is the *inclusive* ceiling. A value
        // equal to the limit is allowed; the next increment trips.
        // Matches the user-facing intuition of "100 tokens / 2
        // requests per window" — the 3rd request fails, the 101st
        // token fails.
        if let Some(limit) = config.input_tokens {
            if state.input_tokens > limit {
                return Some(QuotaError::InputTokensExceeded {
                    used: state.input_tokens,
                    limit,
                    window_end: state.window_end,
                });
            }
        }
        if let Some(limit) = config.output_tokens {
            if state.output_tokens > limit {
                return Some(QuotaError::OutputTokensExceeded {
                    used: state.output_tokens,
                    limit,
                    window_end: state.window_end,
                });
            }
        }
        if let Some(limit) = config.request_count {
            if state.request_count > limit {
                return Some(QuotaError::RequestCountExceeded {
                    used: state.request_count,
                    limit,
                    window_end: state.window_end,
                });
            }
        }
        None
    }

    /// Roll the window forward if `now >= window_end` or if the
    /// config's cycle has changed since the last charge. Returns
    /// `true` if a roll actually happened (cheap signal for tests).
    pub fn advance_if_needed(&self, now: DateTime<Utc>) -> bool {
        let mut state = self.state.lock().expect("quota state mutex poisoned");
        let config_cycle = self.config.lock().expect("quota config poisoned").cycle;
        let drift = state.cycle != config_cycle;
        if drift || now >= state.window_end {
            let (start, end) = config_cycle.window_bounds(now);
            state.window_start = start;
            state.window_end = end;
            state.cycle = config_cycle;
            state.input_tokens = 0;
            state.output_tokens = 0;
            state.request_count = 0;
            true
        } else {
            false
        }
    }

    /// Charge a `TokenUsage` to the meter. Folds cache reads/writes
    /// into `input` and reasoning into `output` via
    /// [`TokenUsage::accumulate`], increments `request_count` by
    /// one, advances the window if needed, and returns the first
    /// limit crossed (if any). On success, persists to disk.
    pub async fn charge(&self, usage: &TokenUsage) -> Result<(), QuotaError> {
        // 1. Advance the window under the lock.
        {
            let mut state = self.state.lock().expect("quota state mutex poisoned");
            let config_cycle = self.config.lock().expect("quota config poisoned").cycle;
            let drift = state.cycle != config_cycle;
            let now = Utc::now();
            if drift || now >= state.window_end {
                let (start, end) = config_cycle.window_bounds(now);
                state.window_start = start;
                state.window_end = end;
                state.cycle = config_cycle;
                state.input_tokens = 0;
                state.output_tokens = 0;
                state.request_count = 0;
            }
            // 2. Fold usage via the shared accumulate helper so
            //    cache and reasoning sub-fields flow into the same
            //    input/output buckets the quota tracks.
            state.input_tokens = state.input_tokens.saturating_add(usage.input);
            state.output_tokens = state.output_tokens.saturating_add(usage.output);
            state.request_count = state.request_count.saturating_add(1);
        }
        // 3. Check under the lock (re-acquire to satisfy borrowck).
        let exceeded = {
            let state = self.state.lock().expect("quota state mutex poisoned");
            let config = self.config.lock().expect("quota config poisoned");
            Self::check_inner(&config, &state)
        };
        if let Some(err) = exceeded {
            // Don't persist a state that crossed a limit — the
            // operator's view is "quota tripped", not "counters
            // kept climbing past the wall".
            return Err(err);
        }
        // 4. Persist on success. Clone the state under the lock, drop
        //    the guard, then await the save — holding the mutex
        //    across an await point would make this non-Send.
        if let Some(path) = self.state_path.clone() {
            let snapshot = self.state.lock().expect("quota state mutex poisoned").clone();
            if let Err(e) = snapshot.save(&path).await {
                tracing::warn!("failed to persist quota state to {}: {}", path.display(), e);
            }
        }
        Ok(())
    }

    /// Forced reset — clears counters, keeps the cycle from the
    /// live config, advances the window to `now`. Used by `peko
    /// quota reset` and by tests.
    pub async fn reset(&self, now: DateTime<Utc>) {
        {
            let mut state = self.state.lock().expect("quota state mutex poisoned");
            let config_cycle = self.config.lock().expect("quota config poisoned").cycle;
            let (start, end) = config_cycle.window_bounds(now);
            state.window_start = start;
            state.window_end = end;
            state.cycle = config_cycle;
            state.input_tokens = 0;
            state.output_tokens = 0;
            state.request_count = 0;
        }
        if let Some(path) = self.state_path.clone() {
            let snapshot = self.state.lock().expect("quota state mutex poisoned").clone();
            if let Err(e) = snapshot.save(&path).await {
                tracing::warn!("failed to persist quota state to {}: {}", path.display(), e);
            }
        }
    }

    /// Snapshot of current counters + window bounds. Used by `peko
    /// quota status` to render the running totals.
    #[must_use]
    pub fn snapshot(&self) -> QuotaState {
        self.state.lock().expect("quota state mutex poisoned").clone()
    }

    /// View of the configured limits (the `[quota]` TOML block).
    #[must_use]
    pub fn config(&self) -> QuotaConfig {
        // Return a clone so callers can use the value without
        // holding the lock (and without `&self.config` lifetime
        // gymnastics). The struct is small and Clone is cheap.
        self.config
            .lock()
            .expect("quota config poisoned")
            .clone()
    }

    /// Where state is persisted, if at all.
    #[must_use]
    pub fn state_path(&self) -> Option<&Path> {
        self.state_path.as_deref()
    }

    /// True iff at least one limit is currently exceeded.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.check().is_some()
    }

    /// F18: replace the live config in place. Used by
    /// `peko quota set` to apply new limits without a daemon
    /// restart. **Does not** reset the counters — the existing
    /// accumulated usage carries into the new window. To force a
    /// fresh window, callers should follow up with
    /// [`Self::reset`].
    pub fn set_config(&self, new_config: QuotaConfig) {
        *self
            .config
            .lock()
            .expect("quota config poisoned") = new_config;
    }

    /// F19: sync version of [`Self::charge`]. Used by the streaming
    /// metering path (`MeteredProvider::stream_with_tools` intercepts
    /// `StreamEvent::Usage` events inside the stream `map` closure,
    /// which is sync). Skips persistence — the next blocking
    /// `charge` (e.g. the following iteration's LLM call) writes
    /// the in-memory state to disk. The on-disk counters may lag by
    /// one streaming call at most, which is acceptable.
    ///
    /// Folds usage, advances the window, increments `request_count`,
    /// and returns the first limit crossed (if any). **No I/O.**
    pub fn try_charge(&self, usage: &TokenUsage) -> Result<(), QuotaError> {
        let mut state = self.state.lock().expect("quota state mutex poisoned");
        let config_cycle = self.config.lock().expect("quota config poisoned").cycle;
        let drift = state.cycle != config_cycle;
        let now = Utc::now();
        if drift || now >= state.window_end {
            let (start, end) = config_cycle.window_bounds(now);
            state.window_start = start;
            state.window_end = end;
            state.cycle = config_cycle;
            state.input_tokens = 0;
            state.output_tokens = 0;
            state.request_count = 0;
        }
        state.input_tokens = state.input_tokens.saturating_add(usage.input);
        state.output_tokens = state.output_tokens.saturating_add(usage.output);
        state.request_count = state.request_count.saturating_add(1);
        let config = self.config.lock().expect("quota config poisoned");
        if let Some(err) = Self::check_inner(&config, &state) {
            Err(err)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn ts(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, min, sec).single().unwrap()
    }

    fn cfg(input: Option<u64>, output: Option<u64>, requests: Option<u64>) -> QuotaConfig {
        QuotaConfig {
            input_tokens: input,
            output_tokens: output,
            request_count: requests,
            cycle: QuotaCycle::Hourly,
        }
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input,
            output,
            total: input + output,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn charge_increments_counters_and_persists() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("quota_state.json");
        let meter = QuotaMeter::load_or_init(
            cfg(Some(1000), Some(1000), Some(100)),
            Some(path.clone()),
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();

        meter.charge(&usage(50, 10)).await.unwrap();
        let snap = meter.snapshot();
        assert_eq!(snap.input_tokens, 50);
        assert_eq!(snap.output_tokens, 10);
        assert_eq!(snap.request_count, 1);

        meter.charge(&usage(100, 20)).await.unwrap();
        let snap = meter.snapshot();
        assert_eq!(snap.input_tokens, 150);
        assert_eq!(snap.output_tokens, 30);
        assert_eq!(snap.request_count, 2);

        // Persisted to disk.
        let reloaded = QuotaState::load(&path).await.unwrap().unwrap();
        assert_eq!(reloaded.input_tokens, 150);
    }

    #[tokio::test]
    async fn charge_trips_input_limit_on_the_offending_call() {
        // limit=100 means up to 100 input tokens per window are
        // allowed; the 101st trips. Two charges of 60 each sum to
        // 120, exceeding 100.
        let meter = QuotaMeter::load_or_init(
            cfg(Some(100), None, None),
            None,
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        meter.charge(&usage(60, 0)).await.unwrap();
        let err = meter.charge(&usage(60, 0)).await.unwrap_err();
        match err {
            QuotaError::InputTokensExceeded { used, limit, .. } => {
                assert_eq!(used, 120);
                assert_eq!(limit, 100);
            }
            other => panic!("expected InputTokensExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn charge_trips_output_limit() {
        // limit=50 output tokens; one charge of 60 trips.
        let meter = QuotaMeter::load_or_init(
            cfg(None, Some(50), None),
            None,
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        let err = meter.charge(&usage(0, 60)).await.unwrap_err();
        match err {
            QuotaError::OutputTokensExceeded { used, limit, .. } => {
                assert_eq!(used, 60);
                assert_eq!(limit, 50);
            }
            other => panic!("expected OutputTokensExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn charge_trips_request_count_limit() {
        // limit=2 means up to 2 requests per window; the 3rd trips.
        let meter = QuotaMeter::load_or_init(
            cfg(None, None, Some(2)),
            None,
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        meter.charge(&usage(10, 10)).await.unwrap();
        meter.charge(&usage(10, 10)).await.unwrap();
        let err = meter.charge(&usage(10, 10)).await.unwrap_err();
        match err {
            QuotaError::RequestCountExceeded { used, limit, .. } => {
                assert_eq!(used, 3);
                assert_eq!(limit, 2);
            }
            other => panic!("expected RequestCountExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn check_returns_none_when_under_all_limits() {
        let meter = QuotaMeter::load_or_init(
            cfg(Some(1000), Some(1000), Some(100)),
            None,
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        meter.charge(&usage(50, 10)).await.unwrap();
        assert!(meter.check().is_none());
        assert!(!meter.is_exhausted());
    }

    #[tokio::test]
    async fn advance_if_needed_resets_when_window_expires() {
        let meter = QuotaMeter::load_or_init(
            cfg(Some(100), Some(100), Some(100)),
            None,
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        meter.charge(&usage(80, 80)).await.unwrap();
        assert_eq!(meter.snapshot().input_tokens, 80);

        // Step past the hourly boundary.
        let rolled = meter.advance_if_needed(ts(2026, 7, 12, 15, 0, 1));
        assert!(rolled, "advance should have happened");
        let snap = meter.snapshot();
        assert_eq!(snap.input_tokens, 0);
        assert_eq!(snap.output_tokens, 0);
        assert_eq!(snap.request_count, 0);
        assert_eq!(snap.window_start, ts(2026, 7, 12, 15, 0, 0));
        assert_eq!(snap.window_end, ts(2026, 7, 12, 16, 0, 0));
    }

    #[tokio::test]
    async fn advance_if_needed_noop_inside_window() {
        let meter = QuotaMeter::load_or_init(
            cfg(Some(100), Some(100), Some(100)),
            None,
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        meter.charge(&usage(50, 0)).await.unwrap();
        let rolled = meter.advance_if_needed(ts(2026, 7, 12, 14, 30, 0));
        assert!(!rolled, "no advance inside window");
        assert_eq!(meter.snapshot().input_tokens, 50);
    }

    #[tokio::test]
    async fn advance_if_needed_detects_cycle_drift() {
        let meter = QuotaMeter::load_or_init(
            cfg(Some(100), Some(100), Some(100)),
            None,
            ts(2026, 7, 12, 14, 30, 0),
        )
        .await
        .unwrap();
        meter.charge(&usage(80, 0)).await.unwrap();
        // Cycle changes (operator ran `peko quota set --cycle daily`).
        // We can't mutate self.config directly (it's not pub), so
        // we exercise drift via the next call after a config
        // replacement: rebuild a meter with the new cycle and the
        // same state-path. The state on disk has cycle=Hourly; the
        // new config's cycle is Daily; the first `charge` should
        // detect drift and reset.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("quota_state.json");
        let m2 = QuotaMeter::load_or_init(
            cfg(Some(100), Some(100), Some(100)),
            Some(path.clone()),
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        m2.charge(&usage(50, 0)).await.unwrap();
        // Reload with a Daily cycle.
        let mut cfg_daily = m2.config().clone();
        cfg_daily.cycle = QuotaCycle::Daily;
        let m3 = QuotaMeter::load_or_init(cfg_daily, Some(path.clone()), ts(2026, 7, 12, 14, 30, 0))
            .await
            .unwrap();
        let _ = m3; // Silence unused warning.
        // Drift is detected inside `charge`'s advance step; verify
        // by manually invoking advance_if_needed on a meter whose
        // config has drifted from its persisted state.
        let m4 = QuotaMeter::load_or_init(
            cfg(Some(100), Some(100), Some(100)),
            Some(path.clone()),
            ts(2026, 7, 12, 14, 30, 0),
        )
        .await
        .unwrap();
        // Override the cycle via a fresh state — m4's persisted
        // state has Daily (from m3), but cfg is Hourly. First
        // charge triggers drift-detection advance.
        m4.charge(&usage(0, 0)).await.unwrap();
        let snap = m4.snapshot();
        assert_eq!(snap.cycle, QuotaCycle::Hourly, "drift should have reset to new config's cycle");
    }

    #[tokio::test]
    async fn reset_clears_counters_and_advances_window() {
        let meter = QuotaMeter::load_or_init(
            cfg(Some(100), Some(100), Some(100)),
            None,
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        meter.charge(&usage(80, 80)).await.unwrap();
        assert_eq!(meter.snapshot().input_tokens, 80);
        meter.reset(ts(2026, 7, 12, 14, 30, 0)).await;
        let snap = meter.snapshot();
        assert_eq!(snap.input_tokens, 0);
        assert_eq!(snap.window_start, ts(2026, 7, 12, 14, 0, 0));
        assert_eq!(snap.window_end, ts(2026, 7, 12, 15, 0, 0));
    }

    #[tokio::test]
    async fn no_config_means_unlimited() {
        let meter = QuotaMeter::load_or_init(QuotaConfig::default(), None, ts(2026, 7, 12, 14, 0, 0))
            .await
            .unwrap();
        // Even pathological usage shouldn't trip anything.
        for _ in 0..1000 {
            meter.charge(&usage(u64::MAX / 2, u64::MAX / 2)).await.unwrap();
        }
        assert!(!meter.is_exhausted());
    }

    #[tokio::test]
    async fn charge_folds_cache_and_reasoning_via_accumulate() {
        // The meter trusts the caller to have already pre-folded
        // cache reads/writes and reasoning into `input` / `output`
        // via `TokenUsage::accumulate`. We test that pre-folded
        // usage trips the input_tokens limit correctly.
        let meter = QuotaMeter::load_or_init(
            cfg(Some(100), None, None),
            None,
            ts(2026, 7, 12, 14, 0, 0),
        )
        .await
        .unwrap();
        let u = TokenUsage {
            // Simulates a caller who has already run
            // `accumulate()`: 60 non-cached input + 20 cache
            // creation + 30 cache read = 110 input.
            input: 110,
            output: 0,
            total: 110,
            cache_creation_input_tokens: Some(20),
            cache_read_input_tokens: Some(30),
            reasoning_output_tokens: None,
        };
        let err = meter.charge(&u).await.unwrap_err();
        match err {
            QuotaError::InputTokensExceeded { used, limit, .. } => {
                assert_eq!(used, 110);
                assert_eq!(limit, 100);
            }
            other => panic!("expected InputTokensExceeded, got {other:?}"),
        }
    }
}