//! Configuration management

use serde::{Deserialize, Serialize};

/// Pekobot configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub agent: AgentConfig,
    pub providers: ProvidersConfig,
    pub memory: MemoryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersConfig {
    pub default: String,
    pub openai: Option<ApiKeyConfig>,
    pub anthropic: Option<ApiKeyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyConfig {
    pub api_key: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub backend: String, // "sqlite" or "none"
    pub path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent: AgentConfig {
                name: "pekobot".to_string(),
                description: None,
            },
            providers: ProvidersConfig {
                default: "openai".to_string(),
                openai: None,
                anthropic: None,
            },
            memory: MemoryConfig {
                backend: "sqlite".to_string(),
                path: None,
            },
        }
    }
}
