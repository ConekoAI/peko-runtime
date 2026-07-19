//! Prompt-cache retention policy
//!
//! Single source of truth for the provider-agnostic knob that gates
//! cache-marker attachment. Maps to provider-specific wire fields:
//!
//! | `CacheRetention` | Anthropic `cache_control`     | OpenAI `prompt_cache_retention` |
//! |------------------|--------------------------------|---------------------------------|
//! | `Default`        | `{ type: "ephemeral" }`        | field omitted (provider default) |
//! | `Long`           | `{ type: "ephemeral", ttl: "1h" }` | `"24h"`                     |
//! | `None`           | field omitted on every block    | field omitted on every block   |
//!
//! `Default` keeps today's behavior (cache markers are attached
//! whenever the caller supplies `prompt_cache_key` or any
//! `cache_retention != None`); the provider picks its own TTL.
//! `None` is the opt-out: callers who want to skip cache markers
//! entirely (e.g. for benchmarking) set this value.

use serde::{Deserialize, Serialize};

/// Prompt-cache retention policy.
///
/// `Default` lets the provider pick its own TTL (Anthropic's 5-minute
/// ephemeral window, OpenAI's standard caching).
/// `Long` requests the longest TTL the provider offers (Anthropic 1h,
/// OpenAI 24h). `None` disables cache markers and session-affinity
/// fields — useful for benchmarking or for routing decisions that
/// should not benefit from cross-request caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    #[default]
    Default,
    Long,
    None,
}

impl CacheRetention {
    /// True iff cache markers / cache-key fields should be emitted on
    /// the wire. `Default` and `Long` both opt in; `None` opts out.
    #[must_use]
    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_retention_default_is_default_variant() {
        assert_eq!(CacheRetention::default(), CacheRetention::Default);
    }

    #[test]
    fn test_cache_retention_default_serializes_to_default() {
        let json = serde_json::to_string(&CacheRetention::Default).unwrap();
        assert_eq!(json, "\"default\"");
    }

    #[test]
    fn test_cache_retention_long_serializes_to_long() {
        let json = serde_json::to_string(&CacheRetention::Long).unwrap();
        assert_eq!(json, "\"long\"");
    }

    #[test]
    fn test_cache_retention_none_serializes_to_none() {
        let json = serde_json::to_string(&CacheRetention::None).unwrap();
        assert_eq!(json, "\"none\"");
    }

    #[test]
    fn test_cache_retention_roundtrip() {
        for variant in [
            CacheRetention::Default,
            CacheRetention::Long,
            CacheRetention::None,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: CacheRetention = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn test_cache_retention_is_enabled() {
        assert!(CacheRetention::Default.is_enabled());
        assert!(CacheRetention::Long.is_enabled());
        assert!(!CacheRetention::None.is_enabled());
    }
}
