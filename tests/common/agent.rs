//! Agent-config helpers for CLI tests.
//!
//! Writes an `openai_compatible` provider config that points at the URL
//! passed in (the CI's `MOCK_LLM_URL`, e.g. `http://mock-llm:8080`).
//! Lives in the `~/.peko/agents/<name>/` layout the CLI expects.
//!
//! **Note:** the URL must go in `base_url`, not `api_key`. The provider
//! dispatch logic in `src/agent/agent.rs::init_provider` maps
//! `ProviderType::OpenAICompatible` to one of the built-in concrete
//! providers by inspecting `base_url`'s hostname; if `base_url` is
//! `None`, it falls back to `ProviderType::OpenAI`, which uses the
//! hardcoded `https://api.openai.com/v1` and treats `api_key` as a
//! real `Authorization: Bearer` token. With the URL stuck in
//! `api_key` we'd hit OpenAI for real and get back 401 Unauthorized.

#![allow(dead_code)]

use std::path::Path;

/// Write a minimal agent config that talks to the mock LLM at
/// `mock_llm_url`. The URL goes in `base_url`; `api_key` is a
/// placeholder string the mock LLM ignores.
///
/// Layout produced under `home/.peko/`:
///   agents/<name>/config.toml          provider/extensions/channels/prompt
///   agents/<name>/SYSTEM.md            empty system prompt
pub fn write_mock_agent(home: &Path, name: &str, mock_llm_url: &str) -> std::io::Result<()> {
    let agent_dir = home.join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;

    // Strip a trailing slash so URL composition in the provider transport
    // (`{base_url}{path}`) doesn't end up with `//v1/chat/completions`.
    let base_url = mock_llm_url.trim_end_matches('/');

    let config_toml = format!(
        r#"version = "1.0"
name = "{name}"
description = "CLI integration test agent"
auto_accept_trusted = false
default_timeout_seconds = 60

[provider]
provider_type = "openai_compatible"
api_key = "mock-llm-test-key"
base_url = "{base_url}"
default_model = "default"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "default"
max_tokens = 1024
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

[extensions]
enabled = []

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}
"#
    );

    std::fs::write(agent_dir.join("config.toml"), config_toml)?;
    std::fs::write(agent_dir.join("SYSTEM.md"), "")?;
    Ok(())
}
