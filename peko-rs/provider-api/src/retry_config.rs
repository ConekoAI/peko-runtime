//! Provider retry-policy configuration block.
//!
//! F40b / PR #3 Phase 2B. The struct lives in `peko_provider_api` so
//! the entire workspace (root config + daemon + `peko-providers`
//! transport + tests) can reference one canonical type. The
//! `peko_providers::factory` reads it directly instead of
//! re-declaring the same literals across three sites.
//!
//! Default values mirror codex's choice (`codex-client/src/retry.rs:42-47`)
//! calibrated against the empirical distribution of provider 429
//! retry windows:
//!
//! - `max_retries = 5` — 6 total transport attempts cover a typical
//!   429 burst without manual override.
//! - `retry_delay_ms = 1000` — 1-second initial backoff, doubles per
//!   attempt.
//! - `retry_max_delay_ms = 30_000` — cap a single exponential
//!   backoff at 30s; beyond that the upstream `Retry-After` header
//!   wins.
//! - `retry_jitter = Some(0.1)` — ±10% uniform spread to break
//!   thundering-herd alignment between peko agents hitting the same
//!   429 wall in lockstep.
//! - `max_attempts = 8` — total worst-case attempts across
//!   transport + engine mid-stream retry site, governed by one
//!   `SharedRetryBudget` instead of stacked ceilings.
//!
//! Validation is exposed separately (`ProviderRetryConfig::validate`)
//! so callers that want to surface caller-side misconfiguration as a
//! config error can do so explicitly. The struct itself never panics
//! — `Default` always succeeds.

use serde::{Deserialize, Serialize};

/// Per-provider retry behavior. Corresponds to the
/// `[provider.retry]` sub-table in `config.example.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderRetryConfig {
    /// Maximum number of transport-level retries for transient
    /// errors (timeouts, 5xx, 429-without-Retry-After). `0` disables
    /// transport retries entirely; the agentic-loop mid-stream
    /// retry site still fires for streaming 429s.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Initial backoff between transport retries, in milliseconds.
    /// The transport layer doubles this on each attempt
    /// (`backoff = retry_delay_ms * 2^attempt`), capped by
    /// `retry_max_delay_ms`.
    #[serde(default = "default_retry_delay_ms")]
    pub retry_delay_ms: u64,
    /// Cap on a single backoff wait after exponential growth, in
    /// milliseconds. Defaults to 30s — beyond that the upstream
    /// `Retry-After` header (parsed by F40a's
    /// `parse_retry_after_header`) wins.
    #[serde(default = "default_retry_max_delay_ms")]
    pub retry_max_delay_ms: u64,
    /// Uniform `[1-jitter, 1+jitter]` band applied to the computed
    /// backoff before sleep. `None` disables jitter (deterministic
    /// pre-F40 behavior); defaults to `Some(0.1)` to match codex's
    /// ±10% spread and break thundering-herd alignment.
    #[serde(default = "default_retry_jitter")]
    pub retry_jitter: Option<f64>,
    /// Total worst-case attempts across transport + engine mid-stream
    /// retry site, governed by a single `SharedRetryBudget`. Sized
    /// as `5 (transport) + 3 (engine) = 8` so a single ceiling
    /// replaces the pre-F40 stacked-budget anti-pattern.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
}

fn default_max_retries() -> u32 {
    5
}
fn default_retry_delay_ms() -> u64 {
    1000
}
fn default_retry_max_delay_ms() -> u64 {
    30_000
}
fn default_retry_jitter() -> Option<f64> {
    Some(0.1)
}
fn default_max_attempts() -> u32 {
    8
}

impl Default for ProviderRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            retry_delay_ms: default_retry_delay_ms(),
            retry_max_delay_ms: default_retry_max_delay_ms(),
            retry_jitter: default_retry_jitter(),
            max_attempts: default_max_attempts(),
        }
    }
}

impl ProviderRetryConfig {
    /// Validate the block. Surfaces caller-side misconfiguration as
    /// a structured error so the daemon can fail loudly at boot
    /// rather than silently running with bad values for the lifetime
    /// of the process.
    ///
    /// Validation rules:
    ///
    /// - `max_retries <= max_attempts` (otherwise transport spends
    ///   its full budget before engine gets a turn).
    /// - `retry_jitter` if present: `0.0 <= jitter < 1.0`.
    /// - `retry_delay_ms > 0` (zero delay would tight-loop).
    /// - `retry_max_delay_ms >= retry_delay_ms` (the cap cannot be
    ///   smaller than the seed).
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.max_retries > self.max_attempts {
            anyhow::bail!(
                "provider.retry: max_retries ({}) cannot exceed max_attempts ({})",
                self.max_retries,
                self.max_attempts
            );
        }
        if let Some(j) = self.retry_jitter {
            if !(0.0..1.0).contains(&j) {
                anyhow::bail!("provider.retry: retry_jitter ({j}) must be in [0.0, 1.0)");
            }
        }
        if self.retry_delay_ms == 0 {
            anyhow::bail!("provider.retry: retry_delay_ms must be > 0");
        }
        if self.retry_max_delay_ms < self.retry_delay_ms {
            anyhow::bail!(
                "provider.retry: retry_max_delay_ms ({}) must be >= retry_delay_ms ({})",
                self.retry_max_delay_ms,
                self.retry_delay_ms
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_factory_constants() {
        let cfg = ProviderRetryConfig::default();
        assert_eq!(cfg.max_retries, 5);
        assert_eq!(cfg.retry_delay_ms, 1000);
        assert_eq!(cfg.retry_max_delay_ms, 30_000);
        assert_eq!(cfg.retry_jitter, Some(0.1));
        assert_eq!(cfg.max_attempts, 8);
    }

    /// F40b: every retry field carries its own `#[serde(default)]`
    /// helper so a partial block parses with the rest at factory
    /// defaults.
    #[test]
    fn partial_block_parses_with_remaining_fields_defaulted() {
        let parsed: ProviderRetryConfig = toml::from_str("max_retries = 8\n").unwrap();
        assert_eq!(parsed.max_retries, 8);
        assert_eq!(parsed.retry_delay_ms, 1000);
        assert_eq!(parsed.retry_jitter, Some(0.1));
        assert_eq!(parsed.max_attempts, 8);
    }

    /// F40b: jitter validation rejects out-of-range values so a
    /// typo'd `retry_jitter = 2.0` doesn't quietly double every
    /// backoff.
    #[test]
    fn jitter_validation_rejects_out_of_range() {
        let mut cfg = ProviderRetryConfig::default();
        cfg.retry_jitter = Some(1.5);
        assert!(cfg.validate().is_err(), "jitter >= 1.0 must be rejected");
        cfg.retry_jitter = Some(-0.01);
        assert!(cfg.validate().is_err(), "negative jitter must be rejected");
        cfg.retry_jitter = Some(0.0);
        assert!(cfg.validate().is_ok(), "jitter=0.0 is valid");
        cfg.retry_jitter = Some(0.99);
        assert!(
            cfg.validate().is_ok(),
            "jitter in [0.0, 1.0) must be accepted"
        );
    }

    /// F40b: `max_retries > max_attempts` is rejected because the
    /// transport layer would burn through the shared budget before
    /// the engine mid-stream retry site ever sees a turn.
    #[test]
    fn max_retries_exceeds_max_attempts_rejected() {
        let mut cfg = ProviderRetryConfig::default();
        cfg.max_retries = 10;
        cfg.max_attempts = 4;
        let err = cfg
            .validate()
            .expect_err("max_retries > max_attempts must fail");
        assert!(
            err.to_string().contains("max_retries"),
            "error must name the failing field: {err}"
        );
    }

    /// F40b: zero `retry_delay_ms` is rejected (would tight-loop on
    /// every transient 5xx); cap smaller than seed is also rejected.
    #[test]
    fn zero_delay_and_cap_smaller_than_seed_rejected() {
        let mut cfg = ProviderRetryConfig::default();
        cfg.retry_delay_ms = 0;
        assert!(cfg.validate().is_err(), "zero delay must be rejected");
        cfg.retry_delay_ms = 1000;
        cfg.retry_max_delay_ms = 500;
        assert!(
            cfg.validate().is_err(),
            "cap smaller than seed must be rejected"
        );
    }

    /// F40b: round-trip through TOML preserves every field so a
    /// daemon that writes its config back on save doesn't drop
    /// new knobs.
    #[test]
    fn toml_roundtrip_preserves_every_field() {
        let original = ProviderRetryConfig {
            max_retries: 7,
            retry_delay_ms: 250,
            retry_max_delay_ms: 15_000,
            retry_jitter: Some(0.25),
            max_attempts: 12,
        };
        original
            .validate()
            .expect("non-default values must validate");
        let serialized = toml::to_string(&original).unwrap();
        let parsed: ProviderRetryConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed, original);
    }
}
