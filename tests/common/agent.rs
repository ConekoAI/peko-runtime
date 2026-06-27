//! Agent-config helpers for CLI tests.
//!
//! Writes a v3 agent config (no `[provider]` block) that references
//! the catalog entry `mock-llm` (seeded by `seed_mock_provider_in_catalog`
//! below). The catalog entry holds the actual base_url and api_key.
//! Lives in the `~/.peko/agents/<name>/` layout the CLI expects.

#![allow(dead_code)]

use super::cli::PekoCli;
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

/// Create a Principal wired to the mock LLM provider and ready to receive
/// `peko send` from the CLI caller (`user:default`).
///
/// This is the Principal-era replacement for `write_v3_mock_agent`: after
/// the "Principal as the single actor" migration, `peko send <name>` targets
/// a Principal (`PrincipalSend` → `PrincipalManager::receive`), not a legacy
/// `~/.peko/agents/<name>/` config. Tests that drive the LLM call path must
/// therefore create a Principal, not an agent.
///
/// Steps:
///  1. Seed `mock-llm` as the sole catalog entry, so the supervisor agent's
///     provider resolution falls through to it (last-resort "first enabled
///     catalog entry" rule in `LlmResolver`).
///  2. Run the real `peko principal create <name>` command, exercising the
///     actual framework: it writes the workspace, `agents/primary.md`
///     prompt, identity, and `principal.toml`.
///
/// No owner rewrite is needed: `peko principal create` defaults the owner to
/// `user:default`, which is exactly the caller `peko send` presents
/// (`GlobalPaths::user()` defaults to `"default"`), so the `Permission::Chat`
/// owner-check in `PrincipalManager::receive` passes. (This differs from the
/// `s6` IPC scenario, where the caller is the local-socket `user:local` and
/// the owner must be patched to match.)
///
/// Must be called BEFORE `DaemonGuard::spawn` (like `write_v3_mock_agent`):
/// `peko principal create` writes files directly and needs no daemon.
pub fn create_mock_principal(cli: &PekoCli, name: &str, mock_llm_url: &str) {
    create_mock_principal_with_tools(cli, name, mock_llm_url, &[]);
}

/// Like [`create_mock_principal`], but additionally grants the Principal a set
/// of capability tools.
///
/// The supervisor agent's tool whitelist is the union of a fixed base set
/// (`Read`, `glob`, `grep`, `session`, `Cron*`, `Task*` — see
/// `src/principal/agent_runner.rs::run_supervisor_prompt`) and the Principal's
/// `[capabilities] tools`. Tests that drive the supervisor into calling tools
/// outside the base set (e.g. `Write`, `Bash`, `Agent`) must grant them here,
/// or the runtime's tool dispatcher rejects the tool_call.
///
/// `tools` are bare tool names (e.g. `"Write"`, `"Bash"`, `"Agent"`); this
/// helper writes them into `principals/<name>/principal.toml` under
/// `[capabilities] tools` after `peko principal create`.
pub fn create_mock_principal_with_tools(
    cli: &PekoCli,
    name: &str,
    mock_llm_url: &str,
    tools: &[&str],
) {
    seed_mock_provider_in_catalog(cli.home(), mock_llm_url);

    let output = cli
        .cmd()
        .args(["principal", "create", name])
        .output()
        .expect("run `peko principal create`");
    assert!(
        output.status.success(),
        "`peko principal create {name}` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    if tools.is_empty() {
        return;
    }

    // Patch the Principal's capability tools so the supervisor whitelist
    // includes them. We rewrite `principal.toml` directly rather than going
    // through a CLI grant path so the helper stays a single, daemon-free
    // setup step (callable before `DaemonGuard::spawn`).
    //
    // Each tool is granted in BOTH forms — the bare name (so the agent's
    // per-agent `init_builtins_async` registers the tool) and the canonical
    // `builtin:tool:<name>` extension ID (so the dispatcher's
    // `is_tool_enabled` owner check passes at execution time). Granting only
    // one form yields a silently-disabled tool. (The supervisor's fixed base
    // whitelist already carries both forms for Read/glob/grep/session/Cron*/
    // Task*; capability tools must supply both themselves.)
    let path = cli
        .peko_dir()
        .join("principals")
        .join(name)
        .join("principal.toml");
    let raw = std::fs::read_to_string(&path).expect("read principal.toml");
    let mut cfg: peko::principal::config::PrincipalConfig =
        toml::from_str(&raw).expect("parse principal.toml");
    cfg.capabilities.tools = tools
        .iter()
        .flat_map(|t| [t.to_string(), format!("builtin:tool:{t}")])
        .collect();
    std::fs::write(
        &path,
        toml::to_string_pretty(&cfg).expect("serialize principal.toml"),
    )
    .expect("write principal.toml");
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
    use peko::providers::catalog::{
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

/// Seed a `minimax` catalog entry pointing at the production MiniMax
/// (Anthropic-compatible) endpoint. The API key is read from the
/// `MINIMAX_API_KEY` env var via `PEKO_TEST_RESOLVER_BOOTSTRAP=1`.
pub fn seed_minimax_provider_in_catalog(home: &Path) {
    use peko::providers::catalog::{
        ApiFormat, ModelInfo, ProviderCatalogEntry, ProviderCatalogFile,
    };
    use std::collections::BTreeMap;

    let peko_dir = home.join(".peko");
    let catalog_path = peko_dir.join("providers.toml");
    if let Some(parent) = catalog_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = chrono::Utc::now();
    let entry = ProviderCatalogEntry {
        id: "minimax".to_string(),
        display_name: "MiniMax".to_string(),
        template_id: None,
        api_format: ApiFormat::AnthropicMessages,
        base_url: "https://api.minimaxi.com/anthropic".to_string(),
        default_model_id: "MiniMax-M3".to_string(),
        models: vec![ModelInfo {
            id: "MiniMax-M3".to_string(),
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
    entries.insert("minimax".to_string(), entry);
    let file = ProviderCatalogFile {
        version: "3.0".to_string(),
        entries,
        default_provider_id: None,
        default_model_id: None,
    };
    let toml = toml::to_string_pretty(&file).expect("serialize catalog");
    std::fs::write(&catalog_path, toml).expect("write catalog");
}

/// Seed a `kimi` catalog entry pointing at the Kimi Code API endpoint.
/// The API key is read from the `KIMI_API_KEY` env var via
/// `PEKO_TEST_RESOLVER_BOOTSTRAP=1`.
pub fn seed_kimi_provider_in_catalog(home: &Path) {
    use peko::providers::catalog::{
        ApiFormat, ModelInfo, ProviderCatalogEntry, ProviderCatalogFile,
    };
    use std::collections::BTreeMap;

    let peko_dir = home.join(".peko");
    let catalog_path = peko_dir.join("providers.toml");
    if let Some(parent) = catalog_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = chrono::Utc::now();
    let entry = ProviderCatalogEntry {
        id: "kimi".to_string(),
        display_name: "Kimi (Kimi Code API)".to_string(),
        template_id: None,
        api_format: ApiFormat::AnthropicMessages,
        base_url: "https://api.kimi.com/coding".to_string(),
        default_model_id: "kimi-for-coding".to_string(),
        models: vec![ModelInfo {
            id: "kimi-for-coding".to_string(),
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
    entries.insert("kimi".to_string(), entry);
    let file = ProviderCatalogFile {
        version: "3.0".to_string(),
        entries,
        default_provider_id: None,
        default_model_id: None,
    };
    let toml = toml::to_string_pretty(&file).expect("serialize catalog");
    std::fs::write(&catalog_path, toml).expect("write catalog");
}
