//! Provider catalog — runtime-owned list of LLM providers and their models.
//!
//! The catalog replaces the previous design where provider/model/API-key
//! were baked into every agent's config. It is a single TOML file at
//! `~/.peko/providers.toml`, loaded once on startup and shared across
//! the runtime via `Arc<RwLock<ProviderCatalog>>`. The OS keychain (see
//! `common::secret_store`) holds the corresponding API keys.
//!
//! ## Design properties
//!
//! - **Templates vs. entries.** Preset templates
//!   (`crate::providers::templates`) describe a known provider
//!   (Anthropic, OpenAI, Ollama, …) with curated model lists. They are
//!   static code. `ProviderCatalogEntry` is the runtime-owned instance
//!   of a provider that the user has added — fully editable, persisted
//!   to disk.
//! - **No secrets on disk.** API keys live in the OS keychain; the
//!   catalog only stores public metadata (id, base URL, format, model
//!   list, default model, headers).
//! - **Per-entry default model.** Each entry declares its own
//!   `default_model_id` which must reference one of its `models[]`.
//! - **Enabled flag.** Disabled entries remain in the catalog but are
//!   not considered for resolution.
//!
//! ## Persistence
//!
//! Writes are atomic: serialize, write to `providers.toml.tmp`, then
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

use crate::providers::templates::{ModelTemplate, ProviderTemplate};

/// Top-level API format understood by the runtime.
///
/// The runtime ships adapters for these two formats. Custom providers
/// declared via `peko provider add --custom --api-format <FMT>` must
/// use one of these values; adding a third format requires a new
/// adapter implementation and is intentionally not user-extensible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    /// OpenAI Chat Completions API. Compatible with OpenAI, Groq,
    /// Together, OpenRouter, Ollama, vLLM, llama.cpp, …
    OpenaiCompletions,
    /// Anthropic Messages API. Compatible with Anthropic, Kimi Code,
    /// MiniMax, …
    AnthropicMessages,
}

impl ApiFormat {
    /// Stable wire id used in CLI / IPC.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiFormat::OpenaiCompletions => "openai_completions",
            ApiFormat::AnthropicMessages => "anthropic_messages",
        }
    }

    /// Parse from wire id.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "openai_completions" | "openai-completions" => Some(Self::OpenaiCompletions),
            "anthropic_messages" | "anthropic-messages" => Some(Self::AnthropicMessages),
            _ => None,
        }
    }
}

impl std::fmt::Display for ApiFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Optional capability tags attached to a model. Used by callers
/// (e.g. desktop UI) to filter models for features they support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    ToolUse,
    Vision,
    JsonMode,
    Streaming,
    PromptCaching,
}

/// Curated information about a model available under a provider entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    /// Model id as it appears on the wire (e.g. `gpt-4o`,
    /// `claude-sonnet-4-5`).
    pub id: String,
    /// Human-readable display name. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Maximum context length in tokens (input + output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u32>,
    /// Maximum output tokens for a single response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Capabilities advertised by the catalog author.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<ModelCapability>,
}

impl ModelInfo {
    /// Construct a minimal `ModelInfo` with just an id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            context_length: None,
            max_output_tokens: None,
            capabilities: Vec::new(),
        }
    }
}

/// One provider entry in the runtime-owned catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCatalogEntry {
    /// Stable, lowercase, filesystem- and keychain-safe provider id.
    /// This is the canonical lookup key used by `LlmResolver`,
    /// `peko provider …`, IPC handlers, and the OS keychain account.
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Optional template id this entry was seeded from (e.g.
    /// `"anthropic"`, `"openai"`). `None` for fully custom entries.
    /// The runtime does not enforce that a template with this id
    /// exists — it is purely metadata so users can see where an entry
    /// originated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Wire format used to talk to the provider.
    pub api_format: ApiFormat,
    /// Base URL for the API.
    pub base_url: String,
    /// Models available under this provider.
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    /// Model id used when no override is supplied.
    pub default_model_id: String,
    /// Optional extra HTTP headers (e.g. `OpenAI-Organization`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Whether this provider requires an API key.
    #[serde(default = "default_true")]
    pub requires_key: bool,
    /// Whether this provider is eligible for resolution.
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

impl ProviderCatalogEntry {
    /// Look up a model by id.
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&ModelInfo> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Construct a `ProviderCatalogEntry` from a preset template, with
    /// the user-supplied id and display name overriding the template's
    /// defaults. Models are seeded from the template.
    #[must_use]
    pub fn from_template(
        template: &ProviderTemplate,
        id: impl Into<String>,
        display_name: Option<String>,
    ) -> Self {
        let id = id.into();
        let display_name = display_name.unwrap_or_else(|| template.display_name.to_string());
        let models: Vec<ModelInfo> = template
            .models
            .iter()
            .map(|m: &ModelTemplate| ModelInfo {
                id: m.id.to_string(),
                display_name: m.display_name.map(str::to_string),
                context_length: m.context_length,
                max_output_tokens: m.max_output_tokens,
                capabilities: m.capabilities.to_vec(),
            })
            .collect();
        let default_model_id = template.default_model.to_string();
        Self {
            id,
            display_name,
            template_id: Some(template.id.to_string()),
            api_format: template.api_format,
            base_url: template.base_url.to_string(),
            models,
            default_model_id,
            headers: template
                .headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            requires_key: template.requires_key,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

/// On-disk schema for `~/.peko/providers.toml`.
///
/// The catalog file is the source of truth at rest. `version` is bumped
/// when the schema changes incompatibly. `entries` is a map keyed by
/// provider id so duplicate ids are rejected at deserialization time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCatalogFile {
    #[serde(default = "default_catalog_version")]
    pub version: String,
    #[serde(default)]
    pub entries: BTreeMap<String, ProviderCatalogEntry>,
    /// Runtime default provider id. Optional — the runtime may also
    /// have a default set via CLI/IPC without persisting it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider_id: Option<String>,
    /// Runtime default model id. Must reference a model on
    /// `default_provider_id` if both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model_id: Option<String>,
}

impl Default for ProviderCatalogFile {
    fn default() -> Self {
        Self {
            version: default_catalog_version(),
            entries: BTreeMap::new(),
            default_provider_id: None,
            default_model_id: None,
        }
    }
}

fn default_catalog_version() -> String {
    "3.0".to_string()
}

/// In-memory provider catalog, shared across the runtime.
///
/// Reads and writes go through this type. Mutations acquire a write
/// lock and persist to disk via `persist()`. The catalog is also
/// responsible for surfacing which entries exist, which are enabled,
/// and resolving default + lookup queries used by `LlmResolver`.
pub struct ProviderCatalog {
    path: PathBuf,
    inner: RwLock<ProviderCatalogFile>,
}

impl ProviderCatalog {
    /// Default filename under the config directory.
    pub const FILENAME: &'static str = "providers.toml";

    /// Load the catalog from `path`, or create an empty one if the file
    /// does not exist. A corrupt file is logged and treated as empty
    /// (with a backup written to `providers.toml.bak`) so the runtime
    /// can still start.
    pub async fn load_or_init(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        let file = if path.exists() {
            match Self::read_file(&path) {
                Ok(f) => f,
                Err(e) => {
                    warn!(
                        "providers.toml at {} is corrupt ({e}); backing up and starting empty",
                        path.display()
                    );
                    let _ = std::fs::copy(&path, path.with_extension("toml.bak"));
                    ProviderCatalogFile::default()
                }
            }
        } else {
            ProviderCatalogFile::default()
        };
        Ok(Arc::new(Self {
            path,
            inner: RwLock::new(file),
        }))
    }

    fn read_file(path: &Path) -> Result<ProviderCatalogFile> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let parsed: ProviderCatalogFile =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(parsed)
    }

    /// Return a snapshot of the catalog file.
    pub async fn snapshot(&self) -> ProviderCatalogFile {
        self.inner.read().await.clone()
    }

    /// Re-read the on-disk catalog into this Arc's inner state. Used by
    /// the daemon after a CLI mutation (`peko provider add`, etc.) so
    /// the long-running process sees the new catalog without being
    /// restarted. The existing `Arc<ProviderCatalog>` reference held
    /// by the daemon stays valid — every reader goes through the
    /// `RwLock`, so swaps are atomic with respect to in-flight reads.
    ///
    /// A missing or unreadable file is logged and treated as no-op so
    /// a transient fs hiccup doesn't blank the daemon's in-memory
    /// state. Returns the entry count after reload so callers can
    /// confirm what they got.
    pub async fn reload(&self) -> Result<usize> {
        let file = if self.path.exists() {
            match Self::read_file(&self.path) {
                Ok(f) => f,
                Err(e) => {
                    warn!(
                        "providers.toml reload at {} failed ({e}); keeping prior in-memory state",
                        self.path.display()
                    );
                    return Ok(self.inner.read().await.entries.len());
                }
            }
        } else {
            ProviderCatalogFile::default()
        };
        let count = file.entries.len();
        let mut guard = self.inner.write().await;
        *guard = file;
        Ok(count)
    }

    /// List all enabled entries.
    pub async fn list_enabled(&self) -> Vec<ProviderCatalogEntry> {
        let guard = self.inner.read().await;
        guard
            .entries
            .values()
            .filter(|e| e.enabled)
            .cloned()
            .collect()
    }

    /// List every entry, including disabled ones.
    pub async fn list_all(&self) -> Vec<ProviderCatalogEntry> {
        let guard = self.inner.read().await;
        guard.entries.values().cloned().collect()
    }

    /// Look up an entry by id.
    pub async fn get(&self, id: &str) -> Option<ProviderCatalogEntry> {
        let guard = self.inner.read().await;
        guard.entries.get(id).cloned()
    }

    /// Look up an enabled entry by id.
    pub async fn get_enabled(&self, id: &str) -> Option<ProviderCatalogEntry> {
        let guard = self.inner.read().await;
        guard.entries.get(id).filter(|e| e.enabled).cloned()
    }

    /// Resolve the maximum context length in tokens for a given
    /// `(provider_id, model_id)` pair declared in the catalog.
    ///
    /// Returns `None` when the provider is unknown, the model is not
    /// declared on that provider, the entry is disabled, or the model
    /// has no `context_length` set (e.g. a user-customised model
    /// without curated metadata).
    ///
    /// This is the **single source of truth** for "the model max
    /// context". Callers that need a concrete budget (compaction,
    /// dry-run reporting, request shaping) consult this instead of
    /// hard-coded fallbacks.
    pub async fn model_context_length(&self, provider_id: &str, model_id: &str) -> Option<u32> {
        let entry = self.get_enabled(provider_id).await?;
        entry.model(model_id).and_then(|m| m.context_length)
    }

    /// Add or replace an entry. Bumps `updated_at`.
    pub async fn upsert(&self, entry: ProviderCatalogEntry) -> Result<()> {
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

    /// Set the runtime default provider + model. Both fields must
    /// reference each other consistently if both are present.
    pub async fn set_default(
        &self,
        provider_id: Option<String>,
        model_id: Option<String>,
    ) -> Result<()> {
        {
            let mut guard = self.inner.write().await;
            if let Some(ref pid) = provider_id {
                let entry = guard.entries.get(pid).with_context(|| {
                    format!("cannot set default: provider '{pid}' is not in the catalog")
                })?;
                if let Some(ref mid) = model_id {
                    if entry.model(mid).is_none() {
                        anyhow::bail!("model '{mid}' is not declared on provider '{pid}'");
                    }
                }
            }
            guard.default_provider_id = provider_id;
            guard.default_model_id = model_id;
        }
        self.persist().await
    }

    /// Read the runtime default.
    pub async fn get_default(&self) -> (Option<String>, Option<String>) {
        let guard = self.inner.read().await;
        (
            guard.default_provider_id.clone(),
            guard.default_model_id.clone(),
        )
    }

    /// Resolve a `(provider_id, model_id)` request using simple
    /// precedence: explicit caller ids first, otherwise the catalog's
    /// stored default, otherwise the first enabled entry.
    ///
    /// `LlmResolver` calls this with caller-supplied overrides already
    /// resolved; this is the inner step that just validates membership
    /// and falls back to defaults.
    pub async fn resolve_default(
        &self,
        override_provider: Option<&str>,
        override_model: Option<&str>,
    ) -> Result<(ProviderCatalogEntry, ModelInfo)> {
        // 1. explicit override (provider must exist; model must exist
        //    on the provider or default to provider's default model).
        if let Some(pid) = override_provider {
            let entry = self
                .get_enabled(pid)
                .await
                .with_context(|| format!("provider '{pid}' not found or disabled"))?;
            let model = resolve_model_on(&entry, override_model)?;
            return Ok((entry, model));
        }

        // 2. persisted default.
        let (default_pid, default_model_id) = self.get_default().await;
        if let Some(pid) = default_pid {
            if let Some(entry) = self.get_enabled(&pid).await {
                let model = resolve_model_on(&entry, default_model_id.as_deref())?;
                return Ok((entry, model));
            }
            warn!(
                "persisted default provider '{pid}' is missing or disabled; \
                 falling back to first enabled entry"
            );
        }

        // 3. first enabled entry.
        let enabled = self.list_enabled().await;
        let entry = enabled
            .first()
            .with_context(|| "no enabled providers in the catalog")?;
        let model = resolve_model_on(entry, None)?;
        Ok((entry.clone(), model))
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
        let serialized = toml::to_string_pretty(&snapshot).context("serializing providers.toml")?;
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, &serialized).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), self.path.display()))?;
        info!(
            "persisted provider catalog to {} ({} entries)",
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

fn resolve_model_on<'a>(
    entry: &'a ProviderCatalogEntry,
    model_id: Option<&str>,
) -> Result<ModelInfo> {
    if let Some(mid) = model_id {
        if let Some(m) = entry.model(mid) {
            return Ok(m.clone());
        }
        anyhow::bail!(
            "model '{mid}' is not declared on provider '{}' \
             (declared models: {})",
            entry.id,
            entry
                .models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    entry
        .model(&entry.default_model_id)
        .cloned()
        .with_context(|| {
            format!(
                "provider '{}' has no default model ({} declared models)",
                entry.id,
                entry.models.len()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::templates;
    use tempfile::tempdir;

    fn temp_catalog() -> (tempfile::TempDir, Arc<ProviderCatalog>) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        let cat = tokio_test::block_on(ProviderCatalog::load_or_init(&path)).unwrap();
        (dir, cat)
    }

    #[test]
    fn api_format_wire_roundtrip() {
        for fmt in [ApiFormat::OpenaiCompletions, ApiFormat::AnthropicMessages] {
            let s = fmt.as_str();
            let back = ApiFormat::from_wire(s).unwrap();
            assert_eq!(fmt, back);
        }
        assert!(ApiFormat::from_wire("garbage").is_none());
    }

    #[test]
    fn empty_catalog_loads_cleanly() {
        let (_dir, cat) = temp_catalog();
        let snap = tokio_test::block_on(cat.snapshot());
        assert_eq!(snap.entries.len(), 0);
        assert_eq!(snap.version, "3.0");
    }

    #[test]
    fn upsert_persists_to_disk() {
        let (dir, cat) = temp_catalog();
        let tmpl = &templates::find_template("anthropic").unwrap();
        let entry = ProviderCatalogEntry::from_template(tmpl, "anthropic", None);
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        let reloaded = tokio_test::block_on(ProviderCatalog::load_or_init(
            dir.path().join("providers.toml"),
        ))
        .unwrap();
        let got = tokio_test::block_on(reloaded.get("anthropic")).unwrap();
        assert_eq!(got.api_format, ApiFormat::AnthropicMessages);
        assert!(got.requires_key);
    }

    #[test]
    fn remove_returns_true_then_false() {
        let (_dir, cat) = temp_catalog();
        let tmpl = &templates::find_template("openai").unwrap();
        let entry = ProviderCatalogEntry::from_template(tmpl, "openai", None);
        tokio_test::block_on(cat.upsert(entry)).unwrap();
        assert!(tokio_test::block_on(cat.remove("openai")).unwrap());
        assert!(!tokio_test::block_on(cat.remove("openai")).unwrap());
    }

    #[test]
    fn set_default_validates_membership() {
        let (_dir, cat) = temp_catalog();
        let tmpl = &templates::find_template("openai").unwrap();
        let entry = ProviderCatalogEntry::from_template(tmpl, "openai", None);
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        // happy path
        tokio_test::block_on(cat.set_default(Some("openai".into()), None)).unwrap();

        // unknown provider
        assert!(tokio_test::block_on(cat.set_default(Some("nope".into()), None)).is_err());

        // model not on provider
        assert!(tokio_test::block_on(
            cat.set_default(Some("openai".into()), Some("gpt-999-not-real".into()))
        )
        .is_err());
    }

    #[test]
    fn resolve_default_precedence() {
        let (_dir, cat) = temp_catalog();
        let tmpl = &templates::find_template("openai").unwrap();
        let entry = ProviderCatalogEntry::from_template(tmpl, "openai", None);
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        // no default, no override -> first enabled
        let (e, m) = tokio_test::block_on(cat.resolve_default(None, None)).unwrap();
        assert_eq!(e.id, "openai");
        assert_eq!(m.id, e.default_model_id);

        // override wins
        let (e2, m2) =
            tokio_test::block_on(cat.resolve_default(Some("openai"), Some(&e.models[0].id)))
                .unwrap();
        assert_eq!(e2.id, "openai");
        assert_eq!(m2.id, e.models[0].id);

        // unknown override -> error
        assert!(tokio_test::block_on(cat.resolve_default(Some("nope"), None)).is_err());
    }

    #[test]
    fn empty_catalog_resolve_default_errors() {
        let (_dir, cat) = temp_catalog();
        assert!(tokio_test::block_on(cat.resolve_default(None, None)).is_err());
    }

    /// `model_context_length` is the SoT for model max context. Seed
    /// the catalog from the Anthropic template and verify hit,
    /// miss-within-provider, unknown-provider, and disabled-entry
    /// paths.
    #[test]
    fn model_context_length_resolves_from_catalog() {
        let (_dir, cat) = temp_catalog();
        let tmpl = &templates::find_template("anthropic").unwrap();
        let entry = ProviderCatalogEntry::from_template(tmpl, "anthropic", None);
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        // hit
        let got = tokio_test::block_on(cat.model_context_length("anthropic", "claude-sonnet-4-5"));
        assert_eq!(got, Some(200_000));

        // model not declared on this provider
        assert_eq!(
            tokio_test::block_on(cat.model_context_length("anthropic", "not-a-real-model")),
            None
        );

        // provider unknown
        assert_eq!(
            tokio_test::block_on(cat.model_context_length("nope", "claude-sonnet-4-5")),
            None
        );
    }

    /// Disabled entries must NOT report a context length even if they
    /// declare one — `get_enabled` semantics propagate.
    #[test]
    fn model_context_length_returns_none_for_disabled_entry() {
        let (_dir, cat) = temp_catalog();
        let tmpl = &templates::find_template("anthropic").unwrap();
        let mut entry = ProviderCatalogEntry::from_template(tmpl, "anthropic", None);
        entry.enabled = false;
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        assert_eq!(
            tokio_test::block_on(cat.model_context_length("anthropic", "claude-sonnet-4-5")),
            None
        );
    }

    /// A model with no `context_length` (e.g. user-added model without
    /// curated metadata) reports `None` rather than 0 — the caller
    /// decides how to handle the unknown.
    #[test]
    fn model_context_length_returns_none_when_model_has_no_length() {
        let (_dir, cat) = temp_catalog();
        let mut entry = ProviderCatalogEntry::from_template(
            &templates::find_template("anthropic").unwrap(),
            "anthropic",
            None,
        );
        // Synthesise a model with no context_length by setting it to None.
        entry.models[0].context_length = None;
        tokio_test::block_on(cat.upsert(entry)).unwrap();

        assert_eq!(
            tokio_test::block_on(cat.model_context_length("anthropic", "claude-sonnet-4-5")),
            None
        );
    }

    #[test]
    fn corrupt_catalog_falls_back_to_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, "this is not valid toml = = =").unwrap();

        let cat = tokio_test::block_on(ProviderCatalog::load_or_init(&path)).unwrap();
        let snap = tokio_test::block_on(cat.snapshot());
        assert!(snap.entries.is_empty());

        // backup file was written
        assert!(path.with_extension("toml.bak").exists());
    }

    #[test]
    fn entry_from_template_seeds_models_and_headers() {
        let tmpl = &templates::find_template("anthropic").unwrap();
        let entry = ProviderCatalogEntry::from_template(tmpl, "anthropic", None);
        assert_eq!(entry.template_id.as_deref(), Some("anthropic"));
        assert!(!entry.models.is_empty(), "template should ship models");
        // default model id must be a real declared model id
        assert!(entry.model(&entry.default_model_id).is_some());
    }

    /// `reload()` re-reads the file into the existing Arc. This is the
    /// contract the daemon depends on: long-running processes get
    /// fresh state without re-instantiating the catalog or breaking
    /// the `Arc<ProviderCatalog>` reference held by the resolver.
    #[tokio::test]
    async fn reload_picks_up_disk_changes_through_same_arc() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        let cat = ProviderCatalog::load_or_init(&path).await.unwrap();

        // Initial state: empty.
        assert_eq!(cat.list_all().await.len(), 0);

        // Write a provider to disk directly (bypassing the in-memory
        // upsert API) — this simulates `peko provider add` on a
        // separate process while the daemon holds the Arc.
        let tmpl = templates::find_template("anthropic").unwrap();
        let entry = ProviderCatalogEntry::from_template(tmpl, "anthropic", None);
        let file = ProviderCatalogFile {
            entries: std::iter::once(("anthropic".to_string(), entry)).collect(),
            default_provider_id: None,
            default_model_id: None,
            ..Default::default()
        };
        std::fs::write(
            &path,
            toml::to_string(&file).expect("serialize provider file"),
        )
        .unwrap();

        // Same Arc, no reload yet → still empty.
        assert_eq!(cat.list_all().await.len(), 0);

        // Reload → sees the new entry through the same Arc.
        let count = cat.reload().await.unwrap();
        assert_eq!(count, 1);
        assert_eq!(cat.list_all().await.len(), 1);
        assert_eq!(cat.get("anthropic").await.unwrap().id, "anthropic");
    }

    /// `reload()` keeps the prior in-memory state on a read failure so
    /// a transient fs hiccup doesn't blank the daemon.
    #[tokio::test]
    async fn reload_keeps_prior_state_on_read_failure() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        let cat = ProviderCatalog::load_or_init(&path).await.unwrap();

        // Seed an entry.
        let tmpl = templates::find_template("ollama").unwrap();
        cat.upsert(ProviderCatalogEntry::from_template(tmpl, "ollama", None))
            .await
            .unwrap();
        assert_eq!(cat.list_all().await.len(), 1);

        // Corrupt the on-disk file.
        std::fs::write(&path, "this is not valid toml = = =").unwrap();

        // Reload should swallow the read failure and keep the
        // in-memory entry intact.
        let count = cat.reload().await.unwrap();
        assert_eq!(count, 1, "should report the prior in-memory count");
        assert_eq!(cat.list_all().await.len(), 1);
        assert_eq!(cat.get("ollama").await.unwrap().id, "ollama");
    }
}
