//! Agent-config helpers for CLI tests.
//!
//! Writes the same `openai_compatible`-pointed-at-`MOCK_LLM_URL` config that
//! `tunnel_e2e.rs` builds (see lines 254-261 there), but in the `~/.peko/`
//! layout the CLI expects (under an isolated [`PekoCli`] home).

#![allow(dead_code)]

use std::path::Path;

/// Write a minimal agent config that talks to the mock LLM.
///
/// Layout produced under `home/.peko/`:
///   agents/<name>/config.toml          provider/extensions/channels/prompt
///   agents/<name>/SYSTEM.md            empty system prompt
pub fn write_mock_agent(home: &Path, name: &str, mock_llm_url: &str) -> std::io::Result<()> {
    let agent_dir = home.join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;

    let config_toml = format!(
        r#"version = "1.0"
name = "{name}"
description = "CLI integration test agent"
auto_accept_trusted = false
default_timeout_seconds = 60

[provider]
provider_type = "openai_compatible"
api_key = "{mock_llm_url}"
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
