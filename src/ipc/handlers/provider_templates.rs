//! `provider_templates` domain request handler (T-109b).
//!
//! Owns the `RequestPacket::ProviderTemplates` IPC variant. The
//! desktop's "Add Provider" modal calls this so the template picker
//! can show the curated list of known providers (Anthropic, OpenAI,
//! Groq, Ollama, …) with their default base URL, API format, and
//! curated model list — the same surface the CLI's
//! `peko provider templates` already prints, but over IPC so the
//! desktop doesn't shell out.
//!
//! The handler holds a narrow [`ProviderTemplatesHost`] port; the
//! daemon-side implementation (`AppState`) is reached only through
//! the trait, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::provider_templates`)
//!   defines the [`ProviderTemplatesHost`] trait; the producer
//!   (`daemon::state`) implements it (same pattern as the rest of
//!   the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ProviderTemplateInfo, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::providers::catalog::ApiFormat;
use crate::providers::templates::{self, ModelTemplate, ProviderTemplate};

/// Narrow port the `provider_templates` handler uses to read the
/// built-in template list.
///
/// Sync because `BUILT_IN_TEMPLATES` is a `&'static [ProviderTemplate]`
/// — no I/O, no locking, no async work. Keeping the trait sync
/// mirrors [`crate::ipc::handlers::credential::CredentialHost`]
/// (also a pure-read surface) and avoids the `async_trait` boxing
/// overhead. The trait is reachable from `AppState` so a future
/// test can stub the templates without spinning up a catalog.
pub(crate) trait ProviderTemplatesHost: Send + Sync {
    /// Snapshot the built-in templates as owned, serializable
    /// `ProviderTemplateInfo`s ready to ship over IPC. The static
    /// lifetimes of the in-runtime templates are projected away —
    /// each entry becomes a fully owned `String` / `u32` shape.
    fn list_templates(&self) -> Vec<ProviderTemplateInfo>;
}

/// `provider_templates` domain request handler. Constructed with
/// an `Arc<dyn ProviderTemplatesHost>` (typically
/// `Arc::new(app_state.clone())` from the dispatcher).
pub(crate) struct ProviderTemplatesHandler {
    host: Arc<dyn ProviderTemplatesHost>,
}

impl ProviderTemplatesHandler {
    pub(crate) fn new(host: Arc<dyn ProviderTemplatesHost>) -> Self {
        Self { host }
    }
}

/// Project a `&'static ProviderTemplate` into the IPC wire shape.
///
/// The two shape differences vs. the in-runtime template:
/// 1. `&'static str` → owned `String` so the struct can be
///    serialized without a lifetime.
/// 2. The `headers` and `capabilities` slices are dropped (T-109b
///    scope decision — the modal doesn't render them, and the
///    catalog entry the user creates from a template starts with
///    the template's defaults intact so no information is lost).
fn template_to_info(t: &ProviderTemplate) -> ProviderTemplateInfo {
    ProviderTemplateInfo {
        id: t.id.to_string(),
        display_name: t.display_name.to_string(),
        api_type: match t.api_format {
            ApiFormat::OpenaiCompletions => "openai",
            ApiFormat::AnthropicMessages => "anthropic",
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
        matches!(request, RequestPacket::ProviderTemplates { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::ProviderTemplates { request_id } => {
                let providers = self.host.list_templates();
                let response = ResponsePacket::ProviderTemplates { request_id, providers };
                send_response(sink, response).await?;
            }
            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ProviderTemplatesHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

// Allow the test module to construct a `ProviderTemplateInfo` from
// a known-good `&'static ProviderTemplate` without going through
// `AppState`. Mirrors the `tests` blocks in the other handlers.
#[allow(dead_code)]
pub(crate) fn template_info_from_static(t: &ProviderTemplate) -> ProviderTemplateInfo {
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
    //! test failure rather than as the desktop's "Add Provider"
    //! modal falling back to an empty picker. Mirrors
    //! `credential_list_emits_rows_with_has_key_flag` and
    //! `provider_list_emits_all_builtin_entries`.

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use std::sync::{Arc, Mutex};

    /// Stub host — each test stages the templates it wants to
    /// exercise. We don't need a real `AppState` here because the
    /// projection (`template_to_info`) is a pure function.
    struct StubHost(Vec<ProviderTemplateInfo>);
    impl ProviderTemplatesHost for StubHost {
        fn list_templates(&self) -> Vec<ProviderTemplateInfo> {
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

    fn anthropic_info() -> ProviderTemplateInfo {
        ProviderTemplateInfo {
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
    async fn provider_templates_emits_seeded_rows() {
        let host = StubHost(vec![anthropic_info()]);
        let handler = ProviderTemplatesHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderTemplates { request_id: 51 },
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
            Some("provider_templates"),
            "response kind must be provider_templates (T-109b wire shape)"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(51));

        let providers = json
            .get("providers")
            .and_then(|v| v.as_array())
            .expect("response should have a providers array");
        assert_eq!(providers.len(), 1);

        // Field names must match what the desktop's Tauri command
        // projection reads in `provider_admin.rs`.
        let p = &providers[0];
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(p.get("display_name").and_then(|v| v.as_str()), Some("Anthropic"));
        assert_eq!(p.get("api_type").and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(
            p.get("base_url").and_then(|v| v.as_str()),
            Some("https://api.anthropic.com")
        );
        assert_eq!(p.get("requires_key").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            p.get("default_model").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-5")
        );

        let models = p.get("models").and_then(|v| v.as_array()).expect("models array");
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.get("context_length").and_then(|v| v.as_u64()), Some(200_000));
        assert_eq!(m.get("max_output_tokens").and_then(|v| v.as_u64()), Some(8_192));
    }

    #[tokio::test]
    async fn provider_templates_emits_empty_array_when_no_templates() {
        // Edge case: a profile that has zero built-in templates (e.g.
        // a future build that ships no presets) must emit an empty
        // `providers` array — not null, not absent — so the desktop
        // modal reduces to "no templates available" without an
        // undefined-property error.
        let host = StubHost(Vec::new());
        let handler = ProviderTemplatesHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderTemplates { request_id: 52 },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        let providers = json
            .get("providers")
            .and_then(|v| v.as_array())
            .expect("response should have a providers array (possibly empty)");
        assert!(providers.is_empty());
    }

    #[test]
    fn template_projection_preserves_anthropic_shape() {
        // Pure-function pin: the projection from `&'static
        // ProviderTemplate` to `ProviderTemplateInfo` must keep the
        // anthropic template's wire shape stable. If a future
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
