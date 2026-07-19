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
//!   (`crate::providers::templates`) describe a known provider with
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

use crate::providers::templates::ProviderTemplate;

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
        let (context_window, max_output_tokens) = template
            .models
            .iter()
            .find(|m| m.id == model_id)
            .map(|m| (m.context_length, m.max_output_tokens))
            .unwrap_or((None, None));
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
    pub async fn upsert(&self, entry: ModelConfig) -> Result<()> {
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
    use crate::providers::templates;
    use tempfile::tempdir;

    fn temp_catalog() -> (tempfile::TempDir, Arc<ModelCatalog>) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.toml");
        let cat = tokio_test::block_on(ModelCatalog::load_or_init(&path)).unwrap();
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
        let (_dir, cat) = temp_catalog();
        let snap = tokio_test::block_on(cat.snapshot());
        assert_eq!(snap.entries.len(), 0);
        assert_eq!(snap.version, "4.0");
    }

    #[test]
    fn upsert_persists_to_disk() {
        let (dir, cat) = temp_catalog();
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
        let (_dir, cat) = temp_catalog();
        let tmpl = templates::find_template("openai").unwrap();
        let entry = ModelConfig::from_template(tmpl, "openai-gpt-4o", "gpt-4o");
        tokio_test::block_on(cat.upsert(entry)).unwrap();
        assert!(tokio_test::block_on(cat.remove("openai-gpt-4o")).unwrap());
        assert!(!tokio_test::block_on(cat.remove("openai-gpt-4o")).unwrap());
    }

    #[test]
    fn context_window_resolves_from_catalog() {
        let (_dir, cat) = temp_catalog();
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
        let (_dir, cat) = temp_catalog();
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
}
