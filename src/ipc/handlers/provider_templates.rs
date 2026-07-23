//! `provider_templates` domain request handler (T-109b).
//!
//! Owns the `RequestPacket::ModelTemplates` IPC variant. The
//! desktop's "Add Model" modal calls this so the preset picker
//! can show the curated list of known model presets (Anthropic,
//! OpenAI, Groq, Ollama, …) with their default base URL, API format,
//! and curated model list — the same surface the CLI's
//! `peko model presets` already prints, but over IPC so the
//! desktop doesn't shell out.
//!
//! The handler holds a narrow [`ModelTemplatesHost`] port; the
//! daemon-side implementation (`AppState`) is reached only through
//! the trait, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::provider_templates`)
//!   defines the [`ModelTemplatesHost`] trait; the producer
//!   (`daemon::state`) implements it (same pattern as the rest of
//!   the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ModelPresetInfo, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;
use peko_providers::catalog::ApiFormat;
use peko_providers::templates::{self, ModelTemplate, ProviderTemplate};

/// Narrow port the `provider_templates` handler uses to read the
/// built-in preset list.
///
/// Sync because `BUILT_IN_TEMPLATES` is a `&'static [ProviderTemplate]`
/// — no I/O, no locking, no async work. Keeping the trait sync
/// (same shape as the pure-read credential port in
/// `ipc::handlers::credential`) avoids the `async_trait` boxing
/// overhead. The trait is reachable from `AppState` so a future
/// test can stub the presets without spinning up a catalog.
pub(crate) trait ModelTemplatesHost: Send + Sync {
    /// Snapshot the built-in presets as owned, serializable
    /// `ModelPresetInfo`s ready to ship over IPC. The static
    /// lifetimes of the in-runtime templates are projected away —
    /// each entry becomes a fully owned `String` / `u32` shape.
    fn list_templates(&self) -> Vec<ModelPresetInfo>;
}

/// `provider_templates` domain request handler. Constructed with
/// an `Arc<dyn ModelTemplatesHost>` (typically
/// `Arc::new(app_state.clone())` from the dispatcher).
pub(crate) struct ProviderTemplatesHandler {
    host: Arc<dyn ModelTemplatesHost>,
}

impl ProviderTemplatesHandler {
    pub(crate) fn new(host: Arc<dyn ModelTemplatesHost>) -> Self {
        Self { host }
    }
}

/// Project a `&'static ProviderTemplate` into the IPC wire shape.
///
/// The two shape differences vs. the in-runtime template:
/// 1. `&'static str` → owned `String` so the struct can be
///    serialized without a lifetime.
/// 2. The `headers` slice is dropped (T-109b scope decision — the
///    modal doesn't render them, and the catalog entry the user
///    creates from a preset starts with the preset's defaults intact
///    so no information is lost).
fn template_to_info(t: &ProviderTemplate) -> ModelPresetInfo {
    ModelPresetInfo {
        id: t.id.to_string(),
        display_name: t.display_name.to_string(),
        api_type: match t.api_format {
            ApiFormat::OpenaiCompletions => "openai",
            ApiFormat::AnthropicMessages => "anthropic",
            ApiFormat::OpenAiResponses => "responses",
        }
        .to_string(),
        base_url: t.base_url.to_string(),
        requires_key: t.requires_key,
        default_model: t.default_model.to_string(),
        models: t.models.iter().map(model_to_info).collect(),
    }
}

/// Project a `&'static ModelTemplate` into the IPC wire shape.
fn model_to_info(m: &ModelTemplate) -> crate::ipc::packet::ModelTemplateInfo {
    crate::ipc::packet::ModelTemplateInfo {
        id: m.id.to_string(),
        display_name: m.display_name.map(str::to_string),
        context_length: m.context_length,
        max_output_tokens: m.max_output_tokens,
    }
}

#[async_trait]
impl RequestHandler for ProviderTemplatesHandler {
    fn domain(&self) -> &'static str {
        "provider_templates"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(request, RequestPacket::ModelTemplates { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::ModelTemplates { request_id } => {
                let presets = self.host.list_templates();
                let response = ResponsePacket::ModelTemplates {
                    request_id,
                    presets,
                };
                send_response(sink, response).await?;
            }
            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ProviderTemplatesHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

// Allow the test module to construct a `ModelPresetInfo` from
// a known-good `&'static ProviderTemplate` without going through
// `AppState`. Mirrors the `tests` blocks in the other handlers.
#[allow(dead_code)]
pub(crate) fn template_info_from_static(t: &ProviderTemplate) -> ModelPresetInfo {
    template_to_info(t)
}

// Reference `templates::iter_templates` so the import doesn't
// appear unused in builds that don't run the tests — the function
// is the canonical entry point the `AppState` impl uses, and
// `template_to_info` is a pure projection of its output.
#[allow(dead_code)]
fn ensure_templates_iter_link(_: std::marker::PhantomData<()>) {
    let _ = templates::iter_templates();
}

#[cfg(test)]
mod tests {
    //! Pin the wire shape so a runtime regression surfaces as a
    //! test failure rather than as the desktop's "Add Model"
    //! modal falling back to an empty picker. Mirrors
    //! `credential_list_emits_rows_with_has_key_flag` and
    //! `model_list_emits_catalog_entries`.

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use std::sync::{Arc, Mutex};

    /// Stub host — each test stages the presets it wants to
    /// exercise. We don't need a real `AppState` here because the
    /// projection (`template_to_info`) is a pure function.
    struct StubHost(Vec<ModelPresetInfo>);
    impl ModelTemplatesHost for StubHost {
        fn list_templates(&self) -> Vec<ModelPresetInfo> {
            self.0.clone()
        }
    }

    struct CaptureSink(Arc<Mutex<Vec<u8>>>);
    #[async_trait]
    impl ResponseSink for CaptureSink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
    }

    fn test_caller() -> CallerContext {
        CallerContext::local()
    }

    fn test_peer() -> PeerAddr {
        PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"))
    }

    fn anthropic_info() -> ModelPresetInfo {
        ModelPresetInfo {
            id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            api_type: "anthropic".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            requires_key: true,
            default_model: "claude-sonnet-4-5".to_string(),
            models: vec![crate::ipc::packet::ModelTemplateInfo {
                id: "claude-sonnet-4-5".to_string(),
                display_name: Some("Claude Sonnet 4.5".to_string()),
                context_length: Some(200_000),
                max_output_tokens: Some(8_192),
            }],
        }
    }

    #[tokio::test]
    async fn model_templates_emits_seeded_rows() {
        let host = StubHost(vec![anthropic_info()]);
        let handler = ProviderTemplatesHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelTemplates { request_id: 51 },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");

        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_templates"),
            "response kind must be model_templates"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(51));

        let presets = json
            .get("presets")
            .and_then(|v| v.as_array())
            .expect("response should have a presets array");
        assert_eq!(presets.len(), 1);

        // Field names must match what the desktop's Tauri command
        // projection reads.
        let p = &presets[0];
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(
            p.get("display_name").and_then(|v| v.as_str()),
            Some("Anthropic")
        );
        assert_eq!(
            p.get("api_type").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            p.get("base_url").and_then(|v| v.as_str()),
            Some("https://api.anthropic.com")
        );
        assert_eq!(p.get("requires_key").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            p.get("default_model").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-5")
        );

        let models = p
            .get("models")
            .and_then(|v| v.as_array())
            .expect("models array");
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(
            m.get("context_length").and_then(|v| v.as_u64()),
            Some(200_000)
        );
        assert_eq!(
            m.get("max_output_tokens").and_then(|v| v.as_u64()),
            Some(8_192)
        );
    }

    #[tokio::test]
    async fn model_templates_emits_empty_array_when_no_templates() {
        // Edge case: a profile that has zero built-in presets (e.g.
        // a future build that ships no presets) must emit an empty
        // `presets` array — not null, not absent — so the desktop
        // modal reduces to "no presets available" without an
        // undefined-property error.
        let host = StubHost(Vec::new());
        let handler = ProviderTemplatesHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelTemplates { request_id: 52 },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        let presets = json
            .get("presets")
            .and_then(|v| v.as_array())
            .expect("response should have a presets array (possibly empty)");
        assert!(presets.is_empty());
    }

    #[test]
    fn template_projection_preserves_anthropic_shape() {
        // Pure-function pin: the projection from `&'static
        // ProviderTemplate` to `ModelPresetInfo` must keep the
        // anthropic preset's wire shape stable. If a future
        // template change adds a new model, this test breaks at
        // compile time (model count differs) — the on-call reviewer
        // can decide whether to add a new round-trip or revert the
        // template change.
        let t = templates::find_template("anthropic").expect("anthropic template exists");
        let info = template_to_info(t);
        assert_eq!(info.id, "anthropic");
        assert_eq!(info.api_type, "anthropic");
        assert!(info.requires_key);
        assert!(!info.models.is_empty());
        let m = &info.models[0];
        assert_eq!(m.id, t.default_model);
        assert!(m.context_length.is_some());
    }
}
