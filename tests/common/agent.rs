//! Agent-config helpers for CLI tests.
//!
//! Writes a v3 agent config (no `[provider]` block) that references
//! the catalog entry `mock-llm` (seeded by `seed_mock_provider_in_catalog`
//! below). The catalog entry holds the actual base_url and api_key.
//! Lives in the `~/.peko/agents/<name>/` layout the CLI expects.

#![allow(dead_code)]

use std::path::Path;

/// Write a minimal agent config that points at the catalog entry
/// `mock-llm` (which `seed_mock_provider_in_catalog` writes to
/// `~/.peko/providers.toml`). The agent itself only carries the
/// soft hints — no `[provider]` block.
///
/// Layout produced under `home/.peko/`:
///   agents/<name>/config.toml          v3 agent config with soft hints
///   agents/<name>/SYSTEM.md            empty system prompt
pub fn write_v3_mock_agent(home: &Path, name: &str, _mock_llm_url: &str) -> std::io::Result<()> {
    let agent_dir = home.join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;

    let config_toml = format!(
        r#"version = "3.0"
name = "{name}"
description = "CLI integration test agent"
auto_accept_trusted = false

preferred_provider_id = "mock-llm"
preferred_model_id = "default"
default_timeout_seconds = 60

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

/// (Removed: the v3 rename already happened, so callers should use
/// `write_v3_mock_agent` directly. The deprecated alias is removed.)
/// Seed a `mock-llm` catalog entry pointing at `mock_llm_url`. The
/// test harness invokes this before spawning the daemon so the
/// daemon's `LlmResolver` finds the entry on first lookup.
///
/// In production CI / Linux, the OS keychain isn't available, so the
/// daemon additionally honors `PEKO_TEST_RESOLVER_BOOTSTRAP=1` to
/// fall back to `MOCK_LLM_API_KEY`. `PekoCli::cmd` exports both
/// env vars whenever `MOCK_LLM_URL` is set.
///
/// Idempotent: re-running with the same `mock_llm_url` overwrites
/// the entry with the same values.
pub fn seed_mock_provider_in_catalog(home: &Path, mock_llm_url: &str) {
    use pekobot::providers::catalog::{
        ApiFormat, ModelInfo, ProviderCatalogEntry, ProviderCatalogFile,
    };
    use std::collections::BTreeMap;

    let peko_dir = home.join(".peko");
    let catalog_path = peko_dir.join("providers.toml");
    if let Some(parent) = catalog_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let base_url = mock_llm_url.trim_end_matches('/').to_string();
    let now = chrono::Utc::now();
    let entry = ProviderCatalogEntry {
        id: "mock-llm".to_string(),
        display_name: "mock-llm".to_string(),
        template_id: None,
        api_format: ApiFormat::OpenaiCompletions,
        base_url,
        default_model_id: "default".to_string(),
        models: vec![ModelInfo {
            id: "default".to_string(),
            display_name: None,
            context_length: None,
            max_output_tokens: None,
            capabilities: vec![],
        }],
        headers: BTreeMap::new(),
        requires_key: true,
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let mut entries = BTreeMap::new();
    entries.insert("mock-llm".to_string(), entry);
    let file = ProviderCatalogFile {
        version: "3.0".to_string(),
        entries,
        default_provider_id: None,
        default_model_id: None,
    };
    let toml = toml::to_string_pretty(&file).expect("serialize catalog");
    std::fs::write(&catalog_path, toml).expect("write catalog");
}
