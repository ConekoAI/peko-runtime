//! OpenAI-compatible adapter
//!
//! For providers that use the `OpenAI` API format but with different base URLs.
//! This is a thin wrapper around `OpenAiAdapter` that allows custom base URLs.

use crate::providers::transport::AuthConfig;
use crate::providers::types::{ChatOptions, ChatResponse, LlmMessage, StreamEvent, ToolDefinition};
use anyhow::Result;
use serde_json::Value;

use super::openai::OpenAiAdapter;

/// OpenAI-compatible adapter
///
/// Uses the `OpenAI` API format but with a custom base URL.
/// Used by Groq, Together, Fireworks, Moonshot, and other OpenAI-compatible providers.
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleAdapter {
    inner: OpenAiAdapter,
    base_url: String,
    name: String,
}

impl OpenAiCompatibleAdapter {
    /// Create a new OpenAI-compatible adapter
    #[must_use]
    pub fn new(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let name = name.into();
        let inner = OpenAiAdapter::new().with_base_url(base_url.clone());

        Self {
            inner,
            base_url,
            name,
        }
    }

    /// Create pre-configured adapters for common providers
    #[must_use]
    pub fn groq() -> Self {
        Self::new("groq", "https://api.groq.com/openai/v1")
    }

    #[must_use]
    pub fn together() -> Self {
        Self::new("together", "https://api.together.xyz/v1")
    }

    #[must_use]
    pub fn fireworks() -> Self {
        Self::new("fireworks", "https://api.fireworks.ai/inference/v1")
    }

    #[must_use]
    pub fn moonshot() -> Self {
        Self::new("moonshot", "https://api.moonshot.cn/v1")
    }

    #[must_use]
    pub fn deepseek() -> Self {
        Self::new("deepseek", "https://api.deepseek.com/v1")
    }

    #[must_use]
    pub fn perplexity() -> Self {
        Self::new("perplexity", "https://api.perplexity.ai")
    }

    #[must_use]
    pub fn openrouter() -> Self {
        Self::new("openrouter", "https://openrouter.ai/api/v1")
    }

    #[must_use]
    pub fn xai() -> Self {
        Self::new("xai", "https://api.x.ai/v1")
    }

    #[must_use]
    pub fn ollama() -> Self {
        Self::new("ollama", "http://localhost:11434/v1")
    }
}

impl super::ApiAdapter for OpenAiCompatibleAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn build_request(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: Option<&[ToolDefinition]>,
        options: &ChatOptions,
        stream: bool,
    ) -> Result<(String, Value)> {
        self.inner
            .build_request(model_id, messages, tools, options, stream)
    }

    fn parse_response(&self, model_id: &str, response: Value) -> Result<ChatResponse> {
        let mut parsed = self.inner.parse_response(model_id, response)?;
        parsed.provider = self.name.clone();
        Ok(parsed)
    }

    fn parse_sse_event(
        &self,
        model_id: &str,
        data: &str,
    ) -> Result<Option<StreamEvent>> {
        self.inner.parse_sse_event(model_id, data)
    }

    fn auth_config(&self, api_key: &str) -> AuthConfig {
        AuthConfig::Bearer {
            token: api_key.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::adapters::ApiAdapter;

    #[test]
    fn test_groq_adapter() {
        let adapter = OpenAiCompatibleAdapter::groq();
        assert_eq!(adapter.name(), "groq");
        assert_eq!(adapter.base_url(), "https://api.groq.com/openai/v1");
    }

    #[test]
    fn test_together_adapter() {
        let adapter = OpenAiCompatibleAdapter::together();
        assert_eq!(adapter.name(), "together");
        assert_eq!(adapter.base_url(), "https://api.together.xyz/v1");
    }

    #[test]
    fn test_moonshot_adapter() {
        let adapter = OpenAiCompatibleAdapter::moonshot();
        assert_eq!(adapter.name(), "moonshot");
        assert_eq!(adapter.base_url(), "https://api.moonshot.cn/v1");
    }

    #[test]
    fn test_ollama_adapter() {
        let adapter = OpenAiCompatibleAdapter::ollama();
        assert_eq!(adapter.name(), "ollama");
        assert_eq!(adapter.base_url(), "http://localhost:11434/v1");
    }
}
