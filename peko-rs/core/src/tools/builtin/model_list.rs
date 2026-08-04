//! `model_list` builtin — discover what models are configured in the
//! principal's catalog before picking which one to spawn a child
//! agent against.
//!
//! Phase 2 of `feature/multi-model-subagents`. Sister of the F35
//! `ToolSearchTool` (`tools/builtin/tool_search.rs`): same shape —
//! per-agent registration via `Weak<ModelCatalog>`, `ToolExposure::Direct`,
//! `parallelizable() == true`, schema-driven `execute()`.
//!
//! ## Why a tool
//!
//! A parent agent that wants to pick a cheap model for cron or a
//! strong model for analysis needs an in-band discovery path that
//! doesn't require poking at `peko model list` from a shell. The
//! standardized `ModelSpec` (PR 1 of `feature/model-first-config`)
//! captures capability flags, but subjective quality ("very
//! capable, use it for coding") and routing intent ("use it for
//! cron") live in the new `note` field on `ModelConfig` (also Phase
//! 2). `model_list` surfaces both.
//!
//! ## Filter args
//!
//! - `filter`: `"vision" | "tools" | "thinking" | "priced" | "json_mode"` —
//!   thin wrapper over the existing `SearchArgs` predicates at
//!   `cli/src/commands/model.rs:117-161`. Selecting "vision" returns
//!   only entries whose `spec.image_input` is true; "priced" returns
//!   only entries with a `PricingHint`. Multiple filters combine
//!   with AND (intersect). `None` ⇒ no capability filter.
//! - `contains`: substring match against `id`, `display_name`, AND
//!   `note`. Case-sensitive on `id` (catalog ids are
//!   lowercase-canonic); case-insensitive on the other two. Matches
//!   the existing `peko model search` predicate so the LLM-side
//!   query and the CLI-side query agree.
//!
//! ## Output shape
//!
//! `{ "count": N, "entries": [ModelSummary, …] }` — exactly the wire
//! shape `peko model list --json` produces. Parent agents can rely
//! on the same projection as the desktop gallery / CLI.

use std::sync::Weak;

use async_trait::async_trait;
use serde_json::{json, Value};

use peko_providers::catalog::ModelCatalog;
use peko_tools_core::{Tool, ToolError};

use crate::extensions::framework::types::ToolExposure;

/// Synthetic tool name surfaced to the LLM. Single source of truth
/// so registration sites (root) and tests don't drift.
pub const MODEL_LIST_TOOL_NAME: &str = "model_list";

/// `model_list` builtin — returns the principal's catalog of
/// configured models, optionally filtered.
///
/// Holds a `Weak<ModelCatalog>` rather than `Arc` so the tool does
/// not extend the catalog's lifetime past the daemon itself.
pub struct ModelListTool {
    catalog: Weak<ModelCatalog>,
}

impl ModelListTool {
    /// Construct with a weak handle to the principal's catalog.
    #[must_use]
    pub fn new(catalog: Weak<ModelCatalog>) -> Self {
        Self { catalog }
    }
}

impl std::fmt::Debug for ModelListTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelListTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for ModelListTool {
    fn name(&self) -> &'static str {
        MODEL_LIST_TOOL_NAME
    }

    fn description(&self) -> String {
        // `ToolMetadata::new` clones the String when registering the
        // tool, so we allocate once per registration here and not per
        // call. Keep this copy close to `ToolSearchTool`'s sister
        // helper so the two builtin stubs advertise the same shape.
        "List the models configured in this principal's catalog. \
         Optional `filter` (vision|tools|thinking|priced|json_mode) \
         narrows by capability. Optional `contains <NEEDLE>` \
         substring-matches against id, display_name, and note. \
         Each entry exposes `id`, `display_name`, `spec`, `pricing`, \
         `note`, and other capability flags so the parent can \
         choose which model to spawn a subagent against."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "string",
                    "enum": ["vision", "tools", "thinking", "priced", "json_mode"],
                    "description": "Optional capability filter. AND-combined with `contains`."
                },
                "contains": {
                    "type": "string",
                    "description": "Optional substring matched (case-insensitive) against id, display_name, and note."
                }
            },
            "additionalProperties": false
        })
    }

    fn exposure(&self) -> ToolExposure {
        // Mirror ToolSearchTool — Direct, never Deferred/Hidden.
        // Discovery is the entire point.
        ToolExposure::Direct
    }

    fn parallelizable(&self) -> bool {
        // Read-only snapshot of the catalog. Safe to run alongside
        // itself or other tools.
        true
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        // ── 1. Parse optional arguments ─────────────────────────────────
        let filter = params
            .get("filter")
            .and_then(Value::as_str)
            .map(str::to_string);
        let contains = params
            .get("contains")
            .and_then(Value::as_str)
            .map(str::to_string);

        // `filter` is constrained by the JSON Schema, but `additionalProperties`
        // is `false` so unknown keys fail validation upstream. We still
        // do a defensive bounds check here in case the validator is
        // bypassed in tests.
        if let Some(ref f) = filter {
            if !matches!(
                f.as_str(),
                "vision" | "tools" | "thinking" | "priced" | "json_mode"
            ) {
                return Err(ToolError::Other(format!(
                    "unknown filter '{f}'; expected one of vision|tools|thinking|priced|json_mode"
                ))
                .into());
            }
        }

        // ── 2. Upgrade the Weak ref; bail if the catalog is gone ────────
        let catalog = self.catalog.upgrade().ok_or_else(|| {
            anyhow::anyhow!("ModelCatalog has been dropped; model_list cannot run")
        })?;

        // ── 3. Snapshot the catalog and apply filters ──────────────────
        let entries = catalog.list_all().await;
        let mut matched: Vec<&peko_providers::catalog::ModelConfig> = entries
            .iter()
            .filter(|e| filter_matches(e, filter.as_deref()))
            .filter(|e| contains_matches(e, contains.as_deref()))
            .collect();

        // Stable order: id ASC. Same ordering as `peko model list`.
        matched.sort_by(|a, b| a.id.cmp(&b.id));

        // ── 4. Project to the canonical ModelSummary wire shape ─────────
        let summaries: Vec<Value> = matched
            .iter()
            .map(|e| model_summary_to_json(e))
            .collect();

        Ok(json!({
            "count": summaries.len(),
            "entries": summaries,
        }))
    }
}

/// Apply the `filter` predicate. `None` means no capability filter.
fn filter_matches(e: &peko_providers::catalog::ModelConfig, filter: Option<&str>) -> bool {
    let Some(f) = filter else {
        return true;
    };
    match f {
        "vision" => e.spec.as_ref().is_some_and(|s| s.image_input),
        "tools" => e.spec.as_ref().is_some_and(|s| {
            use peko_providers::spec::ToolSupport;
            !matches!(s.tool_support, ToolSupport::None)
        }),
        "thinking" => e.spec.as_ref().is_some_and(|s| {
            use peko_providers::spec::ThinkingMode;
            !matches!(s.thinking, ThinkingMode::Disabled)
        }),
        "priced" => e
            .spec
            .as_ref()
            .and_then(|s| s.pricing.as_ref())
            .is_some_and(|p| p.input_per_million.is_some() || p.output_per_million.is_some()),
        "json_mode" => e.spec.as_ref().is_some_and(|s| s.json_mode),
        // Unknown filter values are rejected in `execute`; reaching
        // here with an unknown value is a programmer error.
        _ => true,
    }
}

/// Apply the `contains` needle predicate. Matches against
/// `id` (case-sensitive — catalog ids are lowercase), `display_name`,
/// and `note` (both case-insensitive). Empty needle = no-op.
fn contains_matches(e: &peko_providers::catalog::ModelConfig, needle: Option<&str>) -> bool {
    let Some(n) = needle else {
        return true;
    };
    if n.is_empty() {
        return true;
    }
    if e.id.contains(n) {
        return true;
    }
    if e.display_name.to_lowercase().contains(&n.to_lowercase()) {
        return true;
    }
    if e.note
        .as_deref()
        .is_some_and(|note| note.to_lowercase().contains(&n.to_lowercase()))
    {
        return true;
    }
    false
}

/// Project a `ModelConfig` into the JSON shape the parent agent
/// sees. Mirrors the canonical `model_summary_from_config` projection
/// at `core/src/daemon/state.rs:2279` so the tool's output and the
/// IPC `model.list` response are byte-for-byte equivalent.
fn model_summary_to_json(e: &peko_providers::catalog::ModelConfig) -> Value {
    use peko_providers::catalog::ApiFormat;

    let api_format = match e.api_format {
        ApiFormat::OpenaiCompletions => "openai",
        ApiFormat::AnthropicMessages => "anthropic",
        ApiFormat::OpenAiResponses => "responses",
    };

    let spec = e.spec.as_ref().map(spec_to_json);

    json!({
        "id": e.id,
        "display_name": e.display_name,
        "template_id": e.template_id,
        "api_format": api_format,
        "base_url": e.base_url,
        "model_id": e.model_id,
        "context_window": e.context_window,
        "max_output_tokens": e.max_output_tokens,
        "headers": e.headers,
        "credential_id": e.credential_id,
        "requires_key": e.requires_key,
        "is_local": !e.requires_key,
        "enabled": e.enabled,
        "spec": spec,
        "note": e.note,
    })
}

fn spec_to_json(s: &peko_providers::spec::ModelSpec) -> Value {
    use peko_providers::spec::{ThinkingMode, ToolSupport};
    json!({
        "image_input": s.image_input,
        "audio_input": s.audio_input,
        "tool_support": match s.tool_support {
            ToolSupport::None => "none",
            ToolSupport::FunctionCalling => "function_calling",
            ToolSupport::Full => "full",
        },
        "streaming": s.streaming,
        "thinking": match s.thinking {
            ThinkingMode::Disabled => "disabled",
            ThinkingMode::Optional => "optional",
            ThinkingMode::Required => "required",
            ThinkingMode::CustomBudget => "custom_budget",
        },
        "json_mode": s.json_mode,
        "pricing": s.pricing.as_ref().map(|p| json!({
            "input_per_million": p.input_per_million,
            "output_per_million": p.output_per_million,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_providers::catalog::{ApiFormat, ModelCatalog, ModelCatalogFile, ModelConfig};
    use peko_providers::spec::{
        ModelSpec, PricingHint, ThinkingMode, ToolSupport,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// Build a stub catalog with one entry whose fields we control.
    async fn catalog_with_entry(e: ModelConfig) -> Arc<ModelCatalog> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.toml");
        let mut entries = BTreeMap::new();
        entries.insert(e.id.clone(), e);
        let file = ModelCatalogFile {
            version: "1".to_string(),
            entries,
        };
        std::fs::write(
            &path,
            toml::to_string(&file).expect("serialize catalog"),
        )
        .expect("write catalog");
        ModelCatalog::load_or_init(&path)
            .await
            .expect("load catalog")
    }

    fn entry(
        id: &str,
        display_name: &str,
        spec: Option<ModelSpec>,
        note: Option<&str>,
    ) -> ModelConfig {
        let mut headers = BTreeMap::new();
        headers.insert("X-Stub".to_string(), "true".to_string());
        ModelConfig {
            id: id.to_string(),
            display_name: display_name.to_string(),
            template_id: None,
            api_format: ApiFormat::AnthropicMessages,
            base_url: "https://example.test".to_string(),
            model_id: format!("wire-{id}"),
            context_window: Some(100_000),
            max_output_tokens: Some(8_000),
            headers,
            credential_id: None,
            requires_key: true,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            compat: None,
            spec,
            note: note.map(str::to_string),
        }
    }

    fn vision_spec() -> ModelSpec {
        ModelSpec {
            image_input: true,
            audio_input: false,
            tool_support: ToolSupport::FunctionCalling,
            streaming: true,
            thinking: ThinkingMode::Optional,
            json_mode: true,
            pricing: Some(PricingHint {
                input_per_million: Some(3.0),
                output_per_million: Some(15.0),
            }),
        }
    }

    fn text_only_spec() -> ModelSpec {
        ModelSpec {
            image_input: false,
            audio_input: false,
            tool_support: ToolSupport::None,
            streaming: true,
            thinking: ThinkingMode::Disabled,
            json_mode: false,
            pricing: None,
        }
    }

    #[tokio::test]
    async fn model_list_returns_all_when_no_filter() {
        let cat = catalog_with_entry(entry("alpha", "Alpha", Some(text_only_spec()), None)).await;
        let tool = ModelListTool::new(Arc::downgrade(&cat));
        let out = tool.execute(json!({})).await.expect("execute");
        assert_eq!(out["count"], 1);
        assert_eq!(out["entries"][0]["id"], "alpha");
    }

    #[tokio::test]
    async fn model_list_vision_filter_drops_text_only() {
        let cat = catalog_with_entry(entry("vision", "Vision", Some(vision_spec()), None)).await;
        let tool = ModelListTool::new(Arc::downgrade(&cat));

        let out = tool
            .execute(json!({ "filter": "vision" }))
            .await
            .expect("execute");
        assert_eq!(out["count"], 1);

        // Now make a second catalog with text-only entry and verify it's filtered out.
        let cat2 = catalog_with_entry(entry("plain", "Plain", Some(text_only_spec()), None)).await;
        let tool2 = ModelListTool::new(Arc::downgrade(&cat2));
        let out2 = tool2
            .execute(json!({ "filter": "vision" }))
            .await
            .expect("execute");
        assert_eq!(out2["count"], 0);
    }

    #[tokio::test]
    async fn model_list_priced_filter_requires_pricing_hint() {
        let priced = entry("priced", "Priced", Some(vision_spec()), None);
        let no_price_spec = ModelSpec {
            pricing: None,
            ..vision_spec()
        };
        let unpriced = entry("unpriced", "Unpriced", Some(no_price_spec), None);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.toml");
        let mut entries = BTreeMap::new();
        entries.insert(priced.id.clone(), priced);
        entries.insert(unpriced.id.clone(), unpriced);
        std::fs::write(
            &path,
            toml::to_string(&ModelCatalogFile {
                version: "1".to_string(),
                entries,
            })
            .expect("serialize"),
        )
        .expect("write");
        let cat = ModelCatalog::load_or_init(&path).await.expect("load");

        let tool = ModelListTool::new(Arc::downgrade(&cat));
        let out = tool
            .execute(json!({ "filter": "priced" }))
            .await
            .expect("execute");
        assert_eq!(out["count"], 1);
        assert_eq!(out["entries"][0]["id"], "priced");
    }

    #[tokio::test]
    async fn model_list_contains_matches_note() {
        let cat = catalog_with_entry(entry(
            "haiku",
            "Haiku",
            Some(text_only_spec()),
            Some("very cheap, use it for cron"),
        ))
        .await;

        let tool = ModelListTool::new(Arc::downgrade(&cat));
        let out = tool
            .execute(json!({ "contains": "cron" }))
            .await
            .expect("execute");
        assert_eq!(out["count"], 1, "note substring must match");

        let out2 = tool
            .execute(json!({ "contains": "CRON" }))
            .await
            .expect("execute");
        assert_eq!(out2["count"], 1, "note match must be case-insensitive");
    }

    #[tokio::test]
    async fn model_list_contains_matches_id_and_display_name() {
        let cat = catalog_with_entry(entry("haiku", "Haiku 4.5", Some(text_only_spec()), None))
            .await;

        let tool = ModelListTool::new(Arc::downgrade(&cat));
        let out = tool
            .execute(json!({ "contains": "haiku" }))
            .await
            .expect("execute");
        assert_eq!(out["count"], 1, "id substring must match");

        let out2 = tool
            .execute(json!({ "contains": "4.5" }))
            .await
            .expect("execute");
        assert_eq!(out2["count"], 1, "display_name substring must match");
    }

    #[tokio::test]
    async fn model_list_empty_needle_returns_all() {
        let cat = catalog_with_entry(entry("alpha", "Alpha", Some(text_only_spec()), None)).await;
        let tool = ModelListTool::new(Arc::downgrade(&cat));
        let out = tool
            .execute(json!({ "contains": "" }))
            .await
            .expect("execute");
        assert_eq!(out["count"], 1);
    }

    #[tokio::test]
    async fn model_list_rejects_unknown_filter() {
        let cat = catalog_with_entry(entry("alpha", "Alpha", Some(text_only_spec()), None)).await;
        let tool = ModelListTool::new(Arc::downgrade(&cat));
        let result = tool.execute(json!({ "filter": "magic" })).await;
        assert!(result.is_err(), "unknown filter must error");
    }

    #[tokio::test]
    async fn model_list_drops_catalog_when_weak_drops() {
        let cat = catalog_with_entry(entry("alpha", "Alpha", Some(text_only_spec()), None)).await;
        let tool = ModelListTool::new(Arc::downgrade(&cat));
        drop(cat);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err(), "dropped Weak must error");
    }

    #[tokio::test]
    async fn model_list_metadata_is_direct_and_parallelizable() {
        let cat = catalog_with_entry(entry("alpha", "Alpha", Some(text_only_spec()), None)).await;
        let tool = ModelListTool::new(Arc::downgrade(&cat));
        assert_eq!(tool.name(), MODEL_LIST_TOOL_NAME);
        assert_eq!(tool.exposure(), ToolExposure::Direct);
        assert!(tool.parallelizable());
    }

    #[tokio::test]
    async fn model_list_schema_advertises_filter_and_contains() {
        let cat = catalog_with_entry(entry("alpha", "Alpha", Some(text_only_spec()), None)).await;
        let tool = ModelListTool::new(Arc::downgrade(&cat));
        let schema = tool.parameters();
        let props = schema["properties"].as_object().expect("properties");
        assert!(props.contains_key("filter"));
        assert!(props.contains_key("contains"));
        let filter_enum: Vec<&str> = schema["properties"]["filter"]["enum"]
            .as_array()
            .expect("enum")
            .iter()
            .map(|v| v.as_str().expect("string"))
            .collect();
        for f in ["vision", "tools", "thinking", "priced", "json_mode"] {
            assert!(filter_enum.contains(&f), "missing filter enum value {f}");
        }
        assert_eq!(schema["additionalProperties"], json!(false));
    }
}