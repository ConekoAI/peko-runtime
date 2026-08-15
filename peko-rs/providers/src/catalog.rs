//! Model catalog — runtime-owned list of configured LLM models.
//!
//! The catalog is a single TOML file at `~/.peko/models.toml`, loaded
//! once on startup and shared across the runtime via
//! `Arc<RwLock<ModelCatalog>>`. The credential vault (see
//! `common::vault`) holds the API keys referenced by each model's
//! `credential_id`.
//!
//! ## Design properties
//!
//! - **Model-first.** A configured model bundles endpoint info (base
//!   URL, API format, headers), the wire model id, context-window
//!   metadata, and a reference to a credential. There is no separate
//!   provider layer.
//! - **Templates vs. entries.** Preset templates
//!   (`crate::templates`) describe a known provider with
//!   curated model lists. They are static code. `ModelConfig` is the
//!   runtime-owned instance of a configured model.
//! - **No secrets on disk.** API keys live in the vault; the catalog
//!   only stores public metadata and a `credential_id`.
//! - **No runtime default.** Every Principal must be created with a
//!   configured model; per-send overrides use `--model <id>`.
//! - **Enabled flag.** Disabled entries remain in the catalog but are
//!   not considered for resolution.
//!
//! ## Persistence
//!
//! Writes are atomic: serialize, write to `models.toml.tmp`, then
//! rename. Reads tolerate a missing or empty file (returns an empty
//! catalog).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::spec::ModelSpec;
use crate::templates::ProviderTemplate;
use peko_provider_api::ProviderCompat;

/// Top-level API format understood by the runtime.
///
/// The runtime ships adapters for these formats. Custom models
/// declared via `peko model add --custom --api-format <FMT>` must use
/// one of these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    /// OpenAI Chat Completions API. Compatible with OpenAI, Groq,
    /// Together, OpenRouter, Ollama, vLLM, llama.cpp, …
    OpenaiCompletions,
    /// Anthropic Messages API. Compatible with Anthropic, Kimi Code,
    /// MiniMax, …
    AnthropicMessages,
    /// OpenAI Responses API (`POST /v1/responses`). Successor surface
    /// to Chat Completions; preferred by gpt-4.1, gpt-5, and o-series
    /// reasoning models. Carries `instructions` + `input` items
    /// instead of `messages[]` and exposes a distinct SSE event
    /// family. Compatible with OpenAI direct and the Azure
    /// Responses endpoint.
    OpenAiResponses,
}

impl ApiFormat {
    /// Stable wire id used in CLI / IPC.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiFormat::OpenaiCompletions => "openai_completions",
            ApiFormat::AnthropicMessages => "anthropic_messages",
            ApiFormat::OpenAiResponses => "openai_responses",
        }
    }

    /// Parse from wire id. Accepts both the canonical enum forms and
    /// the short "openai"/"anthropic"/"responses" ids emitted by the
    /// desktop UI.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "openai_completions" | "openai-completions" | "openai" => Some(Self::OpenaiCompletions),
            "anthropic_messages" | "anthropic-messages" | "anthropic" => {
                Some(Self::AnthropicMessages)
            }
            "openai_responses" | "openai-responses" | "responses" => Some(Self::OpenAiResponses),
            _ => None,
        }
    }
}

impl std::fmt::Display for ApiFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One configured model entry in the runtime-owned catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Stable, lowercase, filesystem-safe configured model id.
    /// This is the canonical lookup key used by `LlmResolver`,
    /// `peko model …`, IPC handlers, and principal configs.
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Optional template id this entry was seeded from (e.g.
    /// `"anthropic"`, `"openai"`). `None` for fully custom entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Wire format used to talk to the model endpoint.
    pub api_format: ApiFormat,
    /// Base URL for the API.
    pub base_url: String,
    /// Model id as it appears on the wire (e.g. `gpt-4o`,
    /// `claude-sonnet-4-5`).
    pub model_id: String,
    /// Maximum context length in tokens (input + output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Maximum output tokens for a single response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Optional extra HTTP headers (e.g. `OpenAI-Organization`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Reference to a credential in the vault. `None` means the model
    /// does not require an API key (e.g. a local Ollama endpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    /// Whether this model requires an API key. Used by the UI to decide
    /// whether to prompt for a credential.
    #[serde(default = "default_true")]
    pub requires_key: bool,
    /// Whether this model is eligible for resolution.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Bookkeeping.
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
    /// F29: per-provider adapter hints resolved from the template.
    /// `None` means "use the adapter's built-in F25 default" — the
    /// pre-F29 behaviour. When `Some`, the adapter projects
    /// `ChatOptions::thinking_effort` onto the wire shape named by
    /// `compat.thinking_format` (DeepSeek / Kimi / OpenRouter /
    /// Together / Qwen / Zai / native Anthropic / Responses / Chat
    /// Completions). Field is skipped from JSONL when absent so
    /// pre-F29 entries keep loading without migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<ProviderCompat>,
    /// PR 1 / `feature/model-first-config`: declarative model
    /// capability descriptor (vision, audio, tools, streaming,
    /// thinking, json_mode, pricing). `None` for entries written
    /// before PR 1; the engine falls back to conservative
    /// `ModelSpec::default()` (text-only, no tools, no thinking,
    /// streaming on). Templates that have been audited copy
    /// `ModelSpec` from `ModelTemplate::spec` at create time;
    /// users editing a custom entry via `peko model edit` can
    /// override it directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<ModelSpec>,
    /// Phase 2 of `feature/multi-model-subagents`: free-text
    /// user note attached to this catalog entry. Surfaces on the
    /// `model_list` tool and in `peko model show` so a parent
    /// agent can reason about model choice using both
    /// standardized `ModelSpec` flags and subjective annotations
    /// ("very cheap, use it for cron", "RPG model, use it to
    /// generate fictions", "very capable, use it for coding")
    /// that the spec cannot capture. `None` keeps the pre-Phase-2
    /// behavior. Capped at 500 chars (validated in `from_template`
    /// and `upsert`). `serde-default` skips the field on read so
    /// entries written before Phase 2 still load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Maximum length of a model `note`, in chars. Subjective
/// annotations that blow past this cap are almost certainly
/// pasted-in prompt fragments that don't belong on the catalog —
/// reject early so the UI / CLI can show a clean error.
pub const NOTE_MAX_CHARS: usize = 500;

fn validate_note(note: Option<String>) -> Result<Option<String>> {
    let Some(s) = note else {
        return Ok(None);
    };
    if s.chars().count() > NOTE_MAX_CHARS {
        anyhow::bail!(
            "model note is {n} chars; max is {max}",
            n = s.chars().count(),
            max = NOTE_MAX_CHARS
        );
    }
    Ok(Some(s))
}

fn default_true() -> bool {
    true
}

impl ModelConfig {
    /// Construct a `ModelConfig` from a preset template, with the
    /// user-supplied configured model id and a chosen wire model id.
    /// The template's curated metadata for that wire model is used when
    /// available; otherwise the entry carries no context-window metadata.
    #[must_use]
    pub fn from_template(
        template: &ProviderTemplate,
        id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let model_id = model_id.into();
        let display_name = if let Some(m) = template.models.iter().find(|m| m.id == model_id) {
            m.display_name
                .map(str::to_string)
                .unwrap_or_else(|| model_id.clone())
        } else {
            model_id.clone()
        };
        let (context_window, max_output_tokens, spec) = template
            .models
            .iter()
            .find(|m| m.id == model_id)
            .map(|m| (m.context_length, m.max_output_tokens, m.spec))
            .unwrap_or((None, None, None));
        Self {
            id,
            display_name,
            template_id: Some(template.id.to_string()),
            api_format: template.api_format,
            base_url: template.base_url.to_string(),
            model_id,
            context_window,
            max_output_tokens,
            headers: template
                .headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            credential_id: None,
            requires_key: template.requires_key,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            compat: template.compat,
            spec,
            // `from_template` is the only construction path that
            // isn't already gated by `upsert()`'s note validation,
            // so seed with `None`. Users add the note via
            // `peko model edit --note ...`, which routes through
            // `upsert`.
            note: None,
        }
    }
}

/// On-disk schema for `~/.peko/models.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogFile {
    #[serde(default = "default_catalog_version")]
    pub version: String,
    #[serde(default)]
    pub entries: BTreeMap<String, ModelConfig>,
}

impl Default for ModelCatalogFile {
    fn default() -> Self {
        Self {
            version: default_catalog_version(),
            entries: BTreeMap::new(),
        }
    }
}

fn default_catalog_version() -> String {
    "4.0".to_string()
}

/// In-memory model catalog, shared across the runtime.
pub struct ModelCatalog {
    path: PathBuf,
    inner: RwLock<ModelCatalogFile>,
}

impl ModelCatalog {
    /// Default filename under the config directory.
    pub const FILENAME: &'static str = "models.toml";

    /// Load the catalog from `path`, or create an empty one if the file
    /// does not exist. A corrupt file is logged and treated as empty
    /// (with a backup written to `models.toml.bak`) so the runtime can
    /// still start.
    pub async fn load_or_init(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        let file = if path.exists() {
            match Self::read_file(&path) {
                Ok(f) => f,
                Err(e) => {
                    warn!(
                        "models.toml at {} is corrupt ({e}); backing up and starting empty",
                        path.display()
                    );
                    let _ = std::fs::copy(&path, path.with_extension("toml.bak"));
                    ModelCatalogFile::default()
                }
            }
        } else {
            ModelCatalogFile::default()
        };
        Ok(Arc::new(Self {
            path,
            inner: RwLock::new(file),
        }))
    }

    fn read_file(path: &Path) -> Result<ModelCatalogFile> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let parsed: ModelCatalogFile =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(parsed)
    }

    /// Return a snapshot of the catalog file.
    pub async fn snapshot(&self) -> ModelCatalogFile {
        self.inner.read().await.clone()
    }

    /// Re-read the on-disk catalog into this Arc's inner state.
    pub async fn reload(&self) -> Result<usize> {
        let file = if self.path.exists() {
            match Self::read_file(&self.path) {
                Ok(f) => f,
                Err(e) => {
                    warn!(
                        "models.toml reload at {} failed ({e}); keeping prior in-memory state",
                        self.path.display()
                    );
                    return Ok(self.inner.read().await.entries.len());
                }
            }
        } else {
            ModelCatalogFile::default()
        };
        let count = file.entries.len();
        let mut guard = self.inner.write().await;
        *guard = file;
        Ok(count)
    }

    /// List all enabled entries.
    pub async fn list_enabled(&self) -> Vec<ModelConfig> {
        let guard = self.inner.read().await;
        guard
            .entries
            .values()
            .filter(|e| e.enabled)
            .cloned()
            .collect()
    }

    /// List every entry, including disabled ones.
    pub async fn list_all(&self) -> Vec<ModelConfig> {
        let guard = self.inner.read().await;
        guard.entries.values().cloned().collect()
    }

    /// Look up an entry by id.
    pub async fn get(&self, id: &str) -> Option<ModelConfig> {
        let guard = self.inner.read().await;
        guard.entries.get(id).cloned()
    }

    /// Look up an enabled entry by id.
    pub async fn get_enabled(&self, id: &str) -> Option<ModelConfig> {
        let guard = self.inner.read().await;
        guard.entries.get(id).filter(|e| e.enabled).cloned()
    }

    /// Resolve the maximum context length in tokens for a configured
    /// model id. Returns `None` when the model is unknown, disabled, or
    /// has no `context_window` set.
    pub async fn context_window(&self, id: &str) -> Option<u32> {
        self.get_enabled(id).await.and_then(|m| m.context_window)
    }

    /// Add or replace an entry. Bumps `updated_at`.
    ///
    /// Phase 2: validates `entry.note` against [`NOTE_MAX_CHARS`]
    /// before persisting; a too-long note is rejected with an
    /// `anyhow::Error` carrying a clear message so callers (the
    /// CLI's `peko model add --note`, the `peko model edit --note`
    /// handler, and the IPC `models.upsert` handler) can surface
    /// the exact limit.
    pub async fn upsert(&self, entry: ModelConfig) -> Result<()> {
        let entry = ModelConfig {
            note: validate_note(entry.note)?,
            ..entry
        };
        {
            let mut guard = self.inner.write().await;
            let mut entry = entry;
            entry.updated_at = Utc::now();
            guard.entries.insert(entry.id.clone(), entry);
        }
        self.persist().await
    }

    /// Remove an entry by id. Returns `true` if an entry was removed.
    pub async fn remove(&self, id: &str) -> Result<bool> {
        let removed = {
            let mut guard = self.inner.write().await;
            guard.entries.remove(id).is_some()
        };
        if removed {
            self.persist().await?;
        }
        Ok(removed)
    }

    /// Catalog entries that reference the given credential id.
    /// Used by the credential-deletion safety check (PR 3 / feature
    /// branch `feature/model-first-config`) to refuse a delete that
    /// would orphan a configured model. Returns a fresh `Vec`; safe
    /// to call without holding any lock.
    pub async fn models_referencing(&self, credential_id: &str) -> Vec<ModelConfig> {
        let guard = self.inner.read().await;
        guard
            .entries
            .values()
            .filter(|e| e.credential_id.as_deref() == Some(credential_id))
            .cloned()
            .collect()
    }

    /// Rebind every catalog entry that referenced `from` to `to`.
    /// Returns the count of rewritten entries. Persists only when at
    /// least one entry was rewritten. Used by
    /// `peko credential set --replace-on <old-id>` to bulk-swap a
    /// credential across dependents without breaking them.
    pub async fn rewire_credential(&self, from: &str, to: &str) -> Result<usize> {
        let mut changed = 0usize;
        {
            let mut guard = self.inner.write().await;
            for entry in guard.entries.values_mut() {
                if entry.credential_id.as_deref() == Some(from) {
                    entry.credential_id = Some(to.to_string());
                    entry.updated_at = Utc::now();
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            self.persist().await?;
        }
        Ok(changed)
    }

    /// Atomically persist the in-memory catalog to disk.
    pub async fn persist(&self) -> Result<()> {
        let snapshot = {
            let guard = self.inner.read().await;
            guard.clone()
        };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating catalog parent dir {}", parent.display()))?;
        }
        let serialized = toml::to_string_pretty(&snapshot).context("serializing models.toml")?;
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, &serialized).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), self.path.display()))?;
        info!(
            "persisted model catalog to {} ({} entries)",
            self.path.display(),
            snapshot.entries.len()
        );
        Ok(())
    }

    /// On-disk path of this catalog.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates;
    use tempfile::tempdir;

    async fn temp_catalog() -> (tempfile::TempDir, Arc<ModelCatalog>) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.toml");
        let cat = ModelCatalog::load_or_init(&path).await.unwrap();
        (dir, cat)
    }

    #[test]
    fn api_format_wire_roundtrip() {
        for fmt in [
            ApiFormat::OpenaiCompletions,
            ApiFormat::AnthropicMessages,
            ApiFormat::OpenAiResponses,
        ] {
            let s = fmt.as_str();
            let back = ApiFormat::from_wire(s).unwrap();
            assert_eq!(fmt, back);
        }
        assert!(ApiFormat::from_wire("garbage").is_none());
    }

    #[test]
    fn api_format_accepts_short_desktop_ids() {
        assert_eq!(
            ApiFormat::from_wire("openai"),
            Some(ApiFormat::OpenaiCompletions)
        );
        assert_eq!(
            ApiFormat::from_wire("anthropic"),
            Some(ApiFormat::AnthropicMessages)
        );
        assert_eq!(
            ApiFormat::from_wire("responses"),
            Some(ApiFormat::OpenAiResponses)
        );
        assert_eq!(
            ApiFormat::from_wire("openai_responses"),
            Some(ApiFormat::OpenAiResponses)
        );
    }

    #[test]
    fn empty_catalog_loads_cleanly() {
        let (_dir, cat) = tokio_test::block_on(temp_catalog());
        let snap = tokio_test::block_on(cat.snapshot());
        assert_eq!(snap.entries.len(), 0);
        assert_eq!(snap.version, "4.0");
    }

    #[test]
    fn upsert_persists_to_disk() {
        let (dir, cat) = tokio_test::block_on(temp_catalog());
        let tmpl = templates::find_template("anthropic").unwrap();
        let entry = ModelConfig::from_template(tmpl, "anthropic-haiku", "claude-3-5-haiku-latest");
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        let reloaded =
            tokio_test::block_on(ModelCatalog::load_or_init(dir.path().join("models.toml")))
                .unwrap();
        let got = tokio_test::block_on(reloaded.get("anthropic-haiku")).unwrap();
        assert_eq!(got.api_format, ApiFormat::AnthropicMessages);
        assert!(got.requires_key);
        assert_eq!(got.model_id, "claude-3-5-haiku-latest");
    }

    #[test]
    fn remove_returns_true_then_false() {
        let (_dir, cat) = tokio_test::block_on(temp_catalog());
        let tmpl = templates::find_template("openai").unwrap();
        let entry = ModelConfig::from_template(tmpl, "openai-gpt-4o", "gpt-4o");
        tokio_test::block_on(cat.upsert(entry)).unwrap();
        assert!(tokio_test::block_on(cat.remove("openai-gpt-4o")).unwrap());
        assert!(!tokio_test::block_on(cat.remove("openai-gpt-4o")).unwrap());
    }

    #[test]
    fn context_window_resolves_from_catalog() {
        let (_dir, cat) = tokio_test::block_on(temp_catalog());
        let tmpl = templates::find_template("anthropic").unwrap();
        let entry = ModelConfig::from_template(tmpl, "anthropic-sonnet", "claude-sonnet-4-5");
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        assert_eq!(
            tokio_test::block_on(cat.context_window("anthropic-sonnet")),
            Some(200_000)
        );
        assert_eq!(
            tokio_test::block_on(cat.context_window("unknown-model")),
            None
        );
    }

    #[test]
    fn context_window_returns_none_for_disabled_entry() {
        let (_dir, cat) = tokio_test::block_on(temp_catalog());
        let tmpl = templates::find_template("anthropic").unwrap();
        let mut entry = ModelConfig::from_template(tmpl, "anthropic-sonnet", "claude-sonnet-4-5");
        entry.enabled = false;
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        assert_eq!(
            tokio_test::block_on(cat.context_window("anthropic-sonnet")),
            None
        );
    }

    #[test]
    fn corrupt_catalog_falls_back_to_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.toml");
        std::fs::write(&path, "this is not valid toml = = =").unwrap();

        let cat = tokio_test::block_on(ModelCatalog::load_or_init(&path)).unwrap();
        let snap = tokio_test::block_on(cat.snapshot());
        assert!(snap.entries.is_empty());
        assert!(path.with_extension("toml.bak").exists());
    }

    #[test]
    fn entry_from_template_seeds_metadata() {
        let tmpl = templates::find_template("anthropic").unwrap();
        let entry = ModelConfig::from_template(tmpl, "anthropic-haiku", "claude-3-5-haiku-latest");
        assert_eq!(entry.template_id.as_deref(), Some("anthropic"));
        assert_eq!(entry.model_id, "claude-3-5-haiku-latest");
        assert!(entry.context_window.is_some());
    }

    #[tokio::test]
    async fn reload_picks_up_disk_changes_through_same_arc() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.toml");
        let cat = ModelCatalog::load_or_init(&path).await.unwrap();

        assert_eq!(cat.list_all().await.len(), 0);

        let tmpl = templates::find_template("anthropic").unwrap();
        let entry = ModelConfig::from_template(tmpl, "anthropic-sonnet", "claude-sonnet-4-5");
        let file = ModelCatalogFile {
            entries: std::iter::once(("anthropic-sonnet".to_string(), entry)).collect(),
            ..Default::default()
        };
        std::fs::write(&path, toml::to_string(&file).expect("serialize model file")).unwrap();

        assert_eq!(cat.list_all().await.len(), 0);

        let count = cat.reload().await.unwrap();
        assert_eq!(count, 1);
        assert_eq!(cat.list_all().await.len(), 1);
        assert_eq!(
            cat.get("anthropic-sonnet").await.unwrap().id,
            "anthropic-sonnet"
        );
    }

    #[tokio::test]
    async fn reload_keeps_prior_state_on_read_failure() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.toml");
        let cat = ModelCatalog::load_or_init(&path).await.unwrap();

        let tmpl = templates::find_template("ollama").unwrap();
        cat.upsert(ModelConfig::from_template(tmpl, "ollama-llama", "llama3.1"))
            .await
            .unwrap();
        assert_eq!(cat.list_all().await.len(), 1);

        std::fs::write(&path, "this is not valid toml = = =").unwrap();

        let count = cat.reload().await.unwrap();
        assert_eq!(count, 1, "should report the prior in-memory count");
        assert_eq!(cat.list_all().await.len(), 1);
        assert_eq!(cat.get("ollama-llama").await.unwrap().id, "ollama-llama");
    }

    /// PR 3: `models_referencing` returns every entry whose
    /// `credential_id` matches, in catalog order.
    #[tokio::test]
    async fn models_referencing_finds_matching_entries() {
        let (_dir, cat) = temp_catalog().await;
        let tmpl = templates::find_template("anthropic").unwrap();
        let mut a = ModelConfig::from_template(tmpl, "anthropic-sonnet", "claude-sonnet-4-5");
        a.credential_id = Some("cred-1".into());
        let mut b = ModelConfig::from_template(tmpl, "anthropic-haiku", "claude-3-5-haiku-latest");
        b.credential_id = Some("cred-1".into());
        let mut c = ModelConfig::from_template(tmpl, "anthropic-opus", "claude-3-opus");
        c.credential_id = Some("cred-2".into());
        cat.upsert(a).await.unwrap();
        cat.upsert(b).await.unwrap();
        cat.upsert(c).await.unwrap();

        let refs = cat.models_referencing("cred-1").await;
        assert_eq!(refs.len(), 2);
        let ids: Vec<_> = refs.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"anthropic-sonnet"));
        assert!(ids.contains(&"anthropic-haiku"));

        let none = cat.models_referencing("cred-nonexistent").await;
        assert!(none.is_empty());
    }

    /// PR 3: `rewire_credential` flips every matching `credential_id`
    /// from `from` to `to`, persists the change, and reports the count.
    /// Entries that already point at `to` (or have no credential at all)
    /// are left alone.
    #[tokio::test]
    async fn rewire_credential_swaps_and_persists() {
        let (dir, cat) = temp_catalog().await;
        let tmpl = templates::find_template("anthropic").unwrap();
        let mut a = ModelConfig::from_template(tmpl, "anthropic-sonnet", "claude-sonnet-4-5");
        a.credential_id = Some("old-id".into());
        let mut b = ModelConfig::from_template(tmpl, "anthropic-haiku", "claude-3-5-haiku-latest");
        b.credential_id = Some("old-id".into());
        let mut c = ModelConfig::from_template(tmpl, "anthropic-opus", "claude-3-opus");
        c.credential_id = Some("other".into());
        cat.upsert(a).await.unwrap();
        cat.upsert(b).await.unwrap();
        cat.upsert(c).await.unwrap();

        let changed = cat.rewire_credential("old-id", "new-id").await.unwrap();
        assert_eq!(changed, 2);

        let refs = cat.models_referencing("new-id").await;
        assert_eq!(refs.len(), 2);
        let unaffected = cat.models_referencing("other").await;
        assert_eq!(unaffected.len(), 1);
        assert_eq!(unaffected[0].id, "anthropic-opus");

        // Reload from disk and verify the rewire was persisted.
        let reloaded = ModelCatalog::load_or_init(dir.path().join("models.toml"))
            .await
            .unwrap();
        let after = reloaded.models_referencing("new-id").await;
        assert_eq!(after.len(), 2);
    }

    /// PR 3: `rewire_credential` against an id with no dependents
    /// returns `0` and does not touch the on-disk file.
    #[tokio::test]
    async fn rewire_credential_is_noop_when_nothing_references() {
        let (_dir, cat) = temp_catalog().await;
        let changed = cat.rewire_credential("missing", "new").await.unwrap();
        assert_eq!(changed, 0);
        assert_eq!(cat.list_all().await.len(), 0);
    }

    // ─── Phase 2 of `feature/multi-model-subagents`: note field ──
    //
    // Round-trip + 500-char cap + serde-default so pre-Phase-2
    // catalog files still load.

    #[tokio::test]
    async fn upsert_with_note_round_trips() {
        let (_dir, cat) = temp_catalog().await;
        let entry = ModelConfig {
            id: "haiku".to_string(),
            display_name: "Haiku".to_string(),
            template_id: None,
            api_format: ApiFormat::AnthropicMessages,
            base_url: "https://api.anthropic.com".to_string(),
            model_id: "claude-haiku-4-5".to_string(),
            context_window: None,
            max_output_tokens: None,
            headers: Default::default(),
            credential_id: None,
            requires_key: true,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            compat: None,
            spec: None,
            note: Some("very cheap, use it for cron".to_string()),
        };
        cat.upsert(entry).await.expect("upsert succeeds");
        let back = cat.get("haiku").await.expect("entry round-trips");
        assert_eq!(back.note.as_deref(), Some("very cheap, use it for cron"));
    }

    #[tokio::test]
    async fn upsert_rejects_note_above_500_chars() {
        let (_dir, cat) = temp_catalog().await;
        let too_long = "x".repeat(NOTE_MAX_CHARS + 1);
        let entry = ModelConfig {
            id: "opus".to_string(),
            display_name: "Opus".to_string(),
            template_id: None,
            api_format: ApiFormat::AnthropicMessages,
            base_url: "https://api.anthropic.com".to_string(),
            model_id: "claude-opus-4-8".to_string(),
            context_window: None,
            max_output_tokens: None,
            headers: Default::default(),
            credential_id: None,
            requires_key: true,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            compat: None,
            spec: None,
            note: Some(too_long),
        };
        let err = cat.upsert(entry).await.expect_err("must reject");
        assert!(err.to_string().contains("500"), "expected 500-char message, got: {err}");
    }

    #[tokio::test]
    async fn upsert_accepts_note_at_exactly_500_chars() {
        let (_dir, cat) = temp_catalog().await;
        let at_cap: String = "a".repeat(NOTE_MAX_CHARS);
        let entry = ModelConfig {
            id: "edge".to_string(),
            display_name: "Edge".to_string(),
            template_id: None,
            api_format: ApiFormat::AnthropicMessages,
            base_url: "https://api.anthropic.com".to_string(),
            model_id: "claude-opus-4-8".to_string(),
            context_window: None,
            max_output_tokens: None,
            headers: Default::default(),
            credential_id: None,
            requires_key: true,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            compat: None,
            spec: None,
            note: Some(at_cap.clone()),
        };
        cat.upsert(entry).await.expect("500-char note must succeed");
        let back = cat.get("edge").await.expect("entry round-trips");
        assert_eq!(back.note.as_deref().map(str::len), Some(NOTE_MAX_CHARS));
    }

    #[test]
    fn pre_phase_2_models_toml_loads_with_note_none() {
        // On-disk fixtures written before Phase 2 don't carry the
        // `note` field. `serde(default, skip_serializing_if =
        // "Option::is_none")` means they parse cleanly with
        // `note = None` rather than failing the read.
        let toml = r#"
            version = "4.0"

            [entries.legacy]
            id = "legacy"
            display_name = "Legacy"
            api_format = "anthropic_messages"
            base_url = "https://api.anthropic.com"
            model_id = "claude-3-5-sonnet-latest"
            requires_key = true
            enabled = true
        "#;
        let parsed: ModelCatalogFile = toml::from_str(toml).expect("legacy file parses");
        let entry = parsed.entries.get("legacy").expect("legacy entry present");
        assert!(entry.note.is_none(), "missing field must default to None");
    }
}
