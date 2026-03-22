//! Kimi (Moonshot) provider - Thin wrapper around OpenAICompatibleProvider
//!
//! DEPRECATED: Use `OpenAICompatibleProvider::moonshot()` instead.
//!
//! Kimi uses OpenAI-compatible API, so this is just a thin wrapper
//! with Moonshot-specific defaults.

use crate::providers::OpenAICompatibleProvider;

/// Re-export the OpenAI-compatible provider as Kimi
/// 
/// DEPRECATED: Use `OpenAICompatibleProvider` directly with `OpenAICompatibleConfig::moonshot()`.
pub type KimiProvider = OpenAICompatibleProvider;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::Provider;

    #[test]
    fn test_kimi_provider_alias() {
        // KimiProvider is just an alias for OpenAICompatibleProvider
        let provider = OpenAICompatibleProvider::moonshot("test_key", "kimi-k2.5");
        assert!(provider.is_ok());
        
        let provider = provider.unwrap();
        assert_eq!(provider.name(), "kimi");
    }
}
