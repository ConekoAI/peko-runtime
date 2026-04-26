//! Model Context Registry
//!
//! Registry of known LLM model context window sizes per provider.
//! Used by the compactor to determine auto-compaction thresholds based on
//! the actual provider/model being used, rather than hard-coded defaults.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Registry of known model context windows (tokens)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelContextRegistry {
    /// Fallback when model is unknown
    #[serde(default = "default_limit")]
    pub default_limit: usize,
    /// Provider → Model → Limit
    #[serde(default)]
    pub limits: HashMap<String, HashMap<String, usize>>,
}

fn default_limit() -> usize {
    128_000
}

impl ModelContextRegistry {
    /// Create a new registry with built-in defaults for known models
    #[must_use]
    pub fn new() -> Self {
        let mut limits: HashMap<String, HashMap<String, usize>> = HashMap::new();

        // minimax
        limits
            .entry("minimax".to_string())
            .or_insert_with(HashMap::new)
            .insert("M2.7".to_string(), 204_800);

        // kimi
        limits
            .entry("kimi".to_string())
            .or_insert_with(HashMap::new)
            .insert("K2.6".to_string(), 262_144);

        // openai
        let openai_models = [
            ("gpt-4o".to_string(), 128_000),
            ("gpt-4o-mini".to_string(), 128_000),
            ("gpt-4-turbo".to_string(), 128_000),
            ("gpt-4".to_string(), 8_192),
            ("gpt-3.5-turbo".to_string(), 16_384),
            ("o1".to_string(), 200_000),
            ("o3-mini".to_string(), 200_000),
        ];
        let openai_map = limits
            .entry("openai".to_string())
            .or_insert_with(HashMap::new);
        for (model, limit) in openai_models {
            openai_map.insert(model, limit);
        }

        // anthropic
        let anthropic_models = [
            ("claude-3-5-sonnet".to_string(), 200_000),
            ("claude-3-5-haiku".to_string(), 200_000),
            ("claude-3-opus".to_string(), 200_000),
            ("claude-3-sonnet".to_string(), 200_000),
            ("claude-3-haiku".to_string(), 200_000),
        ];
        let anthropic_map = limits
            .entry("anthropic".to_string())
            .or_insert_with(HashMap::new);
        for (model, limit) in anthropic_models {
            anthropic_map.insert(model, limit);
        }

        // google / gemini
        let gemini_models = [
            ("gemini-1.5-pro".to_string(), 2_097_152),
            ("gemini-1.5-flash".to_string(), 1_048_576),
            ("gemini-1.0-pro".to_string(), 32_768),
        ];
        let gemini_map = limits
            .entry("google".to_string())
            .or_insert_with(HashMap::new);
        for (model, limit) in gemini_models {
            gemini_map.insert(model, limit);
        }

        // ollama — context window varies by model, use common defaults
        let ollama_models = [
            ("llama2".to_string(), 4_096),
            ("llama3".to_string(), 8_192),
            ("llama3.1".to_string(), 128_000),
            ("llama3.2".to_string(), 128_000),
            ("mistral".to_string(), 32_768),
            ("mixtral".to_string(), 32_768),
            ("codellama".to_string(), 16_384),
            ("qwen2.5".to_string(), 128_000),
            ("phi4".to_string(), 16_384),
        ];
        let ollama_map = limits
            .entry("ollama".to_string())
            .or_insert_with(HashMap::new);
        for (model, limit) in ollama_models {
            ollama_map.insert(model, limit);
        }

        Self {
            default_limit: 128_000,
            limits,
        }
    }

    /// Look up the context window limit for a given provider and model.
    /// Falls back to `default_limit` if the provider or model is not known.
    #[must_use]
    pub fn get(&self, provider: &str, model: &str) -> usize {
        self.limits
            .get(provider)
            .and_then(|m| m.get(model))
            .copied()
            .unwrap_or(self.default_limit)
    }

    /// Register or override a limit for a provider/model pair.
    pub fn set(&mut self, provider: impl Into<String>, model: impl Into<String>, limit: usize) {
        self.limits
            .entry(provider.into())
            .or_insert_with(HashMap::new)
            .insert(model.into(), limit);
    }

    /// Merge another registry into this one. Existing entries are overwritten.
    pub fn merge(&mut self, other: &ModelContextRegistry) {
        for (provider, models) in &other.limits {
            let entry = self.limits.entry(provider.clone()).or_default();
            for (model, limit) in models {
                entry.insert(model.clone(), *limit);
            }
        }
        // Also update default if the other has a different explicit value
        // (we can't easily detect "explicit" vs default, so we leave ours)
    }
}

impl Default for ModelContextRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns true if compaction should trigger based on dual-threshold logic.
///
/// Triggers when **either** condition is met:
/// - Ratio-based: `estimated_tokens >= (context_window * auto_threshold_percent / 100)`
/// - Reserved-based: `estimated_tokens >= (context_window - reserve_tokens)`
#[must_use]
pub fn should_auto_compact(
    estimated_tokens: usize,
    context_window: usize,
    config: &crate::compaction::CompactionConfig,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::CompactionConfig;

    #[test]
    fn test_registry_defaults() {
        let reg = ModelContextRegistry::new();
        assert_eq!(reg.get("openai", "gpt-4o"), 128_000);
        assert_eq!(reg.get("kimi", "K2.6"), 262_144);
        assert_eq!(reg.get("minimax", "M2.7"), 204_800);
        assert_eq!(reg.get("unknown", "unknown"), 128_000); // fallback
    }

    #[test]
    fn test_registry_set_and_get() {
        let mut reg = ModelContextRegistry::new();
        reg.set("custom", "my-model", 50_000);
        assert_eq!(reg.get("custom", "my-model"), 50_000);
    }

    #[test]
    fn test_should_auto_compact_ratio() {
        let config = CompactionConfig {
            enabled: true,
            auto_threshold_percent: 85,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            ..CompactionConfig::default()
        };
        // Large model: 1M context, 860K tokens → 86% → ratio threshold fires
        assert!(should_auto_compact(860_000, 1_000_000, &config));
        // Well under ratio
        assert!(!should_auto_compact(500_000, 1_000_000, &config));
    }

    #[test]
    fn test_should_auto_compact_reserved() {
        let config = CompactionConfig {
            enabled: true,
            auto_threshold_percent: 85,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            ..CompactionConfig::default()
        };
        // Standard model: 128K context, 115K tokens → below 85% ratio (108.8K)
        // but above reserved threshold (128K - 16K = 112K)
        assert!(should_auto_compact(115_000, 128_000, &config));
        // Well under both
        assert!(!should_auto_compact(100_000, 128_000, &config));
    }

    #[test]
    fn test_should_auto_compact_disabled() {
        let config = CompactionConfig {
            enabled: false,
            ..CompactionConfig::default()
        };
        assert!(!should_auto_compact(1_000_000, 128_000, &config));
    }

    #[test]
    fn test_registry_merge() {
        let mut reg = ModelContextRegistry::new();
        let mut other = ModelContextRegistry::new();
        other.set("openai", "gpt-4o", 200_000); // override
        other.set("new", "model", 10_000);

        reg.merge(&other);
        assert_eq!(reg.get("openai", "gpt-4o"), 200_000);
        assert_eq!(reg.get("new", "model"), 10_000);
        assert_eq!(reg.get("kimi", "K2.6"), 262_144); // preserved
    }
}
