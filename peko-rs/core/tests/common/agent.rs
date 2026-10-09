//! Agent-config helpers for CLI tests.
//!
//! Lives in the `~/.peko/` layout the CLI expects.
#![allow(dead_code)]

use super::cli::PekoCli;
use std::path::Path;

/// Create a Principal wired to the mock LLM provider and ready to receive
/// `peko send` from the CLI caller (`user:default`).
///
/// Since the "Principal as the single actor" migration, `peko send <name>`
/// targets a Principal (`PrincipalSend` → `PrincipalManager::receive`), not
/// a legacy `~/.peko/agents/<name>/` config. Tests that drive the LLM call
/// path must therefore create a Principal, not an agent.
///
/// Steps:
///  1. Seed `mock-llm` as the sole catalog entry and pin the Principal
///     to it via `peko create --model mock-llm` (model-first:
///     there is no resolver fallback — an unpinned principal fails every
///     send with "no model configured").
///  2. Run the real `peko create <name>` command, exercising the
///     actual framework: it writes the workspace, `agents/root/AGENT.md`
///     prompt, identity, and `principal.toml`.
///
/// No owner rewrite is needed: `peko create` stamps the
/// owner from the real caller (`caller.subject()` — ADR-057), which
/// for the local CLI is `user:local`, and the caller `peko send`
/// presents is the same derived identity (`user:local`, or the hub
/// owner when the runtime is logged into pekohub), so the
/// `Permission::Chat` owner-check in `PrincipalManager::receive`
/// passes.
///
/// Must be called BEFORE `DaemonGuard::spawn`:
/// `peko create` writes files directly and needs no daemon.
pub fn create_mock_principal(cli: &PekoCli, name: &str, mock_llm_url: &str) {
    create_mock_principal_with_tools(cli, name, mock_llm_url, &[]);
}

/// Like [`create_mock_principal`]. The `tools` list is accepted for
/// call-site compatibility and ignored: ADR-066 P2 deleted the
/// capability gate, so a freshly created principal sees every tool
/// (presence = executability) and `principal.toml` carries no grants.
pub fn create_mock_principal_with_tools(
    cli: &PekoCli,
    name: &str,
    mock_llm_url: &str,
    tools: &[&str],
) {
    seed_mock_provider_in_catalog(cli.home(), mock_llm_url);

    let output = cli
        .cmd()
        .args(["create", name, "--model", "mock-llm"])
        .output()
        .expect("run `peko create`");
    assert!(
        output.status.success(),
        "`peko create {name} --model mock-llm` failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // ADR-066 P2: grants are ignored on load and never persisted, so
    // there is nothing to patch. The parameter exists for call-site
    // compatibility only.
    let _ = tools;
}

/// Seed one configured-model entry in the model catalog at
/// `~/.peko/models.toml`. The public seeders below delegate
/// here; the only differences are the entry's id, endpoint format,
/// base URL, and wire model id.
///
/// Idempotent: re-running with the same parameters overwrites the
/// entry with the same values.
fn seed_model_in_catalog(
    home: &Path,
    id: &str,
    display_name: &str,
    api_format: peko_providers::catalog::ApiFormat,
    base_url: &str,
    wire_model_id: &str,
) {
    use peko_providers::catalog::{ModelCatalogFile, ModelConfig};
    use std::collections::BTreeMap;

    let peko_dir = home.join(".peko");
    let catalog_path = peko_dir.join("models.toml");
    if let Some(parent) = catalog_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = chrono::Utc::now();
    let entry = ModelConfig {
        id: id.to_string(),
        display_name: display_name.to_string(),
        template_id: None,
        api_format,
        base_url: base_url.to_string(),
        model_id: wire_model_id.to_string(),
        context_window: None,
        max_output_tokens: None,
        headers: BTreeMap::new(),
        credential_id: None,
        requires_key: true,
        enabled: true,
        created_at: now,
        updated_at: now,
        compat: None,
        spec: None,
        // Phase 2 of `feature/multi-model-subagents`: test helper
        // seeds catalog entries without a user annotation.
        note: None,
    };
    let mut entries = BTreeMap::new();
    entries.insert(id.to_string(), entry);
    let file = ModelCatalogFile {
        version: "4.0".to_string(),
        entries,
    };
    let toml = toml::to_string_pretty(&file).expect("serialize catalog");
    std::fs::write(&catalog_path, toml).expect("write catalog");
}

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
    seed_model_in_catalog(
        home,
        "mock-llm",
        "mock-llm",
        peko_providers::catalog::ApiFormat::OpenaiCompletions,
        mock_llm_url.trim_end_matches('/'),
        "default",
    );
}

/// Seed the env-described real LLM ([`super::real_llm::RealLlm`]) as the
/// catalog entry [`super::real_llm::REAL_LLM_MODEL_ID`]. Its key comes from
/// `LLM_API_KEY` via `PEKO_TEST_RESOLVER_BOOTSTRAP=1`.
pub fn seed_real_llm_in_catalog(home: &Path, llm: &super::real_llm::RealLlm) {
    let entry = llm.catalog_entry();
    seed_model_in_catalog(
        home,
        &entry.id,
        &entry.display_name,
        entry.api_format,
        &entry.base_url,
        &entry.model_id,
    );
}
