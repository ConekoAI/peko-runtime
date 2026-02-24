//! LLM Provider configuration types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider type
    pub provider_type: ProviderType,
    /// API key (optional - can use env var)
    pub api_key: Option<String>,
    /// API key environment variable name
    pub api_key_env: Option<String>,
    /// Base URL (for custom/OpenAI-compatible endpoints)
    pub base_url: Option<String>,
    /// Default model
    pub default_model: String,
    /// Model configurations
    pub models: HashMap<String, ModelConfig>,
    /// Request timeout (seconds)
    pub timeout_seconds: u64,
    /// Maximum retries
    pub max_retries: u32,
    /// Retry delay (milliseconds)
    pub retry_delay_ms: u64,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        let mut models = HashMap::new();
        models.insert(
            "default".to_string(),
            ModelConfig {
                name: "gpt-4o-mini".to_string(),
                max_tokens: 4096,
                temperature: 0.7,
                top_p: 1.0,
                presence_penalty: 0.0,
                frequency_penalty: 0.0,
            },
        );

        Self {
            provider_type: ProviderType::OpenAI,
            api_key: None,
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            base_url: None,
            default_model: "default".to_string(),
            models,
            timeout_seconds: 60,
            max_retries: 3,
            retry_delay_ms: 1000,
        }
    }
}

/// LLM Provider type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    /// `OpenAI` (GPT-4, GPT-3.5)
    OpenAI,
    /// Anthropic (Claude)
    Anthropic,
    /// Ollama (local models)
    Ollama,
    /// OpenAI-compatible API (custom endpoint)
    OpenAICompatible,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderType::OpenAI => write!(f, "openai"),
            ProviderType::Anthropic => write!(f, "anthropic"),
            ProviderType::Ollama => write!(f, "ollama"),
            ProviderType::OpenAICompatible => write!(f, "openai_compatible"),
        }
    }
}

/// Model configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Model name/ID
    pub name: String,
    /// Maximum tokens to generate
    pub max_tokens: u32,
    /// Temperature (0.0 - 2.0)
    pub temperature: f32,
    /// Top-p sampling
    pub top_p: f32,
    /// Presence penalty
    pub presence_penalty: f32,
    /// Frequency penalty
    pub frequency_penalty: f32,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            name: "gpt-4o-mini".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
        }
    }
}

/// Chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Message role: system, user, assistant, tool
    pub role: String,
    /// Message content
    pub content: String,
    /// Tool calls (for assistant messages)
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Tool call ID (for tool messages)
    pub tool_call_id: Option<String>,
    /// Name (for tool messages)
    pub name: Option<String>,
}

impl ChatMessage {
    /// Create a system message
    #[must_use]
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create a user message
    #[must_use]
    pub fn user(content: &str) -> Self {
        Self {
            role: "user".to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create an assistant message
    #[must_use]
    pub fn assistant(content: &str) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create a tool message
    #[must_use]
    pub fn tool(content: &str, tool_call_id: &str) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: None,
        }
    }
}

/// Tool call from assistant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool call ID
    pub id: String,
    /// Tool type (always "function" for now)
    pub tool_type: String,
    /// Function call details
    pub function: FunctionCall,
}

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Function name
    pub name: String,
    /// Function arguments (JSON string)
    pub arguments: String,
}

/// Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool type
    pub tool_type: String,
    /// Function definition
    pub function: FunctionDefinition,
}

/// Function definition for tools
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    /// Function name
    pub name: String,
    /// Function description
    pub description: String,
    /// Parameters schema (JSON Schema)
    pub parameters: serde_json::Value,
}

/// Chat completion request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// Model to use
    pub model: String,
    /// Messages
    pub messages: Vec<ChatMessage>,
    /// Tools available
    pub tools: Option<Vec<ToolDefinition>>,
    /// Tool choice: auto, none, or specific tool
    pub tool_choice: Option<serde_json::Value>,
    /// Max tokens
    pub max_tokens: Option<u32>,
    /// Temperature
    pub temperature: Option<f32>,
    /// Top-p
    pub top_p: Option<f32>,
    /// Stream response
    pub stream: bool,
}

/// Chat completion response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    /// Response ID
    pub id: String,
    /// Object type
    pub object: String,
    /// Created timestamp
    pub created: u64,
    /// Model used
    pub model: String,
    /// Choices
    pub choices: Vec<Choice>,
    /// Usage statistics
    pub usage: Option<Usage>,
}

/// Choice in completion response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// Index
    pub index: u32,
    /// Message
    pub message: ChatMessage,
    /// Finish reason
    pub finish_reason: Option<String>,
}

/// Token usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens
    pub prompt_tokens: u32,
    /// Completion tokens
    pub completion_tokens: u32,
    /// Total tokens
    pub total_tokens: u32,
}

impl ProviderConfig {
    /// Get API key from config or environment
    pub fn get_api_key(&self) -> anyhow::Result<String> {
        if let Some(key) = &self.api_key {
            return Ok(key.clone());
        }

        if let Some(env_var) = &self.api_key_env {
            if let Ok(key) = std::env::var(env_var) {
                return Ok(key);
            }
        }

        anyhow::bail!(
            "API key not found. Set '{}' environment variable or provide api_key in config.",
            self.api_key_env.as_deref().unwrap_or("API_KEY")
        )
    }

    /// Get API key with secret resolution support
    ///
    /// This method checks if the `api_key` is a secret reference (e.g., `${secret:OPENAI_API_KEY}`)
    /// and resolves it using the provided secret resolver.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use pekobot::secrets::SecretResolver;
    /// use pekobot::types::provider::ProviderConfig;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let config = ProviderConfig::default();
    /// let resolver = SecretResolver::new().await?;
    /// resolver.unlock("password").await?;
    ///
    /// let api_key = config.get_api_key_resolved(&resolver).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_api_key_resolved(
        &self,
        resolver: &crate::secrets::SecretResolver,
    ) -> anyhow::Result<String> {
        // First check if we have a direct api_key that might be a secret reference
        if let Some(key) = &self.api_key {
            // Check if it's a secret reference
            if key.starts_with("${secret:") || key.starts_with("${env:") {
                return resolver.resolve(key).await;
            }
            // Plain value, return as-is
            return Ok(key.clone());
        }

        // Fall back to environment variable
        if let Some(env_var) = &self.api_key_env {
            if let Ok(key) = std::env::var(env_var) {
                // Also check if the env var value is a secret reference
                if key.starts_with("${secret:") || key.starts_with("${env:") {
                    return resolver.resolve(&key).await;
                }
                return Ok(key);
            }
        }

        anyhow::bail!(
            "API key not found. Set '{}' environment variable, provide api_key in config, \
             or store it with: pekobot secret set OPENAI_API_KEY",
            self.api_key_env.as_deref().unwrap_or("API_KEY")
        )
    }

    /// Check if the API key configuration contains secret references
    #[must_use]
    pub fn has_secret_reference(&self) -> bool {
        if let Some(key) = &self.api_key {
            if key.starts_with("${secret:") || key.starts_with("${env:") {
                return true;
            }
        }
        false
    }

    /// Get model configuration
    #[must_use]
    pub fn get_model_config(&self, model_name: &str) -> Option<&ModelConfig> {
        self.models.get(model_name)
    }

    /// Get default model configuration
    #[must_use]
    pub fn default_model_config(&self) -> Option<&ModelConfig> {
        self.get_model_config(&self.default_model)
    }

    /// Create `OpenAI` config
    #[must_use]
    pub fn openai(api_key: &str, model: &str) -> Self {
        let mut config = Self::default();
        config.provider_type = ProviderType::OpenAI;
        config.api_key = Some(api_key.to_string());
        config.default_model = "default".to_string();
        config.models.insert(
            "default".to_string(),
            ModelConfig {
                name: model.to_string(),
                ..ModelConfig::default()
            },
        );
        config
    }

    /// Create Ollama config
    #[must_use]
    pub fn ollama(base_url: &str, model: &str) -> Self {
        let mut config = Self::default();
        config.provider_type = ProviderType::Ollama;
        config.base_url = Some(base_url.to_string());
        config.default_model = "default".to_string();
        config.models.insert(
            "default".to_string(),
            ModelConfig {
                name: model.to_string(),
                ..ModelConfig::default()
            },
        );
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ProviderConfig::default();
        assert_eq!(config.provider_type, ProviderType::OpenAI);
        assert_eq!(config.timeout_seconds, 60);
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn test_chat_messages() {
        let system = ChatMessage::system("You are a helpful assistant.");
        let user = ChatMessage::user("Hello!");
        let assistant = ChatMessage::assistant("Hi there!");

        assert_eq!(system.role, "system");
        assert_eq!(user.role, "user");
        assert_eq!(assistant.role, "assistant");
    }

    #[test]
    fn test_model_config() {
        let config = ModelConfig::default();
        assert_eq!(config.name, "gpt-4o-mini");
        assert_eq!(config.temperature, 0.7);
    }

    #[test]
    fn test_provider_type_display() {
        assert_eq!(ProviderType::OpenAI.to_string(), "openai");
        assert_eq!(ProviderType::Anthropic.to_string(), "anthropic");
        assert_eq!(ProviderType::Ollama.to_string(), "ollama");
    }
}
